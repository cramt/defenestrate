//! What starts at logon. Explorer runs the Run keys and the Startup folders
//! when it's the shell, and nothing else does, so without it every app that
//! registers itself to start with Windows (Discord, Steam, a tray tool)
//! silently didn't. The shell does it instead, the way explorer does, once
//! per logon session, then runs the config's own `autostart` lines.
//!
//! The rules are here and tested on the build machine; the registry and
//! folders are read in `sys`.

/// Task Manager's Startup tab records a toggle as a binary value under
/// StartupApproved, named after the Run value or the Startup folder's file.
/// Its first byte is even when the entry is enabled (02, 06) and odd when
/// disabled (03, 07). No value means it was never toggled: enabled.
pub fn approved(value: Option<&[u8]>) -> bool {
    value.and_then(|v| v.first()).is_none_or(|b| b & 1 == 0)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Delete {
    /// Before running: the default, so a command that crashes the session
    /// doesn't run again at every logon.
    Before,
    /// Once it has started: a `!` prefix asks for this.
    After,
}

/// When a RunOnce value goes, from its name. `*` (run in Safe Mode too)
/// means nothing here: the shell never runs in Safe Mode.
pub fn run_once(name: &str) -> Delete {
    if name.trim_start_matches('*').starts_with('!') { Delete::After } else { Delete::Before }
}

/// Startup folder entries explorer leaves alone.
pub fn startup_file(name: &str) -> bool {
    !name.eq_ignore_ascii_case("desktop.ini")
}

#[cfg(windows)]
pub use sys::start;

#[cfg(windows)]
mod sys {
    use std::path::PathBuf;

    use windows::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, REG_CREATED_NEW_KEY,
        REG_CREATE_KEY_DISPOSITION, REG_EXPAND_SZ, REG_OPTION_VOLATILE, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_BINARY,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
    };
    use windows::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath};
    use windows::core::{GUID, HSTRING, PCWSTR, PWSTR, w};

    use super::{Delete, approved, run_once, startup_file};
    use crate::{launcher, log, settings, start_line};

    const RUN: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const RUN_32: PCWSTR = w!(r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run");
    const RUN_ONCE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\RunOnce");
    const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";

    /// At most once per logon session: a shell restarted after a crash
    /// would otherwise start everything a second time.
    pub fn start() {
        if !first_in_session() {
            return log("autostart: already ran in this session");
        }
        // Its own thread: an app's registry entries or a slow network share
        // in a Startup folder mustn't hold up the message loop.
        std::thread::spawn(|| {
            // Explorer's order: machine, then user; Run keys, then folders.
            let mut started = Vec::new();
            run_key(HKEY_LOCAL_MACHINE, RUN, "HKLM Run", "Run", &mut started);
            run_key(HKEY_LOCAL_MACHINE, RUN_32, "HKLM Run (32-bit)", "Run32", &mut started);
            run_key(HKEY_CURRENT_USER, RUN, "HKCU Run", "Run", &mut started);
            // HKLM's RunOnce needs an administrator to delete its values,
            // which the shell isn't; installers that use it reboot anyway.
            user_run_once(&mut started);
            folder(&FOLDERID_CommonStartup, HKEY_LOCAL_MACHINE, "common Startup");
            folder(&FOLDERID_Startup, HKEY_CURRENT_USER, "Startup");
            for line in &settings::get().autostart {
                log(&format!("autostart (config): {line}"));
                started.extend(start_line(line).map(|p| (line.clone(), p)));
            }
            report_early_exits(started);
        });
    }

    /// Run key programs that quit right away (SecurityHealthSystray didn't
    /// run on saturn, 2026-11-19, and nothing said why): an exit code beats
    /// guessing. Many of them hand off to a running instance and exit 0,
    /// which is fine, so only the code is logged, not a verdict.
    fn report_early_exits(started: Vec<(String, HANDLE)>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for (line, p) in started {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as u32;
            unsafe {
                if WaitForSingleObject(p, left) == WAIT_OBJECT_0 {
                    let mut code = 0;
                    let _ = GetExitCodeProcess(p, &mut code);
                    log(&format!("autostart: {line:?} exited within 10 s, code {code:#x}"));
                }
                let _ = CloseHandle(p);
            }
        }
    }

    /// A volatile key goes when the user's hive unloads at logoff, so
    /// whether this call created it says whether the session is new.
    fn first_in_session() -> bool {
        let mut key = HKEY::default();
        let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
        let made = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                w!(r"Software\defenestrate\Session"),
                None,
                PCWSTR::null(),
                REG_OPTION_VOLATILE,
                KEY_READ,
                None,
                &mut key,
                Some(&mut disposition),
            )
        };
        if made != ERROR_SUCCESS {
            // Running twice beats never running.
            log(&format!("autostart: the session marker couldn't be made ({made:?}); running anyway"));
            return true;
        }
        unsafe {
            let _ = RegCloseKey(key);
        }
        disposition == REG_CREATED_NEW_KEY
    }

    fn run_key(root: HKEY, path: PCWSTR, what: &str, approval: &str, started: &mut Vec<(String, HANDLE)>) {
        let Some(key) = open(root, path, KEY_QUERY_VALUE) else { return };
        for (name, line) in command_values(key) {
            if !approved(approval_value(root, approval, &name).as_deref()) {
                log(&format!("autostart ({what}): {name} is disabled in Task Manager"));
                continue;
            }
            log(&format!("autostart ({what}): {name}: {line}"));
            started.extend(start_line(&line).map(|p| (line, p)));
        }
        close(key);
    }

    fn user_run_once(started: &mut Vec<(String, HANDLE)>) {
        let Some(key) = open(HKEY_CURRENT_USER, RUN_ONCE, KEY_QUERY_VALUE | KEY_SET_VALUE) else { return };
        for (name, line) in command_values(key) {
            let when = run_once(&name);
            let delete = || {
                let gone = unsafe { RegDeleteValueW(key, &HSTRING::from(name.as_str())) };
                if gone != ERROR_SUCCESS {
                    log(&format!("autostart (HKCU RunOnce): deleting {name} failed ({gone:?})"));
                }
            };
            if when == Delete::Before {
                delete();
            }
            log(&format!("autostart (HKCU RunOnce): {name}: {line}"));
            started.extend(start_line(&line).map(|p| (line.clone(), p)));
            if when == Delete::After {
                delete();
            }
        }
        close(key);
    }

    fn folder(id: &GUID, approvals: HKEY, what: &str) {
        let dir = match unsafe { SHGetKnownFolderPath(id, KNOWN_FOLDER_FLAG(0), None) } {
            Ok(p) => {
                let path = unsafe { p.to_string() };
                unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
                match path {
                    Ok(path) => PathBuf::from(path),
                    Err(_) => return,
                }
            }
            Err(e) => return log(&format!("autostart ({what}): finding the folder failed: {e}")),
        };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let mut files: Vec<_> = entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).collect();
        files.sort();
        // Said even when empty: an absent line read as "never looked".
        let count = files.iter().filter(|f| f.file_name().is_some_and(|n| startup_file(&n.to_string_lossy()))).count();
        log(&format!("autostart ({what}): {count} entries in {}", dir.display()));
        for file in files {
            let Some(name) = file.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            if !startup_file(&name) {
                continue;
            }
            if !approved(approval_value(approvals, "StartupFolder", &name).as_deref()) {
                log(&format!("autostart ({what}): {name} is disabled in Task Manager"));
                continue;
            }
            log(&format!("autostart ({what}): {}", file.display()));
            // Shortcuts mostly, which only ShellExecuteEx reads.
            launcher::open(&file);
        }
    }

    fn approval_value(root: HKEY, list: &str, name: &str) -> Option<Vec<u8>> {
        let key = HSTRING::from(format!(r"{APPROVED}\{list}"));
        let value = HSTRING::from(name);
        let mut data = vec![0u8; 64];
        let mut len = data.len() as u32;
        let got = unsafe { RegGetValueW(root, &key, &value, RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr().cast()), Some(&mut len)) };
        // Missing (never toggled) or not binary: enabled either way.
        if got != ERROR_SUCCESS {
            return None;
        }
        data.truncate(len as usize);
        Some(data)
    }

    fn open(root: HKEY, path: PCWSTR, access: REG_SAM_FLAGS) -> Option<HKEY> {
        let mut key = HKEY::default();
        // A missing key is the common case (no RunOnce), not worth a line.
        (unsafe { RegOpenKeyExW(root, path, None, access, &mut key) } == ERROR_SUCCESS).then_some(key)
    }

    fn close(key: HKEY) {
        unsafe {
            let _ = RegCloseKey(key);
        }
    }

    /// The key's string values, `%VAR%`s expanded, in the registry's order.
    fn command_values(key: HKEY) -> Vec<(String, String)> {
        let mut out = Vec::new();
        // 16383 characters is the registry's limit for a value name.
        let mut name = vec![0u16; 16384];
        let mut data = vec![0u16; 1024];
        let mut index = 0;
        loop {
            let mut name_len = name.len() as u32;
            let mut kind = REG_VALUE_TYPE::default();
            let mut data_len = (data.len() * 2) as u32;
            let got = unsafe {
                RegEnumValueW(
                    key,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind.0),
                    Some(data.as_mut_ptr().cast()),
                    Some(&mut data_len),
                )
            };
            if got == ERROR_MORE_DATA {
                // The same index again, with room this time.
                data.resize((data_len as usize).div_ceil(2) + 1, 0);
                continue;
            }
            if got == ERROR_NO_MORE_ITEMS {
                break;
            }
            if got != ERROR_SUCCESS {
                log(&format!("autostart: reading a Run value failed ({got:?})"));
                break;
            }
            if let Some(v) = value(&name[..name_len as usize], kind, &data[..(data_len as usize) / 2]) {
                out.push(v);
            }
            index += 1;
        }
        out
    }

    fn value(name: &[u16], kind: REG_VALUE_TYPE, data: &[u16]) -> Option<(String, String)> {
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return None;
        }
        let end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
        let line = expand(&String::from_utf16_lossy(&data[..end]));
        (!line.trim().is_empty()).then(|| (String::from_utf16_lossy(name), line))
    }

    /// Explorer expands REG_SZ lines too; plenty of installers write
    /// `%ProgramFiles%` into one.
    fn expand(line: &str) -> String {
        let src = HSTRING::from(line);
        let need = unsafe { ExpandEnvironmentStringsW(&src, None) };
        if need == 0 {
            return line.to_owned();
        }
        let mut buf = vec![0u16; need as usize];
        let got = unsafe { ExpandEnvironmentStringsW(&src, Some(&mut buf)) };
        if got == 0 || got > need {
            return line.to_owned();
        }
        String::from_utf16_lossy(&buf[..got as usize - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_managers_toggle() {
        assert!(approved(None), "never toggled");
        assert!(approved(Some(&[])));
        assert!(approved(Some(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])));
        assert!(approved(Some(&[0x06, 0, 0])));
        assert!(!approved(Some(&[0x03, 0x8c, 0x1f])), "disabled, with the time it was");
        assert!(!approved(Some(&[0x07])));
    }

    #[test]
    fn run_once_prefixes() {
        assert_eq!(run_once("Setup"), Delete::Before);
        assert_eq!(run_once("!Setup"), Delete::After);
        assert_eq!(run_once("*Setup"), Delete::Before);
        assert_eq!(run_once("*!Setup"), Delete::After);
        assert_eq!(run_once("Set!up"), Delete::Before);
    }

    #[test]
    fn startup_folder_skips_its_own_desktop_ini() {
        assert!(!startup_file("desktop.ini"));
        assert!(!startup_file("Desktop.INI"));
        assert!(startup_file("Discord.lnk"));
    }
}
