//! v0 of the shell Winlogon starts instead of explorer.exe: a top bar with a
//! clock, and Win+Enter for a terminal. Mostly here to prove the
//! cross-compile → image → Winlogon path end to end.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    EndPaint, InvalidateRect, PAINTSTRUCT, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LWIN, VK_RETURN, VK_RWIN};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, HC_ACTION, IDC_ARROW, KBDLLHOOKSTRUCT, LoadCursorW, MSG, PostQuitMessage,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SPI_SETWORKAREA, SPIF_SENDCHANGE, SetTimer,
    SetWindowsHookExW, SystemParametersInfoW, TranslateMessage, WH_KEYBOARD_LL, WM_DESTROY,
    WM_KEYDOWN, WM_PAINT, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP, WS_VISIBLE,
};
use windows::core::{Result, w};

const BAR_HEIGHT: i32 = 28;
const CLOCK_TIMER: usize = 1;
const CREATE_NEW_CONSOLE: u32 = 0x10;

// COLORREF is 0x00BBGGRR.
const BACKGROUND: COLORREF = COLORREF(0x0020_1818);
const FOREGROUND: COLORREF = COLORREF(0x00E0_E0E0);

fn main() {
    if let Err(e) = run() {
        log(&format!("fatal: {e}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("DefenestrateBar");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: CreateSolidBrush(BACKGROUND),
            ..Default::default()
        });

        let width = GetSystemMetrics(SM_CXSCREEN);
        let bar = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("defenestrate"),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            width,
            BAR_HEIGHT,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        // Everything past the window is optional. A shell that exits leaves
        // the user staring at an empty desktop, so these only get logged.
        if let Err(e) = reserve_bar_space(width) {
            log(&format!("reserving the work area failed: {e}"));
        }
        // RegisterHotKey can't have Win+Enter even with explorer gone
        // ("Hot key is already registered"), so keys are taken from a
        // low-level hook, which sees them before any registered hotkey.
        // The hook lives as long as this thread's message loop runs.
        if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance.into()), 0) {
            log(&format!("installing the keyboard hook failed: {e}"));
        }
        SetTimer(Some(bar), CLOCK_TIMER, 1000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Shrinks the work area so maximized windows stop at the bar. The AppBar
/// API would be the polite way, but explorer is what implements it, so a
/// replacement shell has to set the work area itself.
unsafe fn reserve_bar_space(width: i32) -> Result<()> {
    let mut work_area = RECT {
        left: 0,
        top: BAR_HEIGHT,
        right: width,
        bottom: unsafe { GetSystemMetrics(SM_CYSCREEN) },
    };
    unsafe {
        SystemParametersInfoW(
            SPI_SETWORKAREA,
            0,
            Some((&raw mut work_area).cast()),
            SPIF_SENDCHANGE,
        )
    }
}

/// Appends to %LOCALAPPDATA%\\defenestrate\\shell.log. There is no console
/// and no explorer to show a dialog, so a file is the only place to look.
fn log(line: &str) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("defenestrate")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("shell.log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn clock_text() -> Vec<u16> {
    let now = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", now.wHour, now.wMinute)
        .encode_utf16()
        .collect()
}

fn win_held() -> bool {
    // The high bit of GetAsyncKeyState is "down right now".
    let down = |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe {
        GetAsyncKeyState(vk.0.into()) < 0
    };
    down(VK_LWIN) || down(VK_RWIN)
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let key_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    if code == HC_ACTION as i32 && key_down {
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if key.vkCode == u32::from(VK_RETURN.0) && win_held() {
            if let Err(e) = Command::new("cmd.exe").creation_flags(CREATE_NEW_CONSOLE).spawn() {
                log(&format!("starting a terminal failed: {e}"));
            }
            // Swallowed, so whatever else owns Win+Enter never sees it.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            rect.left += 10;
            rect.right -= 10;
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, FOREGROUND);
            let mut title: Vec<u16> = "defenestrate".encode_utf16().collect();
            DrawTextW(hdc, &mut title, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            DrawTextW(hdc, &mut clock_text(), &mut rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_TIMER => unsafe {
            let _ = InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        },
        WM_DESTROY => unsafe {
            PostQuitMessage(0);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
