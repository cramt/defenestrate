//! The window manager's hands and eyes: WinEvent hooks feed `Wm` the
//! windows that come and go, and `apply` makes the screen match the
//! `Effects` it returns. Super+drags (keys.rs) are carried out here too.
//! Runs entirely on the shell's one GUI thread.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, COLORREF, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateSolidBrush};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EnumWindows, GA_ROOT,
    GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClientRect, GetPropW, GetSystemMetrics, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, HWND_TOPMOST, IsHungAppWindow, IsIconic, IsWindowVisible, IsZoomed, KillTimer,
    LWA_ALPHA, PostMessageW, RegisterClassW, RemovePropW, SM_CXSCREEN, SM_CYSCREEN, SPI_SETWORKAREA, SW_HIDE, SW_RESTORE,
    SW_SHOWNA, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow, SetLayeredWindowAttributes, SetPropW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindowAsync, SystemParametersInfoW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE,
    WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP, WS_THICKFRAME, WindowFromPoint,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::keys::{Button, Drag};
use crate::tree::{Axis, Direction, Point, Rect};
use crate::wm::{Command, Effects, Grab, Wm};
use crate::{log, scaled};

/// Posted to the shell's window for every WinEvent: wparam the event,
/// lparam the window. Hook callbacks only post, so all state changes
/// happen in one place, from the message loop.
pub const WM_WINEVENT: u32 = WM_APP + 1;
/// The shell's window gets this WM_TIMER id when the windows whose title
/// bars came or went should be placed again (see `sync_captions`).
pub const REPLACE_TIMER: usize = 0xdf1;

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

thread_local! {
    static WM: RefCell<Option<Wm<HWND>>> = const { RefCell::new(None) };
    static EVENTS_TO: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static ELEVATED: Cell<bool> = const { Cell::new(false) };
    static DRAGGING: Cell<Option<Dragging>> = const { Cell::new(None) };
    static PREVIEW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Windows the shell took the title bar from.
    static STRIPPED: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

/// Runs `f` on the state and applies what it returns, with the state no
/// longer borrowed: moving, showing and focusing windows pumps messages,
/// and those can land back here.
fn update(f: impl FnOnce(&mut Wm<HWND>) -> Option<Effects<HWND>>) {
    let fx = WM.with_borrow_mut(|wm| wm.as_mut().and_then(f));
    if let Some(fx) = fx {
        apply(fx);
        sync_captions();
    }
}

pub fn start(events_to: HWND) {
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
    WM.set(Some(Wm::new(area)));
    if let Err(e) = create_preview() {
        log(&format!("creating the drop preview failed: {e}"));
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

/// Shows everything this hid and gives back every title bar, so no window
/// is lost or stuck without one if the shell exits.
pub fn release() {
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

pub fn on_timer(id: usize) {
    if id == REPLACE_TIMER {
        unsafe {
            let _ = KillTimer(Some(EVENTS_TO.get()), id);
        }
        update(|wm| Some(wm.placement()));
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

fn window_rect(w: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(w, &mut r) }.ok().map(|()| r)
}

fn move_to(w: HWND, r: RECT) {
    let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
    unsafe {
        let _ = SetWindowPos(w, None, r.left, r.top, r.right - r.left, r.bottom - r.top, flags);
    }
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
        }
        Drag::Move(to) => match DRAGGING.get() {
            Some(Dragging::Tile { w, from, start }) => {
                move_to(w, shifted(start, from, to));
                let target = WM.with_borrow(|wm| wm.as_ref().and_then(|wm| wm.drop_preview(w, to, scaled(DROP_BAND))));
                preview(target);
            }
            Some(Dragging::Float { w, from, start }) => move_to(w, shifted(start, from, to)),
            Some(Dragging::TileEdge { w, from, corner, axis, at }) => {
                let at = at + axis.coord(to) - axis.coord(from);
                update(|wm| Some(wm.move_edge(w, corner, at)));
            }
            Some(Dragging::FloatCorner { w, from, start, corner }) => {
                let (dx, dy) = (to.x - from.x, to.y - from.y);
                let min = scaled(100);
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
                update(|wm| Some(wm.drop(w, at, scaled(DROP_BAND))));
            }
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
        if style & TITLE_BAR != TITLE_BAR || IsHungAppWindow(w).as_bool() {
            return false;
        }
        if SetPropW(w, CAPTIONLESS, Some(HANDLE(1 as *mut c_void))).is_err() {
            return false;
        }
        SetWindowLongPtrW(w, GWL_STYLE, (style & !TITLE_BAR) as isize);
        frame_changed(w);
        true
    }
}

/// Gives `w` back the title bar the shell took, if it took one: true if so.
fn restore_caption(w: HWND) -> bool {
    unsafe {
        if RemovePropW(w, CAPTIONLESS).is_err() {
            return false;
        }
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        SetWindowLongPtrW(w, GWL_STYLE, (style | TITLE_BAR) as isize);
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
