//! The window manager's hands and eyes: WinEvent hooks feed `Wm` the
//! windows that come and go, and `apply` makes the screen match the
//! `Effects` it returns. Super+drags (keys.rs) are carried out here too.
//! Runs entirely on the shell's one GUI thread.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, COLORREF, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateSolidBrush};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_MOUSE, SendInput};
use windows::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EnumWindows, GA_ROOT,
    GetForegroundWindow, HWND_BOTTOM,
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
use crate::motion::{Animator, Frame, Pace, Sprite};
use crate::overlay::{self, Look};
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
        if manageable(w) {
            update(|wm| Some(wm.add(w)));
        }
    }
    ANIMATING.set(true);

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
    // A game going fullscreen mid-animation gets the screen to itself at once.
    if event == EVENT_SYSTEM_FOREGROUND && PHASE.get() != Phase::Idle && fullscreen_app() {
        cut();
    }
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
    let animating = animate(fx.pace, fx.activated);
    unsafe {
        // Async throughout: a hung app must not hang the shell.
        for &w in &fx.show {
            let _ = ShowWindowAsync(w, SW_SHOWNA);
        }
        for &(w, r) in &fx.place {
            place(w, r);
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

/// Hands the window manager's new state to the animator and, if anything
/// on screen animates, puts the overlay up showing the screen as it is,
/// before anything moves. True while the overlay is up.
fn animate(pace: Pace, activated: Option<usize>) -> bool {
    let Some((views, active)) = WM.with_borrow(|wm| wm.as_ref().map(|wm| (wm.views(), wm.active()))) else { return false };
    let at = now();
    let running = PHASE.get() != Phase::Idle;
    let allowed = ANIMATING.get() && (running || (overlay::enabled() && !fullscreen_app()));
    ANIM.with_borrow_mut(|a| {
        let a = a.get_or_insert_with(|| Animator::new(views.clone(), active));
        if allowed {
            a.push(views, pace, &|w| visible_frame(w).map(|(_, r)| r), at);
            if let Some(to) = activated {
                a.activate(to, at);
            }
        } else {
            a.rest(views, active);
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
        draw(&frame.sprites);
        overlay::show(screen());
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
    let s = screen();
    pid != unsafe { GetCurrentProcessId() }
        && window_rect(w).is_some_and(|r| r.left <= s.left && r.top <= s.top && r.right >= s.right && r.bottom >= s.bottom)
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

fn frame(at: Duration) -> Frame<HWND> {
    let z = z_order();
    let floating = |i: usize| {
        let mut free = WM.with_borrow(|wm| wm.as_ref().map(|wm| wm.floating(i)).unwrap_or_default());
        free.retain(|&w| on_screen(w));
        free.sort_by_key(|w| z.iter().position(|x| x == w));
        free.into_iter().filter_map(|w| Some((w, visible_frame(w)?.1))).collect()
    };
    let settled = settled();
    ANIM.with_borrow_mut(|a| a.as_mut().map(|a| a.frame(at, settled, &floating))).unwrap_or(Frame { sprites: Vec::new(), moving: false })
}

/// The overlay's picture: windows the shell doesn't manage (dialogs, the
/// launcher) where they are, on top as they'd be; under them, the animation.
fn draw(sprites: &[Sprite<HWND>]) {
    let overlay = overlay::window();
    let mut scene: Vec<(HWND, Rect, f64)> = Vec::new();
    for w in z_order() {
        let managed = WM.with_borrow(|wm| wm.as_ref().is_some_and(|wm| wm.manages(w)));
        if w != overlay && !managed && on_screen(w)
            && let Some((_, r)) = visible_frame(w)
        {
            scene.push((w, r, 1.0));
        }
    }
    scene.extend(sprites.iter().map(|s| (s.window, s.rect, s.alpha)));
    let looks: Vec<(HWND, Look)> = scene
        .into_iter()
        .rev()
        .filter_map(|(w, at, alpha)| {
            // A closed window has nothing left to draw, as in COSMIC.
            let (outer, seen) = visible_frame(w)?;
            let source = RECT {
                left: seen.left - outer.left,
                top: seen.top - outer.top,
                right: seen.right - outer.left,
                bottom: seen.bottom - outer.top,
            };
            Some((w, Look { at, alpha, source }))
        })
        .collect();
    overlay::draw(&looks);
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
    draw(&frame.sprites);
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
}

/// Stops everything where it ends up, at once.
fn cut() {
    let state = WM.with_borrow(|wm| wm.as_ref().map(|wm| (wm.views(), wm.active())));
    ANIM.with_borrow_mut(|a| {
        if let (Some(a), Some((views, active))) = (a, state) {
            a.rest(views, active);
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
        let mut outer = RECT::default();
        let mut visible = RECT::default();
        let (mut l, mut t, mut rr, mut b) = (0, 0, 0, 0);
        if GetWindowRect(w, &mut outer).is_ok()
            && DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut visible).cast(), size_of::<RECT>() as u32).is_ok()
        {
            (l, t, rr, b) = (visible.left - outer.left, visible.top - outer.top, outer.right - visible.right, outer.bottom - visible.bottom);
        }
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
        let to = RECT { left: r.left - l, top: r.top - t, right: r.right + rr, bottom: r.bottom + b };
        if outer != to {
            PENDING.with_borrow_mut(|p| {
                p.retain(|&(x, ..)| x != w);
                p.push((w, outer, to));
            });
        }
        if let Err(e) = SetWindowPos(w, None, to.left, to.top, to.right - to.left, to.bottom - to.top, flags) {
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
    PENDING.with_borrow_mut(|p| p.retain(|&(x, ..)| x != w));
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
                ANIM.with_borrow_mut(|a| a.as_mut().map(|a| a.forget(w)));
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
