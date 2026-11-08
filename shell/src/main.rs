//! The shell Winlogon starts instead of explorer.exe: a tiling window
//! manager (wm.rs, desktop.rs), a Super-tap launcher (launcher.rs), and
//! Super+Enter for a terminal. Keys are in keys.rs.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod fuzzy;
mod tree;
mod wm;

#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod launcher;

#[cfg(not(windows))]
fn main() {
    eprintln!("defenestrate-shell only runs on Windows; this build is for the tests");
}

#[cfg(windows)]
use windows_main::{log, scaled};

#[cfg(windows)]
fn main() {
    // panic = "abort" skips unwinding, but the hook still runs first, so
    // a panic leaves a line in the log instead of a silently missing shell.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    log(&format!("started, pid {}", std::process::id()));
    match windows_main::run() {
        Ok(()) => log("message loop ended"),
        Err(e) => {
            log(&format!("fatal: {e}"));
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
mod windows_main {
    use std::cell::Cell;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, FF_MODERN, FIXED_PITCH, FW_NORMAL, OUT_TT_PRECIS,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Shutdown::{EWX_LOGOFF, ExitWindowsEx, LockWorkStation, SHUTDOWN_REASON};
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage, RegisterClassW, TranslateMessage,
        WM_DESTROY, WM_DISPLAYCHANGE, WM_ENDSESSION, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows::core::{Result, w};

    use crate::keys::{self, Action, WM_ACTION};
    use crate::{desktop, launcher};

    // In 96 DPI pixels; scaled to the real DPI at startup.
    const GAP: i32 = 8;
    const FONT_HEIGHT: i32 = 18;
    // Installed by configs/default.nix (`fonts`). GDI quietly substitutes
    // something else if it's missing.
    const FONT_FACE: windows::core::PCWSTR = w!("Iosevka NFM");
    const CREATE_NEW_CONSOLE: u32 = 0x10;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    thread_local! {
        static DPI: Cell<i32> = const { Cell::new(96) };
    }

    pub fn scaled(px: i32) -> i32 {
        px * DPI.get() / 96
    }

    /// Appends to %LOCALAPPDATA%\defenestrate\shell.log. There is no
    /// console and no explorer to show a dialog, so a file is the only
    /// place to look.
    pub fn log(line: &str) {
        use std::io::Write;
        let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("defenestrate")) else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("shell.log")) {
            let _ = writeln!(f, "{line}");
        }
    }

    pub fn run() -> Result<()> {
        unsafe {
            // Without this Windows bitmap-stretches our windows on any scaled
            // display, and hands us scaled coordinates for everyone else's.
            if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
                log(&format!("becoming DPI aware failed: {e}"));
            }
            DPI.set(GetDpiForSystem() as i32);
            let font = CreateFontW(
                -scaled(FONT_HEIGHT),
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
            );

            let instance = GetModuleHandleW(None)?.into();
            let class = w!("DefenestrateShell");
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance, lpszClassName: class, ..Default::default() });
            // Never shown. A real top-level window rather than a message-only
            // one, because only those get broadcasts like WM_DISPLAYCHANGE.
            let main = CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("defenestrate"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None)?;

            // Past the main window everything is optional: a shell that exits
            // leaves the user with nothing, so failures only get logged.
            // Keys first: they're how the user gets anything done, so nothing
            // that might stall on another app's windows goes before them.
            match keys::install(main) {
                Ok(()) => log("keyboard hook installed"),
                Err(e) => log(&format!("installing the keyboard hook failed: {e}")),
            }
            if let Err(e) = launcher::create(instance, font) {
                log(&format!("creating the launcher failed: {e}"));
            }
            desktop::start(main, scaled(GAP));
            log("managing windows");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    /// Starts `program` in the user's home, as COSMIC's shortcuts do.
    fn spawn(program: &str, args: &[&str], flags: u32) {
        let mut cmd = Command::new(program);
        cmd.args(args).creation_flags(flags);
        if let Some(home) = std::env::var_os("USERPROFILE") {
            cmd.current_dir(home);
        }
        if let Err(e) = cmd.spawn() {
            log(&format!("starting {program} failed: {e}"));
        }
    }

    unsafe extern "system" fn wndproc(window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            desktop::WM_WINEVENT => {
                desktop::on_event(wparam.0 as u32, HWND(lparam.0 as *mut _));
                LRESULT(0)
            }
            WM_ACTION => {
                for action in keys::take() {
                    match action {
                        Action::Launcher => launcher::toggle(),
                        Action::Terminal => spawn("cmd.exe", &[], CREATE_NEW_CONSOLE),
                        // Explorer isn't the shell, but it still opens folder windows.
                        Action::HomeFolder => spawn("explorer.exe", &["."], 0),
                        // DisableLockWorkstation (configs/default.nix, for Super+L)
                        // blocks LockWorkStation too. Disconnecting the console
                        // session lands on the same lock screen, and signing in
                        // reconnects to it with every window as it was.
                        Action::Lock => {
                            let disconnected = Command::new("tsdiscon.exe").creation_flags(CREATE_NO_WINDOW).status();
                            if !disconnected.is_ok_and(|s| s.success()) {
                                if let Err(e) = unsafe { LockWorkStation() } {
                                    log(&format!("locking failed: {e}"));
                                }
                            }
                        }
                        Action::LogOut => {
                            if let Err(e) = unsafe { ExitWindowsEx(EWX_LOGOFF, SHUTDOWN_REASON(0)) } {
                                log(&format!("logging out failed: {e}"));
                            }
                        }
                        Action::Wm(c) => desktop::command(c),
                    }
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                desktop::screen_changed();
                LRESULT(0)
            }
            WM_ENDSESSION | WM_DESTROY => {
                desktop::release();
                if msg == WM_DESTROY {
                    unsafe { PostQuitMessage(0) };
                }
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(window, msg, wparam, lparam) },
        }
    }
}
