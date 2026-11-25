//! COSMIC's window switcher (Alt+Tab, Super+Tab), which there is
//! cosmic-launcher's alt-tab view: every window on every workspace,
//! minimized ones too, most recently focused first, each as its title over
//! its app's name. Letting go of the modifiers switches to the selection
//! (desktop::activate goes to its workspace and restores it); Escape or
//! clicking elsewhere closes it. Looks like the launcher, without the search
//! line, since COSMIC's alt-tab view has none either.

use std::cell::{Cell, RefCell};
use std::path::Path;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_LEFT, DT_RIGHT, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, IDC_ARROW,
    LoadCursorW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
    ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_LBUTTONUP, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Result, w};

use crate::cycle::Cycle;
use crate::keys::Switch;
use crate::launcher::{BACKGROUND, DIM, FOREGROUND, PADDING, ROW_HEIGHT, SELECTED, WIDTH, text};
use crate::{desktop, font, scaled, use_dpi};

const ROWS: usize = 10;

struct Entry {
    window: HWND,
    title: String,
    app: String,
}

thread_local! {
    static OPEN: RefCell<Option<Cycle<Entry>>> = const { RefCell::new(None) };
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

pub fn create(instance: HINSTANCE) -> Result<()> {
    let class = w!("DefenestrateSwitcher");
    unsafe {
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("defenestrate switcher"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        WINDOW.set(window);
    }
    Ok(())
}

pub fn on(s: Switch) {
    match s {
        Switch::Step(step) => {
            let opened = OPEN.with_borrow_mut(|open| match open {
                Some(c) => {
                    c.step(step);
                    false
                }
                None => {
                    *open = Cycle::open(entries(), step);
                    open.is_some()
                }
            });
            if opened {
                show();
            }
            redraw();
        }
        Switch::Done => {
            if let Some(chosen) = close() {
                desktop::activate(chosen);
            }
        }
        Switch::Cancel => {
            if close().is_some() {
                desktop::refocus();
            }
        }
    }
}

fn entries() -> Vec<Entry> {
    desktop::switchable().into_iter().map(|window| Entry { window, title: desktop::window_text(window), app: app_name(window) }).collect()
}

/// The exe's file stem, the closest thing Windows has to the app name
/// COSMIC shows under the title. Unlike the app ID tiling exceptions match
/// (classify::app_id), any extension goes, in any case: it's only shown.
fn app_name(w: HWND) -> String {
    let path = desktop::ask_process(desktop::pid_of(w), desktop::exe_path).flatten().unwrap_or_default();
    Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn show() {
    let window = WINDOW.get();
    let rows = OPEN.with_borrow(|o| o.as_ref().map_or(0, |c| c.items().len().min(ROWS))) as i32;
    unsafe {
        // Where the launcher goes: the pointer's monitor, at its DPI.
        let (area, dpi) = desktop::overlay_spot();
        use_dpi(dpi);
        let (width, height) = (scaled(WIDTH), scaled(ROW_HEIGHT) * rows + scaled(PADDING) * 2);
        let x = area.left + (area.width() - width) / 2;
        let y = area.top + area.height() / 4;
        let _ = SetWindowPos(window, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = ShowWindow(window, SW_SHOW);
    }
    desktop::focus(window);
}

/// Hides the switcher; what was selected, if it was open.
fn close() -> Option<HWND> {
    let chosen = OPEN.take().map(|c| c.into_selected().window);
    unsafe {
        let _ = ShowWindow(WINDOW.get(), SW_HIDE);
    }
    chosen
}

fn redraw() {
    unsafe {
        let _ = InvalidateRect(Some(WINDOW.get()), None, false);
    }
}

/// The first row on screen: it pages, so the selection is always shown.
fn first_row(at: usize) -> usize {
    at / ROWS * ROWS
}

fn paint(window: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(window, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(window, &mut client);
        let background = CreateSolidBrush(BACKGROUND);
        FillRect(hdc, &client, background);
        let _ = DeleteObject(background.into());
        SelectObject(hdc, font().into());
        SetBkMode(hdc, TRANSPARENT);

        let (pad, row) = (scaled(PADDING), scaled(ROW_HEIGHT));
        let mut line = RECT { left: pad, top: pad, right: client.right - pad, bottom: pad + row };
        OPEN.with_borrow(|open| {
            let Some(c) = open else { return };
            for (i, e) in c.items().iter().enumerate().skip(first_row(c.at())).take(ROWS) {
                if i == c.at() {
                    let highlight = CreateSolidBrush(SELECTED);
                    FillRect(hdc, &RECT { left: 0, right: client.right, ..line }, highlight);
                    let _ = DeleteObject(highlight.into());
                }
                SetTextColor(hdc, DIM);
                text(hdc, &e.app, &mut line.clone(), DT_RIGHT);
                SetTextColor(hdc, FOREGROUND);
                let mut title = RECT { right: line.right - scaled(WIDTH) / 4, ..line };
                text(hdc, &e.title, &mut title, DT_LEFT);
                line.top += row;
                line.bottom += row;
            }
        });
        let _ = EndPaint(window, &ps);
    }
}

unsafe extern "system" fn wndproc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        // COSMIC activates a row on click too.
        WM_LBUTTONUP => {
            let y = i32::from((lparam.0 & 0xFFFF) as u16 as i16);
            let row = (y - scaled(PADDING)).div_euclid(scaled(ROW_HEIGHT));
            OPEN.with_borrow_mut(|open| {
                if let Some(c) = open
                    && row >= 0
                {
                    c.select(first_row(c.at()) + row as usize);
                }
            });
            on(Switch::Done);
            LRESULT(0)
        }
        // Losing focus closes it without switching, as in COSMIC.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if close().is_some() {
                crate::log(&format!("switcher lost focus to {:?}", HWND(lparam.0 as *mut _)));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, msg, wparam, lparam) },
    }
}
