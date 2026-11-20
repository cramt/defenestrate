//! The window manager's hands and eyes: WinEvent hooks feed `Wm` the
//! windows that come and go, and `apply` makes the screen match the
//! `Effects` it returns. Super+drags (keys.rs) and the window switcher's
//! choice (switcher.rs) are carried out here too. Runs entirely on the
//! shell's one GUI thread.
//!
//! Monitors come from EnumDisplayMonitors, in physical pixels (the shell is
//! Per-Monitor V2 DPI aware, main.rs), each known to the `Wm` by its monitor
//! device's interface path, which stays the same when it's unplugged and
//! plugged back into the same port. WM_DISPLAYCHANGE reads them again.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, COLORREF, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenElevation, TokenUIAccess};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, INPUT, INPUT_MOUSE, SendInput, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EDD_GET_DEVICE_INTERFACE_NAME, EVENT_OBJECT_CLOAKED, GetCursorPos, MONITORINFOF_PRIMARY,
    SetCursorPos, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND,
    EVENT_SYSTEM_MOVESIZESTART, EnumWindows, GA_ROOT,
    GetForegroundWindow, HWND_BOTTOM, HWND_TOP,
    GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClientRect, GetPropW, GetSystemMetrics, GetWindow, GetWindowLongPtrW,
    GetClassNameW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HWND_TOPMOST, IsHungAppWindow, IsIconic, IsWindow,
    IsWindowVisible, IsZoomed, KillTimer,
    LWA_ALPHA, PostMessageW, RegisterClassW, RemovePropW, SM_CXSCREEN, SM_CYSCREEN, SPI_SETWORKAREA, SW_HIDE, SW_RESTORE,
    SW_SHOWNA, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow, SetLayeredWindowAttributes, SetPropW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindowAsync, SystemParametersInfoW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE,
    WNDCLASSW, WS_CAPTION, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP, WS_THICKFRAME, WindowFromPoint,
};
use windows::core::{BOOL, PCWSTR, PWSTR, w};

use crate::classify::{Facts, Verdict, classify};
use crate::keys::{Button, Drag};
use crate::motion::{Animator, Pace, Scene};
use crate::follow::{Follow, Timer};
use crate::hint;
use crate::overlay::{self, Look};
use crate::settings;
use crate::tree::{Axis, Direction, Gaps, Point, Rect};
use crate::wm::{Command, Effects, Grab, Hold, OutputId, Screens, Tiling, Wm};
use crate::{log, scaled_at};

/// Posted to the shell's window for every WinEvent: wparam the event,
/// lparam the window. Hook callbacks only post, so all state changes
/// happen in one place, from the message loop.
pub const WM_WINEVENT: u32 = WM_APP + 1;
/// The shell's window gets this WM_TIMER id when the windows whose title
/// bars came or went should be placed again (see `sync_captions`).
pub const REPLACE_TIMER: usize = 0xdf1;
/// The shell's window gets this WM_TIMER id when focus_follows_cursor's
/// delay runs out.
pub const FOLLOW_TIMER: usize = 0xdf2;
/// Sends moves held back by `set_pos`.
const FLUSH_TIMER: usize = 0xdf3;
/// How long a window gets to handle a move before the next goes anyway:
/// one that can't move (at its minimum size, say) mustn't hold the newest
/// place back for good.
const IN_FLIGHT_FOR: Duration = Duration::from_millis(100);
pub use crate::overlay::WM_FRAME;

/// How long, once nothing moves, the overlay waits for the windows that
/// slid or faded out to be hidden, so they don't flash as it goes. A hung
/// app's window doesn't get longer than this.
const SETTLE: Duration = Duration::from_millis(100);

/// How close to the screen's edge, or to the line between two tiles, a
/// dragged window has to be dropped to join that group (COSMIC's 32).
const DROP_BAND: i32 = 32;
/// Marks a window whose title bar the shell took, on the window itself, so
/// a shell that crashed can give it back when it starts again.
const CAPTIONLESS: PCWSTR = w!("defenestrate.captionless");
// COLORREF is 0x00BBGGRR: COSMIC's default accent blue.
const PREVIEW_COLOUR: COLORREF = COLORREF(0x00E4_B163);
const PREVIEW_ALPHA: u8 = 96;
/// What a tiled window loses. The sizing border goes with the caption:
/// without a caption, Windows 11 paints the top sizing border as a white
/// strip across the window.
const TITLE_BAR: u32 = WS_CAPTION.0 | WS_THICKFRAME.0;

#[derive(Clone, Copy)]
enum Dragging {
    /// A tile following the pointer, previewing where it would drop.
    Tile { w: HWND, from: Point, start: RECT },
    /// A floating window following the pointer.
    Float { w: HWND, from: Point, start: RECT },
    /// The edge between two tiles, at `at` when the drag started.
    TileEdge { w: HWND, from: Point, corner: [Direction; 2], axis: Axis, at: i32 },
    /// A floating window's corner.
    FloatCorner { w: HWND, from: Point, start: RECT, corner: [Direction; 2] },
}

/// Which windows Windows lets the shell move. UIPI keeps a process from
/// moving, sending input to or hooking the input of windows above its
/// integrity level, i.e. elevated ones.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Reach {
    /// Elevated, or with uiAccess, which bypasses UIPI without elevation:
    /// https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-only-elevate-uiaccess-applications-that-are-installed-in-secure-locations
    Every,
    /// Medium integrity without uiAccess, e.g. the signature check failed.
    SameLevel,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// The overlay is up, drawing motion.rs's frames.
    Running,
    /// Done moving; waiting until `until` at most for what left to be hidden.
    Settling { until: Duration },
}

thread_local! {
    static WM: RefCell<Option<Wm<HWND>>> = const { RefCell::new(None) };
    static ANIM: RefCell<Option<Animator<HWND>>> = const { RefCell::new(None) };
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Idle) };
    static EPOCH: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Off while the shell picks up the windows already open: those
    /// shouldn't fade in.
    static ANIMATING: Cell<bool> = const { Cell::new(false) };
    /// Windows to hide once the animation is over: until then they're
    /// what slides or fades out.
    static DEFERRED: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Windows asked to move that haven't yet: where they were, and where
    /// they're going.
    static PENDING: RefCell<Vec<(HWND, RECT, RECT)>> = const { RefCell::new(Vec::new()) };
    /// Moves sent that the window hasn't handled yet: where it was then,
    /// and when (set_pos).
    static IN_FLIGHT: RefCell<Vec<(HWND, RECT, Instant)>> = const { RefCell::new(Vec::new()) };
    /// The newest place for a window still busy with the last one.
    static HELD: RefCell<Vec<(HWND, RECT)>> = const { RefCell::new(Vec::new()) };
    static EVENTS_TO: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static REACH: Cell<Reach> = const { Cell::new(Reach::SameLevel) };
    static DRAGGING: Cell<Option<Dragging>> = const { Cell::new(None) };
    static PREVIEW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Windows the shell took the title bar from.
    static STRIPPED: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// A window is being moved or sized by its own title bar or borders.
    static MOVING: Cell<bool> = const { Cell::new(false) };
    static FOLLOW: RefCell<Follow<HWND>> = RefCell::new(Follow::new());
    static MONITORS: RefCell<Vec<Monitor>> = const { RefCell::new(Vec::new()) };
    /// Windows whose verdict is in the log already.
    static LOGGED: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

#[derive(Clone)]
struct Monitor {
    id: OutputId,
    rect: Rect,
    dpi: u32,
    primary: bool,
    /// The display adapter driving it, for the log: "Microsoft Basic Display
    /// Adapter" there means the GPU's driver isn't in (yet).
    adapter: String,
    /// The adapter output, \\.\DISPLAYn, which display modes are set on.
    device: String,
}

/// Every monitor, the primary first: it's where an unplugged monitor's
/// workspaces go when the shell starts with them all there.
fn monitors() -> Vec<Monitor> {
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
fn set_modes(list: &[Monitor]) {
    for m in list.iter().filter(|m| m.id.0.starts_with(r"\\?\")) {
        crate::modes::apply(&m.device, &m.id.0);
    }
}

fn screens(list: &[Monitor]) -> Screens {
    Screens::new(list.iter().map(|m| (m.id.clone(), m.rect)).collect()).expect("monitors() never returns none")
}

/// The DPI of the monitor `p` is on (the primary's if none).
fn dpi_at(p: Point) -> u32 {
    MONITORS.with_borrow(|ms| ms.iter().find(|m| m.rect.contains(p)).or(ms.first()).map_or(96, |m| m.dpi))
}

fn monitor_of(r: Rect) -> Option<OutputId> {
    let c = r.centre();
    MONITORS.with_borrow(|ms| ms.iter().find(|m| m.rect.contains(c)).map(|m| m.id.clone()))
}

/// The box around every monitor, which the animation overlay covers.
fn desktop_bounds() -> Rect {
    MONITORS.with_borrow(|ms| {
        ms.iter().map(|m| m.rect).reduce(|a, b| Rect { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) })
    })
    .unwrap_or_else(|| unsafe { Rect { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) } })
}

fn cursor() -> Option<Point> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok().map(|()| Point { x: p.x, y: p.y })
}

/// One hold of the modifiers, from keys.rs, for the command being run.
pub fn command(c: Command, hold: Hold) {
    update(|wm| Some(wm.command(c, hold)));
}

/// Where the launcher and the switcher open: the monitor the pointer is
/// on, as COSMIC opens them on the active output. With its DPI.
pub fn overlay_spot() -> (Rect, u32) {
    let at = WM.with_borrow_mut(|wm| {
        let wm = wm.as_mut()?;
        if let Some(p) = cursor() {
            wm.pointer(p);
        }
        Some(wm.active_area())
    });
    let area = at.unwrap_or_else(desktop_bounds);
    (area, dpi_at(area.centre()))
}

/// Runs `f` on the state and applies what it returns, with the state no
/// longer borrowed: moving, showing and focusing windows pumps messages,
/// and those can land back here.
fn update(f: impl FnOnce(&mut Wm<HWND>) -> Option<Effects<HWND>>) {
    let pointer = cursor();
    let fx = WM.with_borrow_mut(|wm| {
        let wm = wm.as_mut()?;
        // COSMIC's active output follows every pointer motion; asking where
        // the pointer is now comes to the same thing.
        if let Some(p) = pointer {
            wm.pointer(p);
        }
        let fx = f(wm)?;
        // Our own focusing may not come back as a foreground event, and the
        // switcher's order mustn't depend on it.
        if let Some(w) = fx.focus {
            wm.focused(w);
        }
        Some(fx)
    });
    if let Some(fx) = fx {
        apply(fx);
        sync_captions();
    }
    sync_hint();
}

/// The active hint goes around the focused window, if it's managed, shown,
/// not maximized (as in COSMIC) and really has the keyboard. It's hidden
/// while anything moves: animations (the overlay draws then), drags, or a
/// game covering the screen.
fn sync_hint() {
    if settings::get().active_hint.is_none() {
        return;
    }
    let at = || {
        if PHASE.get() != Phase::Idle || DRAGGING.get().is_some() || MOVING.get() {
            return None;
        }
        let (w, tile) = WM.with_borrow(|wm| wm.as_ref().and_then(Wm::hinted))?;
        if unsafe { GetForegroundWindow() } != w || fullscreen_app() {
            return None;
        }
        tile.or_else(|| visible_frame(w).map(|(_, r)| r))
    };
    hint::show(at());
}

pub fn start(events_to: HWND) {
    EVENTS_TO.set(events_to);
    let me = unsafe { GetCurrentProcess() };
    let (elevated, ui_access) = (is_elevated(me), token_flag(me, TokenUIAccess));
    let reach = if elevated || ui_access { Reach::Every } else { Reach::SameLevel };
    log(&format!("TokenElevation={} TokenUIAccess={}: managing {reach:?} windows", elevated as u8, ui_access as u8));
    if crate::handoff::BUILD == crate::handoff::Build::UiAccess && !ui_access {
        log("Windows started the uiAccess build without uiAccess: is its certificate (CN=defenestrate shell) in the machine's Trusted Root store?");
    }
    REACH.set(reach);
    let found = monitors();
    log(&format!("monitors: {}", describe(&found)));
    // Its WM_DISPLAYCHANGE lays out the tiles again at the new size.
    set_modes(&found);
    reset_work_area(&found);
    // One size for every monitor: the primary's, which is first.
    let dpi = found.first().map_or(96, |m| m.dpi);
    let config = settings::get();
    let gaps = Gaps { outer: scaled_at(config.gaps.outer.into(), dpi), inner: scaled_at(config.gaps.inner.into(), dpi) };
    let tiling = Tiling {
        gaps,
        autotile: config.autotile,
        behavior: config.autotile_behavior,
        cursor_follows_focus: config.cursor_follows_focus,
        activation_policy: config.activation_policy,
    };
    WM.set(Some(Wm::with_tiling(screens(&found), tiling)));
    if let Some(h) = config.active_hint
        && let Err(e) = hint::create(h, scaled_at(h.thickness.into(), dpi))
    {
        log(&format!("creating the active hint failed: {e}"));
    }
    MONITORS.set(found);
    EPOCH.set(Some(Instant::now()));
    if let Err(e) = create_preview() {
        log(&format!("creating the drop preview failed: {e}"));
    }
    match overlay::create(events_to) {
        Ok(()) => log(&format!("window animations {}", if overlay::enabled() { "on" } else { "off (Animation effects)" })),
        Err(e) => log(&format!("creating the animation overlay failed: {e}")),
    }

    let mut existing: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM((&raw mut existing) as isize));
    }
    for &w in &existing {
        restore_caption(w);
    }
    // EnumWindows goes top of the z-order down; adding bottom-up leaves
    // the topmost window focused, as it was.
    for w in existing.into_iter().rev() {
        if !visible(w) {
            continue;
        }
        let verdict = verdict(w);
        log_verdict(w, verdict);
        if verdict == Verdict::Tile {
            if unsafe { IsIconic(w) }.as_bool() {
                WM.with_borrow_mut(|wm| wm.as_mut().map(|wm| wm.add_minimized(w)));
            } else {
                // A restarted shell keeps each window on its monitor.
                let centre = visible_frame(w).map(|(_, r)| r.centre());
                update(|wm| Some(match centre {
                    Some(c) => wm.add_at(w, c),
                    None => wm.add(w),
                }));
            }
        }
    }
    ANIMATING.set(true);

    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    // Narrow ranges: one spanning everything would wake the shell for
    // every cursor move (EVENT_OBJECT_LOCATIONCHANGE).
    for (min, max) in [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ] {
        let hook = unsafe { SetWinEventHook(min, max, None, Some(on_winevent), 0, 0, flags) };
        if hook.is_invalid() {
            log(&format!("SetWinEventHook({min:#x}..{max:#x}) failed"));
        }
    }
}

unsafe extern "system" fn collect(w: HWND, out: LPARAM) -> BOOL {
    unsafe { (*(out.0 as *mut Vec<HWND>)).push(w) };
    true.into()
}

unsafe extern "system" fn on_winevent(_: HWINEVENTHOOK, event: u32, w: HWND, object: i32, child: i32, _: u32, _: u32) {
    // OBJID_WINDOW and CHILDID_SELF: the window itself, not a caret,
    // cursor or control inside it.
    if object != 0 || child != 0 || w.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(EVENTS_TO.get()), WM_WINEVENT, WPARAM(event as usize), LPARAM(w.0 as isize));
    }
}

pub fn on_event(event: u32, w: HWND) {
    match event {
        EVENT_SYSTEM_MOVESIZESTART => MOVING.set(true),
        EVENT_SYSTEM_MOVESIZEEND => MOVING.set(false),
        _ => {}
    }
    update(|wm| match event {
        EVENT_OBJECT_DESTROY => {
            // Handles get reused; the next window with this one is new.
            LOGGED.with_borrow_mut(|seen| seen.remove(&(w.0 as usize)));
            Some(wm.remove(w))
        }
        EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED | EVENT_SYSTEM_MINIMIZEEND => {
            if wm.is_minimized(w) {
                on_screen(w).then(|| wm.restored(w))
            } else if wm.manages(w) {
                // Ours to show is shown already; this one showed itself.
                if on_screen(w) { wm.raised(w) } else { None }
            } else if on_screen(w) {
                let verdict = verdict(w);
                log_verdict(w, verdict);
                (verdict == Verdict::Tile).then(|| wm.add_new(w))
            } else {
                None
            }
        }
        // Managed windows on other workspaces are hidden by us; only a
        // window on screen going invisible has really gone, or a minimized
        // one (to a tray icon, say). Checked now rather than trusted from
        // the event: by the time it arrives, a quick switch back may have
        // shown the window again, or the app restored it.
        EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED | EVENT_SYSTEM_MINIMIZESTART => {
            if (wm.is_shown(w) || wm.is_minimized(w)) && !visible(w) {
                Some(wm.remove(w))
            } else {
                (wm.is_shown(w) && unsafe { IsIconic(w) }.as_bool()).then(|| wm.minimize(w))
            }
        }
        EVENT_SYSTEM_FOREGROUND => wm.raised(w).or_else(|| {
            wm.focused(w);
            None
        }),
        _ => None,
    });
    // A game going fullscreen mid-animation gets the screen to itself at once.
    if event == EVENT_SYSTEM_FOREGROUND && PHASE.get() != Phase::Idle && fullscreen_app() {
        cut();
    }
    if event == EVENT_SYSTEM_FOREGROUND {
        // A window that took focus without ever showing as a new one gets
        // its verdict line now: borderless League's game never had one, so
        // there was nothing to say why it stayed where it was (2026-11-20).
        let root = unsafe { GetAncestor(w, GA_ROOT) };
        if !root.is_invalid() && WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| !wm.manages(root))) && on_screen(root) {
            log_verdict(root, verdict(root));
        }
        raise_over_tiles(w);
    }
}

/// A focused window the shell doesn't place goes over any tile drawn over
/// it (wm::tiles_over). Logged, since why Windows left it under one is
/// still unknown.
fn raise_over_tiles(w: HWND) {
    let w = unsafe { GetAncestor(w, GA_ROOT) };
    if w.is_invalid() || WM.with_borrow(|wm| wm.as_ref().is_none_or(|wm| wm.manages(w))) {
        return;
    }
    let Some((_, at)) = visible_frame(w) else { return };
    let tiles: Vec<(HWND, Rect)> = WM
        .with_borrow(|wm| wm.as_ref().map(Wm::tiled).unwrap_or_default())
        .into_iter()
        .filter(|&t| on_screen(t))
        .filter_map(|t| Some((t, visible_frame(t)?.1)))
        .collect();
    let over = crate::wm::tiles_over(&z_order(), w, at, &tiles);
    if over.is_empty() {
        return;
    }
    let over: Vec<String> = over.iter().map(|t| format!("{:#x}", t.0 as usize)).collect();
    log(&format!("focused window {:#x} was under tile(s) {}; raising it", w.0 as usize, over.join(", ")));
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    unsafe {
        let _ = SetWindowPos(w, Some(HWND_TOP), 0, 0, 0, 0, flags);
    }
}

/// What the window switcher lists, most recently focused first: COSMIC
/// lists every toplevel, so besides the managed windows (minimized, or on
/// other workspaces) that's every window Windows' own Alt+Tab would show,
/// such as a full-screen game, or an elevated app when the shell runs
/// without uiAccess, which the shell doesn't tile. Ones never focused go
/// last, topmost first.
pub fn switchable() -> Vec<HWND> {
    let z = z_order();
    WM.with_borrow(|wm| {
        let Some(wm) = wm else { return Vec::new() };
        let ok = |w: HWND| wm.manages(w) || app_window(w);
        let mut list: Vec<HWND> = wm.recent().iter().copied().filter(|&w| ok(w)).collect();
        for w in z {
            if ok(w) && !list.contains(&w) {
                list.push(w);
            }
        }
        list
    })
}

/// The switcher picked `w`. A managed window gets COSMIC's activate (its
/// workspace, restored, focused); anything else is restored and focused.
pub fn activate(w: HWND) {
    if !unsafe { IsWindow(Some(w)) }.as_bool() {
        refocus();
        return;
    }
    let managed = WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| wm.manages(w)));
    if managed {
        update(|wm| Some(wm.activate(w)));
        return;
    }
    unsafe {
        if IsIconic(w).as_bool() {
            let _ = ShowWindowAsync(w, SW_RESTORE);
        }
    }
    focus(w);
    WM.with_borrow_mut(|wm| wm.as_mut().map(|wm| wm.focused(w)));
}

/// WM_DISPLAYCHANGE: a monitor came or went, or one changed resolution.
pub fn screen_changed() {
    let found = monitors();
    log(&format!("monitors changed: {}", describe(&found)));
    set_modes(&found);
    reset_work_area(&found);
    let list = screens(&found);
    MONITORS.set(found);
    // Windows has moved what was on a monitor that went away onto another,
    // and those moves aren't ours to animate.
    cut();
    update(|wm| Some(wm.set_screens(list)));
}

fn describe(list: &[Monitor]) -> String {
    list.iter().map(|m| format!("{} {:?} at {} dpi on {}", m.id.0, m.rect, m.dpi, m.adapter)).collect::<Vec<_>>().join(", ")
}

/// A shell that sets the work area owns it; the previous shell (or the bar
/// this one used to have) may have left part of it reserved. Windows only
/// has one to set, the primary monitor's. No SPIF_SENDCHANGE: that
/// broadcasts WM_SETTINGCHANGE and waits on every window, and on a first
/// boot a busy installer stalled it long enough that the shell never got as
/// far as its keyboard hook.
fn reset_work_area(list: &[Monitor]) {
    let Some(primary) = list.iter().find(|m| m.primary).or(list.first()) else { return };
    let r = primary.rect;
    let mut full = RECT { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
    if let Err(e) = unsafe { SystemParametersInfoW(SPI_SETWORKAREA, 0, Some((&raw mut full).cast()), Default::default()) } {
        log(&format!("resetting the work area failed: {e}"));
    }
}

/// Gives focus back to the workspace's focused window, e.g. after the
/// launcher is dismissed.
pub fn refocus() {
    update(|wm| Some(Effects { focus: wm.focused_window(), ..Effects::default() }));
}

/// Shows everything this hid and gives back every title bar, so no window
/// is lost or stuck without one if the shell exits.
pub fn release() {
    cut();
    let hidden = WM.with_borrow(|wm| wm.as_ref().map(Wm::hidden).unwrap_or_default());
    for w in hidden {
        unsafe {
            let _ = ShowWindowAsync(w, SW_SHOWNA);
        }
    }
    for w in STRIPPED.take() {
        restore_caption(w);
    }
}

/// The managed window on screen under `p`, if any.
fn window_under(p: Point) -> Option<HWND> {
    let w = unsafe { GetAncestor(WindowFromPoint(POINT { x: p.x, y: p.y }), GA_ROOT) };
    WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| wm.is_shown(w))).then_some(w)
}

/// focus_follows_cursor: the pointer moved (keys.rs only says so while
/// it's on). As COSMIC skips it while the pointer is grabbed, nothing
/// happens during a drag or with a button held.
pub fn pointer_moved(p: Point) {
    let Some(ffc) = settings::get().focus_follows_cursor else { return };
    let held = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|&b| unsafe { GetAsyncKeyState(b.0.into()) } < 0);
    if DRAGGING.get().is_some() || MOVING.get() || held {
        return;
    }
    let under = window_under(p);
    let timer = FOLLOW.with_borrow_mut(|f| f.moved(under));
    unsafe {
        match timer {
            Timer::Restart => {
                SetTimer(Some(EVENTS_TO.get()), FOLLOW_TIMER, ffc.delay_ms.try_into().unwrap_or(u32::MAX), None);
            }
            Timer::Cancel => {
                let _ = KillTimer(Some(EVENTS_TO.get()), FOLLOW_TIMER);
            }
            Timer::Leave => {}
        }
    }
}

/// focus_follows_cursor's delay ran out. The launcher and the switcher keep
/// the keyboard while they're up, as COSMIC's keyboard-exclusive layers do.
fn follow_focus() {
    let Some(w) = FOLLOW.with_borrow_mut(Follow::fire) else { return };
    let foreground = unsafe { GetForegroundWindow() };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
    if foreground == w || pid == unsafe { GetCurrentProcessId() } {
        return;
    }
    update(|wm| wm.is_shown(w).then(|| Effects { focus: Some(w), ..Effects::default() }));
}

pub fn on_timer(id: usize) {
    if id == FOLLOW_TIMER {
        unsafe {
            let _ = KillTimer(Some(EVENTS_TO.get()), id);
        }
        follow_focus();
        return;
    }
    if id == FLUSH_TIMER {
        let held = HELD.take();
        for (w, to) in held {
            if let Err(e) = set_pos(w, to) {
                log(&format!("placing {w:?} failed: {e}"));
            }
        }
        if HELD.with_borrow(Vec::is_empty) {
            unsafe {
                let _ = KillTimer(Some(EVENTS_TO.get()), id);
            }
        }
        return;
    }
    if id == REPLACE_TIMER {
        unsafe {
            let _ = KillTimer(Some(EVENTS_TO.get()), id);
        }
        update(|wm| Some(wm.placement()));
    }
}

fn cloaked(w: HWND) -> bool {
    let mut cloak = 0u32;
    let ok = unsafe { DwmGetWindowAttribute(w, DWMWA_CLOAKED, (&raw mut cloak).cast(), size_of::<u32>() as u32) };
    ok.is_ok() && cloak != 0
}

/// Shown, possibly minimized.
fn visible(w: HWND) -> bool {
    unsafe { IsWindowVisible(w) }.as_bool() && !cloaked(w)
}

fn on_screen(w: HWND) -> bool {
    visible(w) && !unsafe { IsIconic(w) }.as_bool()
}

/// What Windows' Alt+Tab lists: visible, unowned top-level windows with a
/// title that aren't tool windows, here not the shell's own.
fn app_window(w: HWND) -> bool {
    unsafe {
        if !visible(w) || GetAncestor(w, GA_ROOT) != w || GetWindowTextLengthW(w) == 0 {
            return false;
        }
        if GetWindow(w, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return false;
        }
        let ex = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(w, Some(&mut pid));
        pid != GetCurrentProcessId()
    }
}

/// Whether a visible window tiles (classify.rs); the rest float where they
/// open.
fn verdict(w: HWND) -> Verdict {
    unsafe {
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
        let mut pid = 0;
        GetWindowThreadProcessId(w, Some(&mut pid));
        let process = (pid != 0).then(|| OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()).flatten();
        let app = process.and_then(exe_stem);
        // As before window rules: without uiAccess, a process the shell can't
        // open is taken to be out of reach too.
        let out_of_reach = REACH.get() == Reach::SameLevel && process.is_none_or(is_elevated);
        if let Some(p) = process {
            let _ = CloseHandle(p);
        }
        let title = window_text(w);
        let class = class_name(w);
        let facts = Facts {
            root: GetAncestor(w, GA_ROOT) == w,
            owned: GetWindow(w, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()),
            style,
            ex_style,
            app: app.as_deref(),
            title: &title,
            class: &class,
            ours: pid == GetCurrentProcessId(),
            out_of_reach,
        };
        classify(&facts, &settings::get().tiling_exceptions)
    }
}

/// The exe's file name without `.exe`: what tiling exceptions match as the
/// app ID.
fn exe_stem(process: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    let name = path.rsplit('\\').next()?;
    Some(name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_owned())
}

fn window_text(w: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(w, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class_name(w: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(w, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// One line per window the shell sees appear and doesn't already manage,
/// with why it tiled or not, so a window that tiles wrongly can be told
/// apart from one the shell never heard of. Each window once.
fn log_verdict(w: HWND, verdict: Verdict) {
    if !verdict.worth_logging() || !LOGGED.with_borrow_mut(|seen| seen.insert(w.0 as usize)) {
        return;
    }
    unsafe {
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
        let owner = GetWindow(w, GW_OWNER).map(|o| o.0 as usize).unwrap_or(0);
        let mut pid = 0;
        GetWindowThreadProcessId(w, Some(&mut pid));
        let app = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().and_then(|p| {
            let name = exe_stem(p);
            let _ = CloseHandle(p);
            name
        });
        log(&format!(
            "window {:#x}: app {} class {:?} title {:?} style {style:#010x} exstyle {ex_style:#010x} owner {owner:#x}: {verdict}",
            w.0 as usize,
            app.as_deref().unwrap_or("?"),
            class_name(w),
            window_text(w),
        ));
    }
}

fn is_elevated(process: HANDLE) -> bool {
    token_flag(process, TokenElevation)
}

/// A token information class whose answer is a single DWORD flag
/// (TOKEN_ELEVATION is a struct of just one).
fn token_flag(process: HANDLE, class: TOKEN_INFORMATION_CLASS) -> bool {
    const { assert!(size_of::<TOKEN_ELEVATION>() == size_of::<u32>()) };
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut flag = 0u32;
        let mut len = 0;
        let ok = GetTokenInformation(token, class, Some((&raw mut flag).cast::<c_void>()), size_of::<u32>() as u32, &mut len);
        let _ = CloseHandle(token);
        ok.is_ok() && flag != 0
    }
}

fn apply(fx: Effects<HWND>) {
    let animating = animate(fx.pace, &fx.activated);
    unsafe {
        // Async throughout: a hung app must not hang the shell.
        for &w in &fx.show {
            let _ = ShowWindowAsync(w, SW_SHOWNA);
        }
        // Before placing: placing a minimized window only moves where it
        // would restore to, and restoring after would put it back there.
        for &w in &fx.restore {
            let _ = ShowWindowAsync(w, SW_RESTORE);
        }
        let mut crossed = false;
        for &(w, r) in &fx.place {
            crossed |= visible_frame(w).is_some_and(|(_, now)| monitor_of(now) != monitor_of(r));
            place(w, r);
        }
        for &(w, from, to) in &fx.carry {
            if let Some(r) = window_rect(w) {
                move_to(w, carried(r, from, to));
                crossed = true;
            }
        }
        // A Per-Monitor aware app moved onto a monitor with another DPI
        // resizes itself to what WM_DPICHANGED suggests, over where it was
        // put; so it's put there again once it has.
        if crossed {
            SetTimer(Some(EVENTS_TO.get()), REPLACE_TIMER, 250, None);
        }
        let follow = fx.follow.and_then(|w| {
            let tile = fx.place.iter().find(|&&(x, _)| x == w).map(|&(_, r)| r);
            Some(tile.or_else(|| visible_frame(w).map(|(_, r)| r))?.centre())
        });
        if let Some(p) = fx.pointer.or(follow) {
            let _ = SetCursorPos(p.x, p.y);
            // Not the user pointing at something: focus mustn't follow it.
            let under = window_under(p);
            FOLLOW.with_borrow_mut(|f| f.warped(under));
        }
        if animating {
            // Kept on screen for the overlay to draw, but under everything,
            // so a click mid-animation finds the windows that stay.
            for &w in &fx.hide {
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
                let _ = SetWindowPos(w, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
            }
            DEFERRED.with_borrow_mut(|d| d.extend(&fx.hide));
        } else {
            for &w in &fx.hide {
                let _ = ShowWindowAsync(w, SW_HIDE);
            }
        }
        if let Some(w) = fx.focus {
            focus(w);
        }
        if let Some(w) = fx.close {
            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn now() -> Duration {
    EPOCH.get().map(|e| e.elapsed()).unwrap_or_default()
}

/// COSMIC keeps a floating window's place relative to its output when it
/// moves it to another (`position_relative`).
fn carried(r: RECT, from: Rect, to: Rect) -> RECT {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let along = |at: i32, start: i32, len: i32, size: i32, to_start: i32, to_len: i32| {
        let room = (len - size).max(1);
        to_start + ((at - start).clamp(0, room) as i64 * (to_len - size).max(0) as i64 / room as i64) as i32
    };
    let left = along(r.left, from.left, from.width(), w, to.left, to.width());
    let top = along(r.top, from.top, from.height(), h, to.top, to.height());
    RECT { left, top, right: left + w, bottom: top + h }
}

/// Hands the window manager's new state to the animator and, if anything
/// on screen animates, puts the overlay up showing the screen as it is,
/// before anything moves. True while the overlay is up.
fn animate(pace: Pace, activated: &[(OutputId, usize)]) -> bool {
    let Some(screens) = WM.with_borrow(|wm| wm.as_ref().map(Wm::screens)) else { return false };
    let at = now();
    let running = PHASE.get() != Phase::Idle;
    let allowed = ANIMATING.get() && (running || (overlay::enabled() && !fullscreen_app()));
    ANIM.with_borrow_mut(|a| {
        let a = a.get_or_insert_with(|| Animator::new(screens.clone()));
        if allowed {
            a.push(screens, pace, &|w| visible_frame(w).map(|(_, r)| r), at);
            for (id, to) in activated {
                a.activate(id, *to, at);
            }
        } else {
            a.rest(screens);
        }
    });
    if !allowed {
        return false;
    }
    let frame = frame(at);
    if !running {
        if !frame.moving {
            return false;
        }
        draw(&frame);
        overlay::show(desktop_bounds());
        let shown = now();
        ANIM.with_borrow_mut(|a| a.as_mut().map(|a| a.restart(at, shown)));
    }
    PHASE.set(Phase::Running);
    true
}

/// A game in exclusive fullscreen, or any other app's window covering the
/// whole screen: the overlay mustn't go over those, or slow them down.
fn fullscreen_app() -> bool {
    if unsafe { SHQueryUserNotificationState() }.is_ok_and(|s| s == QUNS_RUNNING_D3D_FULL_SCREEN) {
        return true;
    }
    let w = unsafe { GetForegroundWindow() };
    if w.is_invalid() || WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| wm.manages(w))) || !on_screen(w) {
        return false;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(w, Some(&mut pid)) };
    let s = monitor_rect(w);
    pid != unsafe { GetCurrentProcessId() }
        && window_rect(w).is_some_and(|r| r.left <= s.left && r.top <= s.top && r.right >= s.right && r.bottom >= s.bottom)
}

/// The whole of the monitor `w` is on.
fn monitor_rect(w: HWND) -> Rect {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let m = unsafe { MonitorFromWindow(w, MONITOR_DEFAULTTONEAREST) };
    if unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
        let r = info.rcMonitor;
        Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    } else {
        desktop_bounds()
    }
}

/// Every top-level window, top first.
fn z_order() -> Vec<HWND> {
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM((&raw mut all) as isize));
    }
    all
}

/// The window's rect, and the part of it you see (Windows 10+ windows have
/// invisible resize borders), on screen.
fn visible_frame(w: HWND) -> Option<(RECT, Rect)> {
    let outer = window_rect(w)?;
    let mut v = RECT::default();
    let ok = unsafe { DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut v).cast(), size_of::<RECT>() as u32) };
    let v = if ok.is_ok() { v } else { outer };
    Some((outer, Rect { left: v.left, top: v.top, right: v.right, bottom: v.bottom }))
}

/// Whether every window placed has moved: COSMIC holds a tiling change back
/// (its TilingBlocker) until the apps have acked and drawn a buffer at their
/// new sizes, or 500 ms. Windows says nothing about when another process
/// has repainted, so a window counts as done once it's handled the move:
/// it's left where it was (an app with a minimum size may not end up
/// exactly where it was put), or it's gone.
fn settled() -> bool {
    PENDING.with_borrow_mut(|p| {
        p.retain(|&(w, from, to)| match window_rect(w) {
            Some(now) => now == from && now != to && unsafe { IsWindowVisible(w).as_bool() },
            None => false,
        });
        p.is_empty()
    })
}

fn frame(at: Duration) -> Scene<HWND> {
    let z = z_order();
    let floating = |id: &OutputId, i: usize| {
        let mut free = WM.with_borrow(|wm| wm.as_ref().map(|wm| wm.floating(id, i)).unwrap_or_default());
        free.retain(|&w| on_screen(w));
        free.sort_by_key(|w| z.iter().position(|x| x == w));
        free.into_iter().filter_map(|w| Some((w, visible_frame(w)?.1))).collect()
    };
    let settled = settled();
    ANIM.with_borrow_mut(|a| a.as_mut().map(|a| a.frame(at, settled, &floating))).unwrap_or(Scene { layers: Vec::new(), moving: false })
}

/// The overlay's picture: windows the shell doesn't manage (dialogs, the
/// launcher) where they are, on top as they'd be; under them, the animation,
/// each monitor's kept to that monitor. The overlay covers every monitor,
/// so screen coordinates are taken back to its corner.
fn draw(frame: &Scene<HWND>) {
    let overlay = overlay::window();
    let bounds = desktop_bounds();
    let mut scene: Vec<(HWND, Rect, f64, Option<Rect>)> = Vec::new();
    for w in z_order() {
        let managed = WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| wm.manages(w)));
        if w != overlay && !managed && on_screen(w)
            && let Some((_, r)) = visible_frame(w)
        {
            scene.push((w, r, 1.0, None));
        }
    }
    for layer in &frame.layers {
        scene.extend(layer.sprites.iter().map(|s| (s.window, s.rect, s.alpha, Some(layer.clip))));
    }
    let looks: Vec<(HWND, Look)> = scene
        .into_iter()
        .rev()
        .filter_map(|(w, at, alpha, clip)| {
            // A closed window has nothing left to draw, as in COSMIC.
            let (outer, seen) = visible_frame(w)?;
            let source = RECT {
                left: seen.left - outer.left,
                top: seen.top - outer.top,
                right: seen.right - outer.left,
                bottom: seen.bottom - outer.top,
            };
            let (at, source) = match clip {
                Some(clip) => clipped(at, source, clip)?,
                None => (at, source),
            };
            let at = Rect { left: at.left - bounds.left, top: at.top - bounds.top, right: at.right - bounds.left, bottom: at.bottom - bounds.top };
            Some((w, Look { at, alpha, source }))
        })
        .collect();
    overlay::draw(&looks);
}

/// The part of a thumbnail drawn at `at` that's inside `clip`, and the
/// part of its `source` that shows there; None if none of it is.
fn clipped(at: Rect, source: RECT, clip: Rect) -> Option<(Rect, RECT)> {
    let vis = Rect { left: at.left.max(clip.left), top: at.top.max(clip.top), right: at.right.min(clip.right), bottom: at.bottom.min(clip.bottom) };
    if vis.width() <= 0 || vis.height() <= 0 || at.width() <= 0 || at.height() <= 0 {
        return None;
    }
    if vis == at {
        return Some((at, source));
    }
    let scale = |d: i32, src: i32, dst: i32| (i64::from(d) * i64::from(src) / i64::from(dst)) as i32;
    let (sw, sh) = (source.right - source.left, source.bottom - source.top);
    let src = RECT {
        left: source.left + scale(vis.left - at.left, sw, at.width()),
        top: source.top + scale(vis.top - at.top, sh, at.height()),
        right: source.right - scale(at.right - vis.right, sw, at.width()),
        bottom: source.bottom - scale(at.bottom - vis.bottom, sh, at.height()),
    };
    Some((vis, src))
}

/// The pacing thread's tick: one composited frame went by.
pub fn on_frame() {
    overlay::frame_taken();
    let phase = PHASE.get();
    if phase == Phase::Idle {
        return;
    }
    let at = now();
    let frame = frame(at);
    draw(&frame);
    if frame.moving {
        PHASE.set(Phase::Running);
        return;
    }
    match phase {
        Phase::Running => {
            settle();
            if DEFERRED.with_borrow(Vec::is_empty) {
                finish();
            } else {
                PHASE.set(Phase::Settling { until: at + SETTLE });
            }
        }
        Phase::Settling { until } => {
            let hidden = DEFERRED.with_borrow(|d| d.iter().all(|&w| unsafe { !IsWindowVisible(w).as_bool() }));
            if hidden || at >= until {
                finish();
            }
        }
        Phase::Idle => {}
    }
}

/// Hides what slid or faded out, unless it's back on screen meanwhile (a
/// quick switch back to its workspace).
fn settle() {
    let gone: Vec<HWND> = DEFERRED.take();
    let gone: Vec<HWND> = WM.with_borrow(|wm| {
        let Some(wm) = wm else { return Vec::new() };
        gone.into_iter().filter(|&w| wm.manages(w) && !wm.is_shown(w)).collect()
    });
    for &w in &gone {
        unsafe {
            let _ = ShowWindowAsync(w, SW_HIDE);
        }
    }
    // A click mid-slide can land on a window that was leaving.
    let clicked_away = gone.contains(&unsafe { GetForegroundWindow() });
    DEFERRED.set(gone);
    if clicked_away {
        refocus();
    }
}

fn finish() {
    if PHASE.get() == Phase::Running {
        settle();
    }
    PHASE.set(Phase::Idle);
    DEFERRED.take();
    overlay::hide();
    sync_hint();
}

/// Stops everything where it ends up, at once.
fn cut() {
    let state = WM.with_borrow(|wm| wm.as_ref().map(Wm::screens));
    ANIM.with_borrow_mut(|a| {
        if let (Some(a), Some(screens)) = (a, state) {
            a.rest(screens);
        }
    });
    if PHASE.get() != Phase::Idle {
        finish();
    }
}

/// Moves `w` so its visible frame fills `r`. Windows 10+ windows have
/// invisible resize borders outside what you see, which SetWindowPos
/// counts, so they're added back on.
fn place(w: HWND, r: Rect) {
    unsafe {
        if IsZoomed(w).as_bool() {
            let _ = ShowWindowAsync(w, SW_RESTORE);
        }
        // Windows minimizes the windows that were on a monitor that goes
        // away, hidden ones included, without the shell hearing of it; the
        // tiling still has them, and placing a minimized window only moves
        // its stub. Not activated: focus is the Wm's to give.
        if IsIconic(w).as_bool() {
            let _ = ShowWindowAsync(w, SW_SHOWNOACTIVATE);
        }
        let mut outer = RECT::default();
        let mut visible = RECT::default();
        let (mut l, mut t, mut rr, mut b) = (0, 0, 0, 0);
        if GetWindowRect(w, &mut outer).is_ok()
            && DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut visible).cast(), size_of::<RECT>() as u32).is_ok()
        {
            (l, t, rr, b) = (visible.left - outer.left, visible.top - outer.top, outer.right - visible.right, outer.bottom - visible.bottom);
        }
        let to = RECT { left: r.left - l, top: r.top - t, right: r.right + rr, bottom: r.bottom + b };
        if outer != to {
            PENDING.with_borrow_mut(|p| {
                p.retain(|&(x, ..)| x != w);
                p.push((w, outer, to));
            });
        }
        if let Err(e) = set_pos(w, to) {
            log(&format!("placing {w:?} failed: {e}"));
        }
    }
}

/// SetWindowPos, one move in flight per window. Async, so a hung app can't
/// hang the shell, but then each move waits in the app's own queue: a
/// Super+drag resize sends one per pointer move, and a console, slow to
/// resize (it re-wraps its text), kept working through them after the
/// drag ended (saturn, 2026-11-20). A window still busy gets only the
/// newest place, sent once it has handled the last.
fn set_pos(w: HWND, to: RECT) -> windows::core::Result<()> {
    let busy = IN_FLIGHT.with_borrow_mut(|f| {
        f.retain(|&(x, from, sent)| x != w || (sent.elapsed() < IN_FLIGHT_FOR && window_rect(x) == Some(from)));
        f.iter().any(|&(x, ..)| x == w)
    });
    if busy {
        HELD.with_borrow_mut(|h| {
            h.retain(|&(x, _)| x != w);
            h.push((w, to));
        });
        unsafe { SetTimer(Some(EVENTS_TO.get()), FLUSH_TIMER, 8, None) };
        return Ok(());
    }
    HELD.with_borrow_mut(|h| h.retain(|&(x, _)| x != w));
    if let Some(from) = window_rect(w).filter(|&from| from != to) {
        IN_FLIGHT.with_borrow_mut(|f| f.push((w, from, Instant::now())));
    }
    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    unsafe { SetWindowPos(w, None, to.left, to.top, to.right - to.left, to.bottom - to.top, flags) }
}

/// Windows only lets the process that got the last input take the
/// foreground. The keys came through our hook but went to someone else,
/// so send ourselves an empty input first.
pub fn focus(w: HWND) {
    unsafe {
        let nudge = INPUT { r#type: INPUT_MOUSE, ..Default::default() };
        SendInput(&[nudge], size_of::<INPUT>() as i32);
        if !SetForegroundWindow(w).as_bool() {
            log(&format!("focusing {w:?} failed"));
        }
    }
}

fn window_rect(w: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(w, &mut r) }.ok().map(|()| r)
}

fn move_to(w: HWND, r: RECT) {
    PENDING.with_borrow_mut(|p| p.retain(|&(x, ..)| x != w));
    let _ = set_pos(w, r);
}

/// `r` moved by how far the pointer went since `from`.
fn shifted(r: RECT, from: Point, to: Point) -> RECT {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    RECT { left: r.left + dx, top: r.top + dy, right: r.right + dx, bottom: r.bottom + dy }
}

/// COSMIC's Super+drag: left moves the window under the pointer, right
/// resizes it from the corner nearest the pointer. A tile picked up with
/// left follows the pointer and drops back into the tiling where it's let
/// go; a tile resized with right moves the edge between it and its
/// neighbour on that side.
pub fn drag(d: Drag) {
    match d {
        Drag::Start(button, at) => {
            let w = unsafe { GetAncestor(WindowFromPoint(POINT { x: at.x, y: at.y }), GA_ROOT) };
            let Some(grab) = WM.with_borrow_mut(|wm| wm.as_mut().and_then(|wm| wm.grab(w))) else { return };
            let Some(start) = window_rect(w) else { return };
            // The overlay would show the window in its tile, not under the pointer.
            cut();
            focus(w);
            let centre = Point { x: (start.left + start.right) / 2, y: (start.top + start.bottom) / 2 };
            let corner = [
                if at.x < centre.x { Direction::Left } else { Direction::Right },
                if at.y < centre.y { Direction::Up } else { Direction::Down },
            ];
            let dragging = match (button, grab) {
                (Button::Left, Grab::Tiled) => Some(Dragging::Tile { w, from: at, start }),
                (Button::Left, Grab::Free) => Some(Dragging::Float { w, from: at, start }),
                (Button::Right, Grab::Tiled) => WM
                    .with_borrow(|wm| wm.as_ref().and_then(|wm| wm.edge(w, corner)))
                    .map(|(axis, edge)| Dragging::TileEdge { w, from: at, corner, axis, at: edge }),
                (Button::Right, Grab::Free) => Some(Dragging::FloatCorner { w, from: at, start, corner }),
            };
            DRAGGING.set(dragging);
            sync_hint();
        }
        Drag::Move(to) => match DRAGGING.get() {
            Some(Dragging::Tile { w, from, start }) => {
                move_to(w, shifted(start, from, to));
                let band = scaled_at(DROP_BAND, dpi_at(to));
                let target = WM.with_borrow(|wm| wm.as_ref().and_then(|wm| wm.drop_preview(w, to, band)));
                preview(target);
            }
            Some(Dragging::Float { w, from, start }) => move_to(w, shifted(start, from, to)),
            Some(Dragging::TileEdge { w, from, corner, axis, at }) => {
                let at = at + axis.coord(to) - axis.coord(from);
                update(|wm| Some(wm.move_edge(w, corner, at)));
            }
            Some(Dragging::FloatCorner { w, from, start, corner }) => {
                let (dx, dy) = (to.x - from.x, to.y - from.y);
                let min = scaled_at(100, dpi_at(to));
                let mut r = start;
                match corner[0] {
                    Direction::Left => r.left = (r.left + dx).min(r.right - min),
                    _ => r.right = (r.right + dx).max(r.left + min),
                }
                match corner[1] {
                    Direction::Up => r.top = (r.top + dy).min(r.bottom - min),
                    _ => r.bottom = (r.bottom + dy).max(r.top + min),
                }
                move_to(w, r);
            }
            None => {}
        },
        Drag::End(at) => {
            if let Some(Dragging::Tile { w, .. }) = DRAGGING.take() {
                preview(None);
                ANIM.with_borrow_mut(|a| a.as_mut().map(|a| a.forget(w)));
                let band = scaled_at(DROP_BAND, dpi_at(at));
                update(|wm| Some(wm.drop(w, at, band)));
            }
            sync_hint();
        }
    }
}

/// A see-through accent-coloured window over where a dragged tile would
/// land, like COSMIC's drop placeholder.
fn create_preview() -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = w!("DefenestrateDropPreview");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(preview_proc),
            hInstance: instance,
            lpszClassName: class,
            hbrBackground: CreateSolidBrush(PREVIEW_COLOUR),
            ..Default::default()
        });
        // Transparent to the mouse, so it's never what's under the pointer.
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        let window = CreateWindowExW(ex, class, w!("defenestrate drop preview"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None)?;
        SetLayeredWindowAttributes(window, COLORREF(0), PREVIEW_ALPHA, LWA_ALPHA)?;
        PREVIEW.set(window);
    }
    Ok(())
}

unsafe extern "system" fn preview_proc(w: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(w, msg, wparam, lparam) }
}

fn preview(at: Option<Rect>) {
    let window = PREVIEW.get();
    if window.is_invalid() {
        return;
    }
    unsafe {
        match at {
            Some(r) => {
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                let _ = SetWindowPos(window, Some(HWND_TOPMOST), r.left, r.top, r.width(), r.height(), flags);
            }
            None => {
                let _ = ShowWindowAsync(window, SW_HIDE);
            }
        }
    }
}

/// Tiled windows go without a title bar: the tiling is where they are, so
/// there's nothing to drag, and Super+Q closes them. Windows leaving the
/// tiling get theirs back. Taking or giving one back changes the window's
/// invisible borders, which `place` reads from DWM, and that only catches
/// up once the window has handled the change; so everything is placed again
/// a moment later.
fn sync_captions() {
    let tiled = WM.with_borrow(|wm| wm.as_ref().map(Wm::tiled).unwrap_or_default());
    let mut changed = false;
    for &w in &tiled {
        if strip_caption(w) {
            STRIPPED.with_borrow_mut(|s| s.push(w));
            changed = true;
        }
    }
    let gone: Vec<HWND> = STRIPPED.with_borrow_mut(|s| {
        let (keep, gone) = s.iter().partition(|w| tiled.contains(w));
        *s = keep;
        gone
    });
    for w in gone {
        changed |= restore_caption(w);
    }
    if changed {
        unsafe {
            SetTimer(Some(EVENTS_TO.get()), REPLACE_TIMER, 250, None);
        }
    }
}

/// True if `w` had a system title bar and now hasn't. Windows that draw
/// their own in the client area (Firefox, Chromium, Electron) keep it:
/// styles can't take that away, so they're left alone.
fn strip_caption(w: HWND) -> bool {
    unsafe {
        if !GetPropW(w, CAPTIONLESS).is_invalid() || draws_own_title_bar(w) {
            return false;
        }
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        // Changing another process's window's style waits on its thread.
        // A caption is enough: fixed-size windows (League's game, windowed)
        // have no sizing border, and requiring both left them their title
        // bar (saturn, 2026-11-20).
        if style & WS_CAPTION.0 != WS_CAPTION.0 || IsHungAppWindow(w).as_bool() {
            return false;
        }
        // What's taken is kept on the window, so exactly that comes back.
        let taken = style & TITLE_BAR;
        if SetPropW(w, CAPTIONLESS, Some(HANDLE(taken as usize as *mut c_void))).is_err() {
            log(&format!("couldn't mark {w:?} as captionless; leaving its title bar"));
            return false;
        }
        SetWindowLongPtrW(w, GWL_STYLE, (style & !taken) as isize);
        frame_changed(w);
        true
    }
}

/// Gives `w` back the title bar the shell took, if it took one: true if so.
fn restore_caption(w: HWND) -> bool {
    unsafe {
        let taken = GetPropW(w, CAPTIONLESS).0 as usize as u32 & TITLE_BAR;
        if RemovePropW(w, CAPTIONLESS).is_err() {
            return false;
        }
        // Shells from before kept a plain 1 here, having taken both.
        let taken = if taken == 0 { TITLE_BAR } else { taken };
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        SetWindowLongPtrW(w, GWL_STYLE, (style | taken) as isize);
        frame_changed(w);
        true
    }
}

fn frame_changed(w: HWND) {
    let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    unsafe {
        let _ = SetWindowPos(w, None, 0, 0, 0, 0, flags);
    }
}

/// A window whose client area reaches the top of its visible frame has no
/// system title bar to take: it draws its own.
fn draws_own_title_bar(w: HWND) -> bool {
    unsafe {
        let mut visible = RECT::default();
        let mut client = RECT::default();
        let mut origin = POINT::default();
        if DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut visible).cast(), size_of::<RECT>() as u32).is_err()
            || GetClientRect(w, &mut client).is_err()
            || !ClientToScreen(w, &mut origin).as_bool()
        {
            return false;
        }
        origin.y <= visible.top + 1
    }
}
