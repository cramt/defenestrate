//! COSMIC's active hint: a border in the accent colour just outside the
//! focused window, `active_hint` pixels thick (cosmic-comp's
//! IndicatorShader::focus_element). Here it's one topmost window of ours,
//! cut down to a frame by a window region and filled by its class brush:
//! layered and click-through like the animation overlay, it never takes
//! input, and nothing redraws it but moving it, so an idle shell spends
//! nothing on it.
//!
//! COSMIC draws it with the window, so it's always where the window is.
//! Here it follows what the shell knows: a tile's place, or where a floating
//! window was when it got focus or a move of it ended. A floating window an
//! app moves by itself leaves it behind until the next focus change.

use std::cell::Cell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRectRgn, CreateSolidBrush, DeleteObject, RGN_DIFF, SetWindowRgn};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_TOPMOST, LWA_ALPHA, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindowAsync, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::settings::ActiveHint;
use crate::tree::Rect;

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// Where it's shown, so the same place twice costs nothing.
    static AT: Cell<Option<Rect>> = const { Cell::new(None) };
    static THICKNESS: Cell<i32> = const { Cell::new(0) };
}

/// `thickness` in real pixels.
pub fn create(hint: ActiveHint, thickness: i32) -> windows::core::Result<()> {
    let [r, g, b] = hint.colour;
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = w!("DefenestrateActiveHint");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            lpszClassName: class,
            // COLORREF is 0x00BBGGRR.
            hbrBackground: CreateSolidBrush(COLORREF(u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16)),
            ..Default::default()
        });
        let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
        let window = CreateWindowExW(ex, class, w!("defenestrate active hint"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None)?;
        // Opaque, but layered: only a layered window is click-through.
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
        WINDOW.set(window);
    }
    THICKNESS.set(thickness.max(1));
    Ok(())
}

unsafe extern "system" fn proc_(w: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(w, msg, wparam, lparam) }
}

/// Around `r`, a window's visible frame; None hides it.
pub fn show(r: Option<Rect>) {
    let window = WINDOW.get();
    if window.is_invalid() || AT.get() == r {
        return;
    }
    AT.set(r);
    unsafe {
        let Some(r) = r else {
            let _ = ShowWindowAsync(window, SW_HIDE);
            return;
        };
        let t = THICKNESS.get();
        let (width, height) = (r.width() + 2 * t, r.height() + 2 * t);
        // The window owns the region once it's set, so only the scratch one
        // is deleted.
        let frame = CreateRectRgn(0, 0, width, height);
        let inside = CreateRectRgn(t, t, width - t, height - t);
        CombineRgn(Some(frame), Some(frame), Some(inside), RGN_DIFF);
        let _ = DeleteObject(inside.into());
        SetWindowRgn(window, Some(frame), false);
        let _ = SetWindowPos(window, Some(HWND_TOPMOST), r.left - t, r.top - t, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}
