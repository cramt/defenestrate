//! The window manager's hands and eyes: WinEvent hooks feed `Wm` the
//! windows that come and go, and `apply` makes the screen match the
//! `Effects` it returns. Runs entirely on the shell's one GUI thread.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EnumWindows, GA_ROOT, GW_OWNER,
    GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetSystemMetrics, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, PostMessageW, SM_CXSCREEN, SM_CYSCREEN, SPI_SETWORKAREA,
    SW_HIDE, SW_RESTORE, SW_SHOWNA, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindowAsync, SystemParametersInfoW, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_THICKFRAME,
};
use windows::core::BOOL;

use crate::tree::Rect;
use crate::log;
use crate::wm::{Command, Effects, Wm};

/// Posted to the shell's window for every WinEvent: wparam the event,
/// lparam the window. Hook callbacks only post, so all state changes
/// happen in one place, from the message loop.
pub const WM_WINEVENT: u32 = WM_APP + 1;

thread_local! {
    static WM: RefCell<Option<Wm<HWND>>> = const { RefCell::new(None) };
    static EVENTS_TO: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static ELEVATED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` on the state and applies what it returns, with the state no
/// longer borrowed: moving, showing and focusing windows pumps messages,
/// and those can land back here.
fn update(f: impl FnOnce(&mut Wm<HWND>) -> Option<Effects<HWND>>) {
    let fx = WM.with_borrow_mut(|wm| wm.as_mut().and_then(f));
    if let Some(fx) = fx {
        apply(fx);
    }
}

pub fn start(events_to: HWND, gap: i32) {
    EVENTS_TO.set(events_to);
    ELEVATED.set(is_elevated(unsafe { GetCurrentProcess() }));
    let area = screen();
    // A shell that sets the work area owns it; the previous shell (or the
    // bar this one used to have) may have left part of it reserved. No
    // SPIF_SENDCHANGE: that broadcasts WM_SETTINGCHANGE and waits on every
    // window, and on a first boot a busy installer stalled it long enough
    // that the shell never got as far as its keyboard hook.
    unsafe {
        let mut full = RECT { left: area.left, top: area.top, right: area.right, bottom: area.bottom };
        if let Err(e) = SystemParametersInfoW(SPI_SETWORKAREA, 0, Some((&raw mut full).cast()), Default::default()) {
            log(&format!("resetting the work area failed: {e}"));
        }
    }
    WM.set(Some(Wm::new(area, gap)));

    let mut existing: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM((&raw mut existing) as isize));
    }
    // EnumWindows goes top of the z-order down; adding bottom-up leaves
    // the topmost window focused, as it was.
    for w in existing.into_iter().rev() {
        if manageable(w) {
            update(|wm| Some(wm.add(w)));
        }
    }

    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    // Narrow ranges: one spanning everything would wake the shell for
    // every cursor move (EVENT_OBJECT_LOCATIONCHANGE).
    for (min, max) in [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
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
    update(|wm| match event {
        EVENT_OBJECT_DESTROY => Some(wm.remove(w)),
        EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED | EVENT_SYSTEM_MINIMIZEEND => {
            (!wm.manages(w) && manageable(w)).then(|| wm.add(w))
        }
        // Managed windows on other workspaces are hidden by us; only a
        // window on screen going invisible has really gone. Checked now
        // rather than trusted from the event: by the time it arrives, a
        // quick switch back may have shown the window again.
        EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED | EVENT_SYSTEM_MINIMIZESTART => {
            (wm.is_shown(w) && !on_screen(w)).then(|| wm.remove(w))
        }
        EVENT_SYSTEM_FOREGROUND => {
            wm.focused(w);
            None
        }
        _ => None,
    });
}

pub fn command(c: Command) {
    update(|wm| Some(wm.command(c)));
}

pub fn screen_changed() {
    let area = screen();
    update(|wm| Some(wm.set_area(area)));
}

/// Gives focus back to the workspace's focused window, e.g. after the
/// launcher is dismissed.
pub fn refocus() {
    update(|wm| Some(Effects { focus: wm.focused_window(), ..Effects::default() }));
}

/// Shows everything this hid, so no window is lost if the shell exits.
pub fn release() {
    let hidden = WM.with_borrow(|wm| wm.as_ref().map(Wm::hidden).unwrap_or_default());
    for w in hidden {
        unsafe {
            let _ = ShowWindowAsync(w, SW_SHOWNA);
        }
    }
}

/// The primary monitor. Others aren't managed yet.
fn screen() -> Rect {
    unsafe { Rect { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) } }
}

fn cloaked(w: HWND) -> bool {
    let mut cloak = 0u32;
    let ok = unsafe { DwmGetWindowAttribute(w, DWMWA_CLOAKED, (&raw mut cloak).cast(), size_of::<u32>() as u32) };
    ok.is_ok() && cloak != 0
}

fn on_screen(w: HWND) -> bool {
    let shown = unsafe { IsWindowVisible(w).as_bool() && !IsIconic(w).as_bool() };
    shown && !cloaked(w)
}

/// Windows that tile: real, resizable, unowned app windows. Dialogs,
/// popups and tool windows float where they open.
fn manageable(w: HWND) -> bool {
    unsafe {
        if !on_screen(w) || GetAncestor(w, GA_ROOT) != w {
            return false;
        }
        if GetWindow(w, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return false;
        }
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        let ex = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32;
        if style & WS_THICKFRAME.0 == 0 || style & WS_CHILD.0 != 0 || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return false;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(w, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return false;
        }
        // Windows won't let a normal process move an elevated one's
        // windows (UIPI), so those would sit on top of the tiles.
        if !ELEVATED.get() {
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
            let elevated = is_elevated(process);
            let _ = CloseHandle(process);
            if elevated {
                return false;
            }
        }
        true
    }
}

fn is_elevated(process: HANDLE) -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast::<c_void>()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

fn apply(fx: Effects<HWND>) {
    unsafe {
        // Async throughout: a hung app must not hang the shell.
        for &w in &fx.show {
            let _ = ShowWindowAsync(w, SW_SHOWNA);
        }
        for &(w, r) in &fx.place {
            place(w, r);
        }
        for &w in &fx.hide {
            let _ = ShowWindowAsync(w, SW_HIDE);
        }
        if let Some(w) = fx.focus {
            focus(w);
        }
        if let Some(w) = fx.close {
            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
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
        let mut outer = RECT::default();
        let mut visible = RECT::default();
        let (mut l, mut t, mut rr, mut b) = (0, 0, 0, 0);
        if GetWindowRect(w, &mut outer).is_ok()
            && DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut visible).cast(), size_of::<RECT>() as u32).is_ok()
        {
            (l, t, rr, b) = (visible.left - outer.left, visible.top - outer.top, outer.right - visible.right, outer.bottom - visible.bottom);
        }
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
        if let Err(e) = SetWindowPos(w, None, r.left - l, r.top - t, r.width() + l + rr, r.height() + t + b, flags) {
            log(&format!("placing {w:?} failed: {e}"));
        }
    }
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
