//! The environment the shell hands to everything it starts. Explorer, as the
//! shell, rebuilds its own from the registry when told to with
//! WM_SETTINGCHANGE("Environment"), which is how a PATH entry an installer
//! adds reaches apps started after it. Without explorer nobody did, so a
//! winget app that put itself on PATH wasn't found until the next logon.
//! Every way the shell starts something inherits the process environment,
//! so replacing that is all it takes.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStringExt;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PCWSTR;

use crate::log;

/// lParam of a WM_SETTINGCHANGE: true when it's the environment that changed.
pub fn is_environment(lparam: isize) -> bool {
    if lparam == 0 {
        return false;
    }
    let name = unsafe { PCWSTR(lparam as *const u16).to_string() };
    name.is_ok_and(|n| n.eq_ignore_ascii_case("Environment"))
}

/// Takes on the environment a new logon of this user would get now.
pub fn reload() {
    let Some(fresh) = fresh() else { return };
    let current = by_key(std::env::vars_os());
    let mut changed = Vec::new();
    for (key, (k, v)) in &fresh {
        if current.get(key).map(|(_, v)| v) != Some(v) {
            // SAFETY: set_var is unsafe because another thread could be
            // reading the environment through libc's getenv at the same
            // time. On Windows std goes through SetEnvironmentVariableW,
            // which takes the PEB lock, and the shell's other thread (the
            // keyboard hook) never reads the environment anyway.
            unsafe { std::env::set_var(k, v) };
            changed.push(k.to_string_lossy().into_owned());
        }
    }
    for (_, (k, _)) in current.iter().filter(|(key, _)| !fresh.contains_key(*key)) {
        // SAFETY: as above.
        unsafe { std::env::remove_var(k) };
        changed.push(format!("-{}", k.to_string_lossy()));
    }
    if !changed.is_empty() {
        log(&format!("environment reloaded, changed: {}", changed.join(" ")));
        log_path();
    }
}

pub fn log_path() {
    log(&format!("PATH={}", std::env::var_os("PATH").unwrap_or_default().to_string_lossy()));
}

/// Names are case-insensitive on Windows: "Path" and "PATH" are one
/// variable, so they're compared uppercased.
type Vars = HashMap<String, (OsString, OsString)>;

fn by_key(vars: impl Iterator<Item = (OsString, OsString)>) -> Vars {
    vars.filter(|(k, _)| !hidden(k)).map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v))).collect()
}

/// cmd.exe's per-drive working directories ("=C:"): process state, not
/// environment, and no logon has them.
fn hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('=')
}

fn fresh() -> Option<Vars> {
    unsafe {
        let mut token = HANDLE::default();
        if let Err(e) = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE, &mut token) {
            log(&format!("reloading the environment: opening the shell's token failed: {e}"));
            return None;
        }
        let mut block: *mut c_void = std::ptr::null_mut();
        // Not inherited: the registry's view only, as explorer rebuilds it.
        let made = CreateEnvironmentBlock(&mut block, Some(token), false);
        let _ = CloseHandle(token);
        if let Err(e) = made {
            log(&format!("reloading the environment: CreateEnvironmentBlock failed: {e}"));
            return None;
        }
        let vars = by_key(parse(block as *const u16).into_iter());
        let _ = DestroyEnvironmentBlock(block);
        Some(vars)
    }
}

/// NAME=value\0NAME=value\0\0
unsafe fn parse(mut p: *const u16) -> Vec<(OsString, OsString)> {
    let mut out = Vec::new();
    loop {
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        if len == 0 {
            return out;
        }
        let entry = unsafe { std::slice::from_raw_parts(p, len) };
        // Search from 1: a hidden variable's name starts with '='.
        if let Some(eq) = entry.iter().skip(1).position(|&c| c == u16::from(b'=')).map(|i| i + 1) {
            out.push((OsString::from_wide(&entry[..eq]), OsString::from_wide(&entry[eq + 1..])));
        }
        p = unsafe { p.add(len + 1) };
    }
}
