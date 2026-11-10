//! Draws motion.rs's frames. COSMIC animates textures it composites itself;
//! here the windows belong to other processes, and moving one every frame
//! would make its app lay out and repaint every frame. So the real windows
//! go straight to where they end up, and a topmost window of ours covers
//! the screen meanwhile, showing DWM thumbnails of them where the animation
//! has them: DWM scales live window contents into any rectangle, in its
//! own process, with nothing loaded into anyone else's. The overlay is
//! layered and click-through and never activates, so input lands on the
//! real windows, already in their final places.
//!
//! Frames are paced by DwmFlush on a thread of their own, which only runs
//! while something animates: an idle shell has no timer and no thread
//! awake.
//!
//! Where Windows can't do what COSMIC does:
//! - COSMIC stretches a client's old buffer until it commits one at the new
//!   size. A DWM thumbnail is the window as it is now, already resized, so
//!   an app slow to repaint shows its unpainted part for those frames.
//! - COSMIC maps a new window only once it's tiled. Here it is already on
//!   screen, where its app put it, by the time the shell hears of it, so it
//!   shows there for a moment before the overlay fades it in at its tile.
//! - Whether an app has repainted isn't observable, so the TilingBlocker
//!   wait (motion.rs) ends when the windows have moved, not when they've
//!   drawn.
//! - Where windows part mid-animation, COSMIC shows the wallpaper. Here
//!   it's the desktop's colour and wallpaper as PaintDesktop draws them.
//!   Without explorer DWM shows something else behind the windows (a dark
//!   grey in the VM, where PaintDesktop gives black), and it won't
//!   thumbnail the desktop window (E_INVALIDARG), so that can't be copied.
//! - Nothing is drawn over a fullscreen game, and "Animation effects" off
//!   in Windows' settings means no animations; COSMIC has neither check.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, FILETIME, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY,
    DWM_TNP_VISIBLE, DwmFlush, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_DESKTOP, EndPaint, FillRect, GetSysColor, GetSysColorBrush, PAINTSTRUCT, PaintDesktop, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, LWA_ALPHA, PostMessageW, RegisterClassW, SPI_GETCLIENTAREAANIMATION,
    SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, SystemParametersInfoW,
    WM_APP, WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::core::{BOOL, w};

use crate::log;
use crate::tree::Rect;

/// Posted to the shell's window once per composited frame while animating.
pub const WM_FRAME: u32 = WM_APP + 3;

thread_local! {
    static OVERLAY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// In registration order, which DWM draws bottom first. None where the
    /// window couldn't have one (it's gone), so the order still lines up.
    static THUMBS: RefCell<Vec<(HWND, Option<isize>)>> = const { RefCell::new(Vec::new()) };
    static STATS: RefCell<Option<Stats>> = const { RefCell::new(None) };
    static PAINTED: Cell<bool> = const { Cell::new(false) };
}

static TICKING: Mutex<bool> = Mutex::new(false);
static WAKE: Condvar = Condvar::new();
/// A frame is posted and not yet drawn: a slow GUI thread gets the latest
/// moment, not a backlog.
static POSTED: AtomicBool = AtomicBool::new(false);

pub fn create(notify: HWND) -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = w!("DefenestrateAnimation");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class, ..Default::default() });
        // Layered and transparent: the mouse goes through to the real
        // windows, and WindowFromPoint never returns it.
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        let window = CreateWindowExW(ex, class, w!("defenestrate animation"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None)?;
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
        OVERLAY.set(window);
    }
    if stats_wanted() {
        STATS.set(Some(Stats::now()));
    }
    let notify = notify.0 as isize;
    std::thread::spawn(move || pace(HWND(notify as *mut _)));
    Ok(())
}

pub fn window() -> HWND {
    OVERLAY.get()
}

unsafe extern "system" fn proc(w: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        // Where windows part mid-animation, what the bare desktop shows: its
        // colour, and the wallpaper if there is one. The overlay sits at the
        // screen's origin, so that lines up.
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(w, &mut ps);
            FillRect(dc, &ps.rcPaint, GetSysColorBrush(COLOR_DESKTOP));
            let wallpaper = PaintDesktop(dc).as_bool();
            let _ = EndPaint(w, &ps);
            if !PAINTED.replace(true) {
                log(&format!("overlay background: desktop colour {:06x}, wallpaper painted: {wallpaper}", GetSysColor(COLOR_DESKTOP)));
            }
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(w, msg, wparam, lparam) },
    }
}

fn pace(notify: HWND) {
    loop {
        {
            let Ok(mut ticking) = TICKING.lock() else { return };
            while !*ticking {
                let Ok(t) = WAKE.wait(ticking) else { return };
                ticking = t;
            }
        }
        let start = Instant::now();
        // Returns at the next composition. With nothing for DWM to
        // compose it may return at once; don't spin on that.
        if unsafe { DwmFlush() }.is_err() || start.elapsed() < Duration::from_millis(2) {
            std::thread::sleep(Duration::from_millis(8));
        }
        if !POSTED.swap(true, Ordering::AcqRel) {
            unsafe {
                let _ = PostMessageW(Some(notify), WM_FRAME, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn frames(on: bool) {
    if let Ok(mut ticking) = TICKING.lock() {
        *ticking = on;
        WAKE.notify_one();
    }
}

/// The posted frame is being drawn; the next may be posted.
pub fn frame_taken() {
    POSTED.store(false, Ordering::Release);
    STATS.with_borrow_mut(|s| {
        if let Some(s) = s {
            s.frames += 1;
        }
    });
}

/// The user's "Animation effects" (Settings > Accessibility > Visual
/// effects), read each time so flipping it takes effect at once.
pub fn enabled() -> bool {
    if OVERLAY.get().is_invalid() {
        return false;
    }
    let mut on = BOOL(1);
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some((&raw mut on).cast()), Default::default()) };
    ok.is_err() || on.as_bool()
}

/// What a thumbnail shows: `source`, the part of the window you see,
/// relative to its outer rect, drawn at `at` on screen.
pub struct Look {
    pub at: Rect,
    pub alpha: f64,
    pub source: RECT,
}

/// One thumbnail per entry, bottom first; a window may be in it twice.
/// Thumbnails that are already in the right order are kept, so a frame that
/// only moves things re-registers nothing.
pub fn draw(scene: &[(HWND, Look)]) {
    let overlay = OVERLAY.get();
    THUMBS.with_borrow_mut(|thumbs| {
        let keep = thumbs.iter().zip(scene).take_while(|((a, _), (b, _))| a == b).count();
        for (_, t) in thumbs.drain(keep..) {
            if let Some(t) = t {
                let _ = unsafe { DwmUnregisterThumbnail(t) };
            }
        }
        for &(w, _) in &scene[keep..] {
            thumbs.push((w, unsafe { DwmRegisterThumbnail(overlay, w) }.ok()));
        }
        for (&(_, t), (_, look)) in thumbs.iter().zip(scene) {
            let Some(t) = t else { continue };
            let Look { at, alpha, source } = *look;
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_OPACITY | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT { left: at.left, top: at.top, right: at.right, bottom: at.bottom },
                rcSource: source,
                opacity: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
                // A fully transparent one is still drawn: don't bother DWM.
                fVisible: (alpha > 0.0 && at.width() > 0 && at.height() > 0).into(),
                fSourceClientAreaOnly: false.into(),
            };
            let _ = unsafe { DwmUpdateThumbnailProperties(t, &props) };
        }
    });
}

/// Puts the overlay, drawn already, over `area` and waits until DWM has
/// composed it, so windows moved after this returns are never seen jumping.
pub fn show(area: Rect) {
    let overlay = OVERLAY.get();
    STATS.with_borrow_mut(|s| {
        if let Some(s) = s {
            s.started();
        }
    });
    unsafe {
        let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        let _ = SetWindowPos(overlay, Some(HWND_TOPMOST), area.left, area.top, area.width(), area.height(), flags);
        let _ = UpdateWindow(overlay);
        let _ = DwmFlush();
    }
    frames(true);
}

pub fn hide() {
    frames(false);
    unsafe {
        let _ = ShowWindow(OVERLAY.get(), SW_HIDE);
    }
    for (_, t) in THUMBS.take() {
        if let Some(t) = t {
            let _ = unsafe { DwmUnregisterThumbnail(t) };
        }
    }
    STATS.with_borrow_mut(|s| {
        if let Some(s) = s {
            s.ended();
        }
    });
}

/// Logging what animating costs, and what the shell spends between
/// animations, is on while `%LOCALAPPDATA%\defenestrate\anim-stats` exists
/// when the shell starts.
fn stats_wanted() -> bool {
    std::env::var_os("LOCALAPPDATA")
        .is_some_and(|d| std::path::PathBuf::from(d).join("defenestrate").join("anim-stats").exists())
}

fn ticks(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

fn cpu() -> u64 {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    ticks(k) + ticks(u)
}

/// The shell's own CPU time, in 100 ns ticks, from one mark to the next,
/// and how many frames it drew.
struct Stats {
    at: Instant,
    cpu: u64,
    frames: u32,
}

impl Stats {
    fn now() -> Self {
        Stats { at: Instant::now(), cpu: cpu(), frames: 0 }
    }

    fn report(&mut self, what: &str) {
        let then = std::mem::replace(self, Stats::now());
        log(&format!(
            "{what}: {} ms, shell cpu {:.1} ms, {} frames drawn",
            then.at.elapsed().as_millis(),
            (self.cpu - then.cpu) as f64 / 10_000.0,
            then.frames,
        ));
    }

    fn started(&mut self) {
        self.report("idle");
    }

    fn ended(&mut self) {
        self.report("animation");
    }
}
