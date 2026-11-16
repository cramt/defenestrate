//! Sets the configured display modes (display.rs) on the monitors that are
//! there: at startup and whenever the monitors change.

use std::cell::RefCell;
use std::collections::HashSet;

use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW};
use windows::core::{HSTRING, PCWSTR, w};

use crate::display::Edid;
use crate::{log, settings};

thread_local! {
    /// Monitors (by device path) a mode was set on this run. Each gets one
    /// try: setting a mode raises WM_DISPLAYCHANGE, which comes back here,
    /// and a mode that doesn't stick mustn't be set again forever. Windows
    /// keeps a set mode for that monitor from then on anyway.
    static TRIED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// `device` is the adapter output (\\.\DISPLAYn), `path` the monitor's
/// device interface path (\\?\DISPLAY#PHLC310#5&...#{guid}).
pub fn apply(device: &str, path: &str) {
    let modes = &settings::get().displays;
    if modes.is_empty() || TRIED.with_borrow(|t| t.contains(path)) {
        return;
    }
    let Some(edid) = edid(path) else {
        log(&format!("display modes: no EDID for {path}"));
        return;
    };
    let edid = Edid::parse(&edid);
    let Some(want) = modes.iter().find(|m| m.is_for(&edid)) else {
        log(&format!("display modes: none configured for {path} (model {:?}, serial {:?})", edid.model, edid.serial));
        return;
    };
    let device = HSTRING::from(device);
    let offered = (0..).map_while(|i| mode(&device, ENUM_DISPLAY_SETTINGS_MODE(i))).map(|m| (m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency));
    let Some(hz) = want.pick(offered) else {
        log(&format!("display modes: {path} offers nothing at {}x{}", want.width, want.height));
        return;
    };
    let Some(mut current) = mode(&device, ENUM_CURRENT_SETTINGS) else { return };
    let now = (current.dmPelsWidth, current.dmPelsHeight, current.dmDisplayFrequency);
    if now == (want.width, want.height, hz) {
        return;
    }
    TRIED.with_borrow_mut(|t| t.insert(path.to_owned()));
    current.dmPelsWidth = want.width;
    current.dmPelsHeight = want.height;
    current.dmDisplayFrequency = hz;
    current.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    // Per user, no admin needed, as the Settings app does it.
    let result = unsafe { ChangeDisplaySettingsExW(&device, Some(&current), None, CDS_UPDATEREGISTRY, None) };
    let outcome = if result == DISP_CHANGE_SUCCESSFUL { "done".to_owned() } else { format!("failed ({})", result.0) };
    log(&format!("display modes: {path} {}x{}@{} -> {}x{}@{hz}: {outcome}", now.0, now.1, now.2, want.width, want.height));
}

fn mode(device: &HSTRING, which: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut m = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(device, which, &mut m) }.as_bool().then_some(m)
}

/// The monitor's EDID as Windows read it, kept under its device instance:
/// the interface path minus its \\?\ and #{class guid}, with \ for #.
fn edid(path: &str) -> Option<Vec<u8>> {
    let instance = path.strip_prefix(r"\\?\")?.rsplit_once('#')?.0.replace('#', r"\");
    let key = HSTRING::from(format!(r"SYSTEM\CurrentControlSet\Enum\{instance}\Device Parameters"));
    let mut buf = vec![0u8; 1024];
    let mut len = buf.len() as u32;
    let found = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), w!("EDID"), RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
    };
    found.is_ok().then(|| {
        buf.truncate(len as usize);
        buf
    })
}
