//! A see-through accent-coloured window over where a dragged tile would
//! land, like COSMIC's drop placeholder.

use std::cell::Cell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{DefWindowProcW, HWND_TOPMOST, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindowAsync};
use windows::core::w;

use crate::overlay::click_through;
use crate::tree::Rect;

// COLORREF is 0x00BBGGRR: COSMIC's default accent blue.
const COLOUR: COLORREF = COLORREF(0x00E4_B163);
const ALPHA: u8 = 96;

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

pub fn create() -> windows::core::Result<()> {
    WINDOW.set(click_through(w!("DefenestrateDropPreview"), w!("defenestrate drop preview"), Some(proc_), Some(COLOUR), ALPHA)?);
    Ok(())
}

unsafe extern "system" fn proc_(w: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(w, msg, wparam, lparam) }
}

/// Over `at`; None hides it.
pub fn show(at: Option<Rect>) {
    let window = WINDOW.get();
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
