//! The monitors, from EnumDisplayMonitors, in physical pixels (the shell is
//! Per-Monitor V2 DPI aware, main.rs), each known to the `Wm` by its monitor
//! device's interface path, which stays the same when it's unplugged and
//! plugged back into the same port.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    EDD_GET_DEVICE_INTERFACE_NAME, GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXSCREEN, SM_CYSCREEN, SPI_SETWORKAREA, SystemParametersInfoW,
};
use windows::core::{BOOL, PCWSTR};

use crate::log;
use crate::tree::Rect;
use crate::wm::OutputId;

#[derive(Clone)]
pub struct Monitor {
    pub id: OutputId,
    pub rect: Rect,
    pub dpi: u32,
    pub primary: bool,
    /// The display adapter driving it, for the log: "Microsoft Basic Display
    /// Adapter" there means the GPU's driver isn't in (yet).
    pub adapter: String,
    /// The adapter output, \\.\DISPLAYn, which display modes are set on.
    pub device: String,
}

/// Every monitor, the primary first: it's where an unplugged monitor's
/// workspaces go when the shell starts with them all there.
pub fn monitors() -> Vec<Monitor> {
    let mut found: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(found_monitor), LPARAM((&raw mut found) as isize));
    }
    found.sort_by_key(|m| !m.primary);
    if found.is_empty() {
        log("EnumDisplayMonitors found nothing; using the primary screen's size");
        let rect = unsafe { Rect { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) } };
        found.push(Monitor { id: OutputId("primary".into()), rect, dpi: 96, primary: true, adapter: "?".into(), device: String::new() });
    }
    found
}

unsafe extern "system" fn found_monitor(m: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
    let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
    if !unsafe { GetMonitorInfoW(m, (&raw mut info).cast()) }.as_bool() {
        return true.into();
    }
    let device = String::from_utf16_lossy(&info.szDevice[..info.szDevice.iter().position(|&c| c == 0).unwrap_or(32)]);
    // The adapter's \\.\DISPLAYn gets handed out again in a different
    // order after a change; the monitor behind it has a path that doesn't.
    let mut dd = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
    let path = unsafe { EnumDisplayDevicesW(windows::core::PCWSTR(info.szDevice.as_ptr()), 0, &mut dd, EDD_GET_DEVICE_INTERFACE_NAME) }
        .as_bool()
        .then(|| String::from_utf16_lossy(&dd.DeviceID[..dd.DeviceID.iter().position(|&c| c == 0).unwrap_or(128)]))
        .filter(|p| !p.is_empty());
    let (mut x, mut y) = (96, 96);
    let _ = unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    // No taskbar or other appbar reserves any of it: tile the whole monitor.
    let r = info.monitorInfo.rcMonitor;
    let adapter = adapter_of(&device).unwrap_or_else(|| "?".into());
    unsafe { &mut *(out.0 as *mut Vec<Monitor>) }.push(Monitor {
        id: OutputId(path.unwrap_or_else(|| device.clone())),
        rect: Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom },
        dpi: x,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        adapter,
        device,
    });
    true.into()
}

/// The name of the adapter whose output is `device` (\\.\DISPLAYn).
fn adapter_of(device: &str) -> Option<String> {
    let wide = |a: &[u16]| String::from_utf16_lossy(&a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())]);
    (0..).map_while(|i| {
        let mut dd = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool().then_some(dd)
    })
    .find(|dd| wide(&dd.DeviceName) == device)
    .map(|dd| wide(&dd.DeviceString))
}

/// Sets the configured modes. A monitor with no interface path (only its
/// \\.\DISPLAYn) has no EDID to match by, and is left as it is.
pub fn set_modes(list: &[Monitor]) {
    for m in list.iter().filter(|m| m.id.0.starts_with(r"\\?\")) {
        crate::modes::apply(&m.device, &m.id.0);
    }
}

pub fn describe(list: &[Monitor]) -> String {
    list.iter().map(|m| format!("{} {:?} at {} dpi on {}", m.id.0, m.rect, m.dpi, m.adapter)).collect::<Vec<_>>().join(", ")
}

/// A shell that sets the work area owns it; the previous shell (or the bar
/// this one used to have) may have left part of it reserved. Windows only
/// has one to set, the primary monitor's. No SPIF_SENDCHANGE: that
/// broadcasts WM_SETTINGCHANGE and waits on every window, and on a first
/// boot a busy installer stalled it long enough that the shell never got as
/// far as its keyboard hook.
pub fn reset_work_area(list: &[Monitor]) {
    let Some(primary) = list.iter().find(|m| m.primary).or(list.first()) else { return };
    let r = primary.rect;
    let mut full = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
    if let Err(e) = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some((&raw mut full).cast()), Default::default()) } {
        log(&format!("resetting the work area failed: {e}"));
    }
}
