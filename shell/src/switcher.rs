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
use windows::Win32::Graphics::Gdi::{DT_LEFT, DT_RIGHT, InvalidateRect, SetTextColor};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, SW_HIDE, SWP_NOACTIVATE, SWP_NOZORDER, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_LBUTTONUP, WM_PAINT,
};
use windows::core::{Result, w};

use crate::cycle::Cycle;
use crate::keys::Switch;
use crate::popup::{DIM, FOREGROUND, Popup, ROW_HEIGHT, ROWS, WIDTH, first_row, highlight, text};
use crate::desktop;

struct Entry {
    window: HWND,
    title: String,
    app: String,
}

thread_local! {
    static OPEN: RefCell<Option<Cycle<Entry>>> = const { RefCell::new(None) };
    static POPUP: Cell<Popup> = const { Cell::new(Popup::NONE) };
}

pub fn create(instance: HINSTANCE, dpi: u32) -> Result<()> {
    POPUP.set(Popup::create(instance, w!("DefenestrateSwitcher"), w!("defenestrate switcher"), Some(wndproc), (0, 0), dpi)?);
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

/// Where the launcher goes, as tall as the rows it shows.
fn show() {
    let rows = OPEN.with_borrow(|o| o.as_ref().map_or(0, |c| c.items().len().min(ROWS))) as i32;
    Popup::open(&POPUP, rows, SWP_NOZORDER | SWP_NOACTIVATE);
    desktop::focus(POPUP.get().window);
}

/// Hides the switcher; what was selected, if it was open.
fn close() -> Option<HWND> {
    let chosen = OPEN.take().map(|c| c.into_selected().window);
    unsafe {
        let _ = ShowWindow(POPUP.get().window, SW_HIDE);
    }
    chosen
}

fn redraw() {
    unsafe {
        let _ = InvalidateRect(Some(POPUP.get().window), None, false);
    }
}

fn paint() {
    let popup = POPUP.get();
    let row = popup.scaled(ROW_HEIGHT);
    popup.paint(|hdc, client, mut line| unsafe {
        OPEN.with_borrow(|open| {
            let Some(c) = open else { return };
            for (i, e) in c.items().iter().enumerate().skip(first_row(c.at())).take(ROWS) {
                if i == c.at() {
                    highlight(hdc, client, line);
                }
                SetTextColor(hdc, DIM);
                text(hdc, &e.app, &mut line.clone(), DT_RIGHT);
                SetTextColor(hdc, FOREGROUND);
                let mut title = RECT { right: line.right - popup.scaled(WIDTH) / 4, ..line };
                text(hdc, &e.title, &mut title, DT_LEFT);
                line.top += row;
                line.bottom += row;
            }
        });
    });
}

unsafe extern "system" fn wndproc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint();
            LRESULT(0)
        }
        // COSMIC activates a row on click too.
        WM_LBUTTONUP => {
            let y = i32::from((lparam.0 & 0xFFFF) as u16 as i16);
            let row = POPUP.get().row_at(y);
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

