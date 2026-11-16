//! Windows' light/dark mode from the config (COSMIC's Mode is_dark), set at
//! every start of the shell, so a changed config reaches an existing install
//! through `dfnstrate update` and apps opened after logon start in it.

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW};
use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
use windows::core::{PCWSTR, w};

use crate::{log, settings};

const KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

pub fn apply() {
    let light = u32::from(!settings::get().dark);
    let mut changed = false;
    for value in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        if read(value) == Some(light) {
            continue;
        }
        let bytes = light.to_le_bytes();
        let set = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, KEY, value, REG_DWORD.0, Some(bytes.as_ptr().cast()), bytes.len() as u32) };
        if set.is_err() {
            log(&format!("setting {} failed: {set:?}", unsafe { value.display() }));
            return;
        }
        changed = true;
    }
    if !changed {
        return;
    }
    log(&format!("Windows' {} mode set", if light == 1 { "light" } else { "dark" }));
    // What the Settings app sends. Its own thread with a timeout: a broadcast
    // waits on every top-level window, and one busy at logon once stalled
    // the shell before it had its keyboard hook (desktop.rs, reset_work_area).
    std::thread::spawn(|| unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        );
    });
}

fn read(value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut len = size_of::<u32>() as u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, KEY, value, RRF_RT_REG_DWORD, None, Some((&raw mut data).cast()), Some(&mut len)) }.ok().ok()?;
    Some(data)
}
