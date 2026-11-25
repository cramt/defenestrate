//! What the launcher and the window switcher share: a topmost list of rows
//! opened centred on the pointer's monitor, a quarter of the way down, and
//! drawn at that monitor's DPI (Per-Monitor V2: each monitor has its own).

use std::cell::Cell;
use std::thread::LocalKey;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FF_MODERN, FIXED_PITCH, FW_NORMAL, FillRect, HDC, HFONT,
    OUT_TT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClientRect, IDC_ARROW, LoadCursorW, RegisterClassW, SET_WINDOW_POS_FLAGS, SW_SHOW, SetWindowPos, ShowWindow,
    WNDCLASSW, WNDPROC, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Result};

use crate::{desktop, scaled_at};

/// Rows a popup shows at once; it pages past that.
pub const ROWS: usize = 10;
// In 96 DPI pixels.
pub const WIDTH: i32 = 640;
pub const ROW_HEIGHT: i32 = 28;
pub const PADDING: i32 = 12;
const FONT_HEIGHT: i32 = 18;
// Installed by configs/default.nix (`fonts`). GDI quietly substitutes
// something else if it's missing.
const FONT_FACE: PCWSTR = windows::core::w!("Iosevka NFM");

// COLORREF is 0x00BBGGRR.
pub const BACKGROUND: COLORREF = COLORREF(0x0020_1818);
pub const SELECTED: COLORREF = COLORREF(0x0040_3030);
pub const FOREGROUND: COLORREF = COLORREF(0x00E0_E0E0);
pub const DIM: COLORREF = COLORREF(0x0080_8080);

thread_local! {
    static FONT: Cell<Option<(u32, HFONT)>> = const { Cell::new(None) };
}

/// A popup's window, and the DPI of the monitor it last opened on, which
/// it's drawn at.
#[derive(Clone, Copy)]
pub struct Popup {
    pub window: HWND,
    pub dpi: u32,
}

impl Popup {
    pub const NONE: Popup = Popup { window: HWND(std::ptr::null_mut()), dpi: 96 };

    /// Hidden, `size` big until it first opens.
    pub fn create(instance: HINSTANCE, class: PCWSTR, title: PCWSTR, proc: WNDPROC, (width, height): (i32, i32), dpi: u32) -> Result<Popup> {
        unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance,
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            });
            let window =
                CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, class, title, WS_POPUP, 0, 0, width, height, None, None, Some(instance), None)?;
            Ok(Popup { window, dpi })
        }
    }

    /// Shows it `rows` rows tall where it opens: on the monitor the pointer
    /// is on, as COSMIC opens it on the active output, sized for that
    /// monitor's DPI. Centred, a quarter of the way down: where the eye
    /// already is. The new DPI is in `slot` before anything is moved, as
    /// moving and showing it can draw it.
    pub fn open(slot: &'static LocalKey<Cell<Popup>>, rows: i32, flags: SET_WINDOW_POS_FLAGS) {
        let (area, dpi) = desktop::overlay_spot();
        let popup = Popup { dpi, ..slot.get() };
        slot.set(popup);
        let (width, height) = size(rows, dpi);
        let x = area.left + (area.width() - width) / 2;
        let y = area.top + area.height() / 4;
        unsafe {
            let _ = SetWindowPos(popup.window, None, x, y, width, height, flags);
            let _ = ShowWindow(popup.window, SW_SHOW);
        }
    }

    pub fn scaled(self, px: i32) -> i32 {
        scaled_at(px, self.dpi)
    }

    /// Paints it: the background, then `rows` with the font and the text
    /// background set, given the client rect and the first row's rect.
    pub fn paint(self, rows: impl FnOnce(HDC, RECT, RECT)) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(self.window, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(self.window, &mut client);
            let background = CreateSolidBrush(BACKGROUND);
            FillRect(hdc, &client, background);
            let _ = DeleteObject(background.into());
            SelectObject(hdc, font(self.dpi).into());
            SetBkMode(hdc, TRANSPARENT);
            let (pad, row) = (self.scaled(PADDING), self.scaled(ROW_HEIGHT));
            rows(hdc, client, RECT { left: pad, top: pad, right: client.right - pad, bottom: pad + row });
            let _ = EndPaint(self.window, &ps);
        }
    }

    /// The row at client `y`, counted from the first on screen.
    pub fn row_at(self, y: i32) -> i32 {
        (y - self.scaled(PADDING)).div_euclid(self.scaled(ROW_HEIGHT))
    }
}

/// A popup `rows` rows tall at `dpi`.
pub fn size(rows: i32, dpi: u32) -> (i32, i32) {
    (scaled_at(WIDTH, dpi), scaled_at(ROW_HEIGHT, dpi) * rows + scaled_at(PADDING, dpi) * 2)
}

/// The first row on screen: popups page, so the selection is always shown.
pub fn first_row(selected: usize) -> usize {
    selected / ROWS * ROWS
}

/// The selected row's highlight, across the whole width.
pub fn highlight(hdc: HDC, client: RECT, line: RECT) {
    unsafe {
        let brush = CreateSolidBrush(SELECTED);
        FillRect(hdc, &RECT { left: 0, right: client.right, ..line }, brush);
        let _ = DeleteObject(brush.into());
    }
}

pub fn text(hdc: HDC, s: &str, rect: &mut RECT, align: DRAW_TEXT_FORMAT) {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        DrawTextW(hdc, &mut wide, rect, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
    }
}

/// The shell's font at `dpi`, made the first time it's needed at that DPI.
fn font(dpi: u32) -> HFONT {
    if let Some((at, f)) = FONT.get()
        && at == dpi
    {
        return f;
    }
    let f = unsafe {
        CreateFontW(
            -scaled_at(FONT_HEIGHT, dpi),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (FIXED_PITCH.0 | FF_MODERN.0) as u32,
            FONT_FACE,
        )
    };
    if let Some((_, old)) = FONT.replace(Some((dpi, f))) {
        unsafe {
            let _ = DeleteObject(old.into());
        }
    }
    f
}
