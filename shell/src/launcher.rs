//! The Super-tap launcher: type to fuzzy-filter the Start Menu's
//! shortcuts, Enter starts the selection. With nothing matching, Enter runs
//! what was typed as a command, like a Run box. It shows the clock too,
//! since there's no bar to put one in.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DT_LEFT, DT_RIGHT, InvalidateRect, SetTextColor};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_ESCAPE, VK_NEXT, VK_PRIOR, VK_RETURN, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, KillTimer, SW_HIDE, SW_SHOWNORMAL, SWP_NOACTIVATE, SetTimer, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_CHAR,
    WM_KEYDOWN, WM_PAINT, WM_TIMER,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::System::Threading::GetProcessId;
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::popup::{DIM, FOREGROUND, Popup, ROW_HEIGHT, ROWS, first_row, highlight, size, text};
use crate::{desktop, fuzzy, log};

const CLOCK_TIMER: usize = 1;

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
    /// A URI to its registered handler, or if ShellExecuteEx can't, the
    /// command line `or_else`.
    Uri { uri: &'static str, or_else: &'static str },
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static POPUP: Cell<Popup> = const { Cell::new(Popup::NONE) };
}

/// The query line, then the matches.
const LINES: i32 = ROWS as i32 + 1;

/// Hidden, sized for `dpi` until it first opens.
pub fn create(instance: HINSTANCE, dpi: u32) -> Result<()> {
    POPUP.set(Popup::create(instance, w!("DefenestrateLauncher"), w!("defenestrate launcher"), Some(wndproc), size(LINES, dpi), dpi)?);
    Ok(())
}

pub fn toggle() {
    let window = POPUP.get().window;
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(window) }.as_bool() {
        hide();
    } else {
        show();
    }
}

fn show() {
    let apps = scan();
    STATE.with_borrow_mut(|s| {
        s.apps = apps;
        s.query.clear();
        s.refilter();
    });
    Popup::open(&POPUP, LINES, SWP_NOACTIVATE);
    let window = POPUP.get().window;
    unsafe {
        SetTimer(Some(window), CLOCK_TIMER, 1000, None);
    }
    desktop::focus(window);
}

fn hide() {
    let window = POPUP.get().window;
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

/// A file as explorer opens it, a shortcut's target and all (autostart.rs).
pub fn open(path: &Path) {
    launch(Launch::Shortcut(path.to_path_buf()));
}

/// COSMIC's System(Screenshot), as Windows does it: the Snipping Tool's
/// region overlay, which copies the snip to the clipboard and saves it to
/// Pictures\Screenshots. Windows opens it from PrintScreen through explorer's
/// own key handling, gone here; the plain key only copies the whole screen
/// (win32k), with nothing on screen to say so. Whether `ms-screenclip:`
/// opens without explorer isn't known yet, so if its handler can't be
/// started the Snipping Tool is asked directly; the log says which went.
pub fn screenshot() {
    launch(Launch::Uri { uri: "ms-screenclip:", or_else: "SnippingTool.exe /clip" });
}

/// Through ShellExecuteEx, as explorer opens things: a shortcut's target,
/// arguments and working directory come from the shortcut itself. Typed
/// lines go to `start`, which resolves URLs, PATH lookups and arguments the
/// way the Run box does. Spawning cmd.exe for shortcuts too failed once with
/// "Access is denied" on saturn while Super+T's cmd.exe started fine
/// (windows-bugs.md #14); the cause wasn't found.
fn launch(what: Launch) {
    let home = std::env::var_os("USERPROFILE").map(HSTRING::from);
    let (file, params, show, target, or_else) = match what {
        Launch::Shortcut(path) => (HSTRING::from(path.as_os_str()), None, SW_SHOWNORMAL, path.display().to_string(), None),
        Launch::CommandLine(line) => (HSTRING::from("cmd.exe"), Some(HSTRING::from(format!("/c start \"\" {line}"))), SW_HIDE, line, None),
        Launch::Uri { uri, or_else } => (HSTRING::from(uri), None, SW_SHOWNORMAL, uri.to_owned(), Some(or_else)),
    };
    // Its own thread: a slow or hung shell extension mustn't hang the shell.
    std::thread::spawn(move || {
        let pcwstr = |s: &Option<HSTRING>| s.as_ref().map_or(PCWSTR::null(), |s| PCWSTR(s.as_ptr()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            // Done before returning, as this thread ends right after; errors
            // go to the log, not a dialog.
            fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: pcwstr(&params),
            lpDirectory: pcwstr(&home),
            nShow: show.0,
            ..Default::default()
        };
        unsafe {
            // ShellExecuteEx wants COM, single-threaded, as its docs say.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            // Launches are logged: whether the launcher opened Firefox on
            // saturn (2026-11-19) couldn't be told from the logs. No process
            // is what a single-instance app handing off, or a URL going to
            // a running browser, gives.
            match ShellExecuteExW(&mut info) {
                Err(e) => match or_else {
                    Some(line) => {
                        log(&format!("launching {target:?} failed: {e}; starting {line:?} instead"));
                        crate::spawn_line(line);
                    }
                    None => log(&format!("launching {target:?} failed: {e}")),
                },
                Ok(()) if info.hProcess.is_invalid() => log(&format!("launched {target:?}: no new process")),
                Ok(()) => {
                    log(&format!("launched {target:?}: pid {} ({})", GetProcessId(info.hProcess), desktop::token_summary(info.hProcess)));
                    let _ = CloseHandle(info.hProcess);
                }
            }
        }
    });
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

fn paint() {
    let popup = POPUP.get();
    let row = popup.scaled(ROW_HEIGHT);
    popup.paint(|hdc, client, mut line| unsafe {
        STATE.with_borrow(|s| {
            SetTextColor(hdc, DIM);
            text(hdc, &clock_text(), &mut line.clone(), DT_RIGHT);
            SetTextColor(hdc, FOREGROUND);
            text(hdc, &format!("> {}_", s.query), &mut line.clone(), DT_LEFT);

            for (i, &app) in s.matches.iter().enumerate().skip(first_row(s.selected)).take(ROWS) {
                line.top += row;
                line.bottom += row;
                if i == s.selected {
                    highlight(hdc, client, line);
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
    });
}

unsafe extern "system" fn wndproc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let redraw = || unsafe {
        let _ = InvalidateRect(Some(window), None, false);
    };
    match msg {
        WM_PAINT => {
            paint();
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
