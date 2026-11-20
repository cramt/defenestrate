//! Default apps (the config's `defaultApps`), by asking. Windows keeps the
//! default for a file type or URL scheme the user's own choice: UserChoice
//! carries a hash, UCPD.sys stops programs writing it, and the policy for
//! it (DefaultAssociationsConfiguration) only applies on domain-joined
//! machines; on saturn two sign-ins left https on Edge (2026-11-20). What's
//! left is Settings' page for the app, one click on "Set default".

use crate::settings::DefaultApp;

/// An app as RegisteredApplications names it, in the hive it's in: what
/// Settings' `registeredAppMachine` / `registeredAppUser` take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    pub name: String,
    pub machine: bool,
}

impl Registered {
    pub fn settings_uri(&self) -> String {
        let scope = if self.machine { "Machine" } else { "User" };
        format!("ms-settings:defaultapps?registeredApp{scope}={}", self.name)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Everything goes where the config says.
    Done,
    /// Some ProgIds have no app registering them yet (winget installs apps
    /// during the first logon): look again later.
    Wait(Vec<String>),
    /// Open Settings at these apps' pages; every wrong one has an app.
    Ask(Vec<Registered>),
}

/// What to do, given what each id goes to now (`current`) and which
/// registered app claims a ProgId for an id (`owner`).
pub fn next(wants: &[DefaultApp], current: impl Fn(&str) -> Option<String>, owner: impl Fn(&str, &str) -> Option<Registered>) -> Next {
    let mut ask: Vec<Registered> = Vec::new();
    let mut missing = Vec::new();
    for w in wants {
        if current(&w.id).is_some_and(|p| p.eq_ignore_ascii_case(&w.prog_id)) {
            continue;
        }
        match owner(&w.id, &w.prog_id) {
            Some(app) if !ask.contains(&app) => ask.push(app),
            Some(_) => {}
            None => missing.push(w.prog_id.clone()),
        }
    }
    // Asking before every app is in would mean asking twice.
    if !missing.is_empty() {
        missing.dedup();
        Next::Wait(missing)
    } else if ask.is_empty() {
        Next::Done
    } else {
        Next::Ask(ask)
    }
}

#[cfg(windows)]
pub use sys::start;

#[cfg(windows)]
mod sys {
    use std::time::Duration;

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::HSTRING;

    use super::{Next, Registered, next};
    use crate::{launcher, log, settings};

    /// Checks every 30 s for half an hour, long enough for the first
    /// logon's winget run, and asks at most once per logon.
    pub fn start() {
        let wants = settings::get().default_apps.clone();
        if wants.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            let mut waited_for: Vec<String> = Vec::new();
            for _ in 0..60 {
                match next(&wants, current, owner) {
                    Next::Done => return log("default apps: all set"),
                    Next::Ask(apps) => {
                        for app in apps {
                            let uri = app.settings_uri();
                            log(&format!("default apps: not set yet, opening {uri}"));
                            launcher::open(std::path::Path::new(&uri));
                        }
                        return;
                    }
                    Next::Wait(missing) => {
                        if missing != waited_for {
                            log(&format!("default apps: waiting for an app to register {}", missing.join(", ")));
                            waited_for = missing;
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(30));
            }
            log(&format!("default apps: gave up waiting for {}", waited_for.join(", ")));
        });
    }

    fn string(root: HKEY, key: &str, name: &str) -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let got = unsafe {
            RegGetValueW(root, &HSTRING::from(key), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
        };
        if got != ERROR_SUCCESS {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    fn is_extension(id: &str) -> bool {
        id.starts_with('.')
    }

    fn current(id: &str) -> Option<String> {
        let key = if is_extension(id) {
            format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{id}\UserChoice")
        } else {
            format!(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\{id}\UserChoice")
        };
        string(HKEY_CURRENT_USER, &key, "ProgId")
    }

    /// The registered app whose Capabilities send `id` to `prog_id`.
    fn owner(id: &str, prog_id: &str) -> Option<Registered> {
        let list = if is_extension(id) { "FileAssociations" } else { "URLAssociations" };
        for (root, machine) in [(HKEY_LOCAL_MACHINE, true), (HKEY_CURRENT_USER, false)] {
            for (name, capabilities) in values(root, r"Software\RegisteredApplications") {
                if string(root, &format!(r"{capabilities}\{list}"), id).is_some_and(|p| p.eq_ignore_ascii_case(prog_id)) {
                    return Some(Registered { name, machine });
                }
            }
        }
        None
    }

    /// A key's string values, name -> data.
    fn values(root: HKEY, key: &str) -> Vec<(String, String)> {
        use windows::Win32::System::Registry::{KEY_QUERY_VALUE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegEnumValueW, RegOpenKeyExW};
        use windows::core::PWSTR;
        let mut out = Vec::new();
        let mut k = HKEY::default();
        if unsafe { RegOpenKeyExW(root, &HSTRING::from(key), None, KEY_QUERY_VALUE, &mut k) } != ERROR_SUCCESS {
            return out;
        }
        for i in 0.. {
            let mut name = vec![0u16; 16384];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u16; 1024];
            let mut data_len = (data.len() * 2) as u32;
            let mut kind = REG_VALUE_TYPE::default();
            let got = unsafe {
                RegEnumValueW(k, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut kind.0), Some(data.as_mut_ptr().cast()), Some(&mut data_len))
            };
            if got != ERROR_SUCCESS {
                break;
            }
            if kind == REG_SZ {
                let d = (data_len as usize / 2).saturating_sub(1);
                out.push((String::from_utf16_lossy(&name[..name_len as usize]), String::from_utf16_lossy(&data[..d])));
            }
        }
        unsafe {
            let _ = RegCloseKey(k);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(id: &str, prog_id: &str) -> DefaultApp {
        DefaultApp { id: id.into(), prog_id: prog_id.into() }
    }

    fn firefox() -> Registered {
        Registered { name: "Firefox-308046B0AF4A39CB".into(), machine: true }
    }

    #[test]
    fn asks_once_per_app_when_its_in() {
        let wants = [want("https", "FirefoxURL-1"), want(".html", "FirefoxHTML-1")];
        let edge = |_: &str| Some("MSEdgeHTM".to_owned());
        let installed = |_: &str, _: &str| Some(firefox());
        assert_eq!(next(&wants, edge, installed), Next::Ask(vec![firefox()]));
        assert_eq!(firefox().settings_uri(), "ms-settings:defaultapps?registeredAppMachine=Firefox-308046B0AF4A39CB");
    }

    #[test]
    fn waits_for_the_app_and_stops_once_set() {
        let wants = [want("https", "FirefoxURL-1")];
        assert_eq!(next(&wants, |_| None, |_, _| None), Next::Wait(vec!["FirefoxURL-1".into()]));
        // UserChoice keeps whatever case the app registered.
        assert_eq!(next(&wants, |_| Some("firefoxurl-1".into()), |_, _| None), Next::Done);
    }
}
