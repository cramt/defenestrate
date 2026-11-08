//! The Super-tap launcher: type to fuzzy-filter the Start Menu's
//! shortcuts, Enter starts the selection. With nothing matching, Enter runs
//! what was typed as a command, like a Run box. It shows the clock too,
//! since there's no bar to put one in.

use std::cell::{Cell, RefCell};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteObject,
    DrawTextW, EndPaint, FillRect, HFONT, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetSystemMetrics, IDC_ARROW, KillTimer, LoadCursorW, RegisterClassW, SM_CXSCREEN,
    SM_CYSCREEN, SW_HIDE, SW_SHOW, SWP_NOACTIVATE, SWP_NOSIZE, SetTimer, SetWindowPos, ShowWindow, WA_INACTIVE, WM_ACTIVATE,
    WM_CHAR, WM_KEYDOWN, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Result, w};

use crate::{desktop, fuzzy, log, scaled};

const ROWS: usize = 10;
const WIDTH: i32 = 640;
const ROW_HEIGHT: i32 = 28;
const PADDING: i32 = 12;
const CLOCK_TIMER: usize = 1;
// CREATE_NO_WINDOW: `cmd /c start` is only a launcher, no console wanted.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// COLORREF is 0x00BBGGRR.
const BACKGROUND: COLORREF = COLORREF(0x0020_1818);
const SELECTED: COLORREF = COLORREF(0x0040_3030);
const FOREGROUND: COLORREF = COLORREF(0x00E0_E0E0);
const DIM: COLORREF = COLORREF(0x0080_8080);

struct App {
    name: String,
    path: PathBuf,
}

#[derive(Default)]
struct State {
    apps: Vec<App>,
    query: String,
    /// Indices into `apps`, best match first.
    matches: Vec<usize>,
    /// Index into `matches`; 0 while it's empty.
    selected: usize,
}

impl State {
    fn refilter(&mut self) {
        let names: Vec<&str> = self.apps.iter().map(|a| a.name.as_str()).collect();
        self.matches = fuzzy::rank(&self.query, &names);
        self.selected = 0;
    }

    fn select(&mut self, by: isize) {
        let n = self.matches.len() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + by).rem_euclid(n) as usize;
        }
    }
}

enum Launch {
    Shortcut(PathBuf),
    CommandLine(String),
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static FONT: Cell<HFONT> = const { Cell::new(HFONT(std::ptr::null_mut())) };
}

pub fn create(instance: HINSTANCE, font: HFONT) -> Result<()> {
    FONT.set(font);
    let class = w!("DefenestrateLauncher");
    unsafe {
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let height = scaled(ROW_HEIGHT) * (ROWS as i32 + 1) + scaled(PADDING) * 2;
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("defenestrate launcher"),
            WS_POPUP,
            0,
            0,
            scaled(WIDTH),
            height,
            None,
            None,
            Some(instance),
            None,
        )?;
        WINDOW.set(window);
    }
    Ok(())
}

pub fn toggle() {
    let window = WINDOW.get();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(window) }.as_bool() {
        hide();
    } else {
        show();
    }
}

fn show() {
    let window = WINDOW.get();
    let apps = scan();
    STATE.with_borrow_mut(|s| {
        s.apps = apps;
        s.query.clear();
        s.refilter();
    });
    unsafe {
        // Centred, a quarter of the way down: where the eye already is.
        let x = (GetSystemMetrics(SM_CXSCREEN) - scaled(WIDTH)) / 2;
        let y = GetSystemMetrics(SM_CYSCREEN) / 4;
        let _ = SetWindowPos(window, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = ShowWindow(window, SW_SHOW);
        SetTimer(Some(window), CLOCK_TIMER, 1000, None);
    }
    desktop::focus(window);
}

fn hide() {
    let window = WINDOW.get();
    unsafe {
        let _ = KillTimer(Some(window), CLOCK_TIMER);
        let _ = ShowWindow(window, SW_HIDE);
    }
}

/// Every shortcut in the all-users and the user's Start Menu, the way
/// Start lists them: by file name, uninstallers left out.
fn scan() -> Vec<App> {
    let roots = [("ProgramData", r"Microsoft\Windows\Start Menu\Programs"), ("APPDATA", r"Microsoft\Windows\Start Menu\Programs")];
    let mut apps: Vec<App> = Vec::new();
    for (var, sub) in roots {
        if let Some(base) = std::env::var_os(var) {
            walk(&Path::new(&base).join(sub), &mut apps);
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by_key(|a| a.name.to_lowercase());
    apps
}

fn walk(dir: &Path, out: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if !matches!(ext.as_deref(), Some("lnk" | "url" | "appref-ms")) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if name.to_lowercase().contains("uninstall") {
            continue;
        }
        out.push(App { name, path });
    }
}

fn launch(what: Launch) {
    // `start` resolves shortcuts, URLs, PATH lookups and arguments the
    // way the Run box does, and doesn't wait for what it starts.
    let target = match &what {
        Launch::Shortcut(path) => format!("\"{}\"", path.display()),
        Launch::CommandLine(line) => line.clone(),
    };
    let mut cmd = Command::new("cmd.exe");
    cmd.raw_arg(format!("/c start \"\" {target}")).creation_flags(CREATE_NO_WINDOW);
    if let Some(home) = std::env::var_os("USERPROFILE") {
        cmd.current_dir(home);
    }
    if let Err(e) = cmd.spawn() {
        log(&format!("launching {target} failed: {e}"));
    }
}

fn on_key(key: u16) -> bool {
    let vk = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(key);
    match vk {
        VK_ESCAPE => {
            hide();
            desktop::refocus();
        }
        VK_RETURN => {
            let what = STATE.with_borrow(|s| match s.matches.get(s.selected) {
                Some(&i) => Some(Launch::Shortcut(s.apps[i].path.clone())),
                None if !s.query.trim().is_empty() => Some(Launch::CommandLine(s.query.trim().to_owned())),
                None => None,
            });
            hide();
            if let Some(what) = what {
                launch(what);
            }
        }
        VK_DOWN | VK_TAB => STATE.with_borrow_mut(|s| s.select(1)),
        VK_UP => STATE.with_borrow_mut(|s| s.select(-1)),
        VK_NEXT => STATE.with_borrow_mut(|s| s.select(ROWS as isize)),
        VK_PRIOR => STATE.with_borrow_mut(|s| s.select(-(ROWS as isize))),
        _ => return false,
    }
    true
}

fn on_char(c: u32) {
    STATE.with_borrow_mut(|s| {
        match c {
            0x08 => {
                s.query.pop();
            }
            // Ctrl+Backspace: drop the last word.
            0x7F => {
                let keep = s.query.trim_end().rfind(' ').map_or(0, |i| i + 1);
                s.query.truncate(keep);
            }
            c if c >= 0x20 => {
                if let Some(ch) = char::from_u32(c) {
                    s.query.push(ch);
                }
            }
            _ => return,
        }
        s.refilter();
    });
}

fn clock_text() -> String {
    let now = unsafe { GetLocalTime() };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!(
        "{} {} {}  {:02}:{:02}",
        DAYS[usize::from(now.wDayOfWeek) % 7],
        now.wDay,
        MONTHS[usize::from(now.wMonth).saturating_sub(1) % 12],
        now.wHour,
        now.wMinute
    )
}

fn text(hdc: windows::Win32::Graphics::Gdi::HDC, s: &str, rect: &mut RECT, align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT) {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        DrawTextW(hdc, &mut wide, rect, align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
    }
}

fn paint(window: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(window, &mut ps);
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(window, &mut client);
        let background = CreateSolidBrush(BACKGROUND);
        FillRect(hdc, &client, background);
        let _ = DeleteObject(background.into());
        SelectObject(hdc, FONT.get().into());
        SetBkMode(hdc, TRANSPARENT);

        let (pad, row) = (scaled(PADDING), scaled(ROW_HEIGHT));
        let mut line = RECT { left: pad, top: pad, right: client.right - pad, bottom: pad + row };
        STATE.with_borrow(|s| {
            SetTextColor(hdc, DIM);
            text(hdc, &clock_text(), &mut line.clone(), DT_RIGHT);
            SetTextColor(hdc, FOREGROUND);
            text(hdc, &format!("> {}_", s.query), &mut line.clone(), DT_LEFT);

            // Scrolls by pages so the selection is always on screen.
            let first = s.selected / ROWS * ROWS;
            for (i, &app) in s.matches.iter().enumerate().skip(first).take(ROWS) {
                line.top += row;
                line.bottom += row;
                if i == s.selected {
                    let highlight = CreateSolidBrush(SELECTED);
                    FillRect(hdc, &RECT { left: 0, right: client.right, ..line }, highlight);
                    let _ = DeleteObject(highlight.into());
                }
                SetTextColor(hdc, FOREGROUND);
                text(hdc, &s.apps[app].name, &mut line.clone(), DT_LEFT);
            }
            if s.matches.is_empty() && !s.query.trim().is_empty() {
                line.top += row;
                line.bottom += row;
                SetTextColor(hdc, DIM);
                text(hdc, "Enter runs it as a command", &mut line.clone(), DT_LEFT);
            }
        });
        let _ = EndPaint(window, &ps);
    }
}

unsafe extern "system" fn wndproc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(window), None, false);
    };
    match msg {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        WM_KEYDOWN if on_key(wparam.0 as u16) => {
            redraw();
            LRESULT(0)
        }
        WM_CHAR => {
            on_char(wparam.0 as u32);
            redraw();
            LRESULT(0)
        }
        WM_TIMER => {
            redraw();
            LRESULT(0)
        }
        // Clicking elsewhere dismisses it, like Start.
        WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            hide();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, msg, wparam, lparam) },
    }
}
