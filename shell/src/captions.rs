//! Taking a tiled window's title bar and giving it back: the tiling is
//! where a tile is, so there's nothing to drag, and Super+Q closes it.
//! desktop.rs decides which windows (`sync_captions`).

use std::ffi::c_void;

use windows::Win32::Foundation::{HANDLE, HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClientRect, GetPropW, GetWindowLongPtrW, IsHungAppWindow, RemovePropW, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetPropW, SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_THICKFRAME,
};
use windows::core::{PCWSTR, w};

use crate::log;

/// Marks a window whose title bar the shell took, on the window itself, so
/// a shell that crashed can give it back when it starts again.
const CAPTIONLESS: PCWSTR = w!("defenestrate.captionless");
/// What a tiled window loses. The sizing border goes with the caption:
/// without a caption, Windows 11 paints the top sizing border as a white
/// strip across the window.
const TITLE_BAR: u32 = WS_CAPTION.0 | WS_THICKFRAME.0;

/// True if `w` had a system title bar and now hasn't. Windows that draw
/// their own in the client area (Firefox, Chromium, Electron) keep it:
/// styles can't take that away, so they're left alone.
pub fn strip(w: HWND) -> bool {
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
pub fn restore(w: HWND) -> bool {
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

/// `w`'s style as its app has it: what the shell took put back, so taking
/// the title bar doesn't read as the app changing its style (classify::Fit).
pub fn own_style(w: HWND) -> u32 {
    unsafe {
        let style = GetWindowLongPtrW(w, GWL_STYLE) as u32;
        let marked = GetPropW(w, CAPTIONLESS);
        if marked.is_invalid() {
            return style;
        }
        let taken = marked.0 as usize as u32 & TITLE_BAR;
        style | if taken == 0 { TITLE_BAR } else { taken }
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
