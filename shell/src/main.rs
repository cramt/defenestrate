//! The shell Winlogon starts instead of explorer.exe: a tiling window
//! manager (wm.rs, desktop.rs), a Super-tap launcher (launcher.rs), an
//! Alt+Tab window switcher (switcher.rs), and Super+Enter for a terminal.
//! Keys are in keys.rs.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod classify;
mod cycle;
mod display;
mod follow;
mod fuzzy;
mod motion;
mod settings;
mod tree;
mod wm;

#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod env;
#[cfg(windows)]
mod handoff;
#[cfg(windows)]
mod hint;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod launcher;
#[cfg(windows)]
mod modes;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod switcher;

#[cfg(not(windows))]
fn main() {
    eprintln!("defenestrate-shell only runs on Windows; this build is for the tests");
}

#[cfg(windows)]
use windows_main::{font, log, scaled, scaled_at, use_dpi};

#[cfg(windows)]
fn main() {
    // panic = "abort" skips unwinding, but the hook still runs first, so
    // a panic leaves a line in the log instead of a silently missing shell.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    log(&format!("started, pid {}, {:?} build", std::process::id(), handoff::BUILD));
    if handoff::BUILD == handoff::Build::Launcher {
        // Winlogon doesn't restart a Shell process that dies (killed, exit
        // code 1, in the VM), so stepping aside loses no restart a crashed
        // shell would otherwise have got.
        match handoff::hand_over() {
            handoff::Handover::Started { pid } => return log(&format!("handed over to {}, pid {pid}", handoff::UIACCESS_EXE)),
            handoff::Handover::Failed(why) => log(&format!("{why}; running here, without uiAccess")),
        }
    }
    if !handoff::claim_session() {
        return log("another shell already runs in this session; leaving it be");
    }
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
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW};
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY};
    use windows::Win32::Graphics::Gdi::{
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DeleteObject, FF_MODERN, FIXED_PITCH, FW_NORMAL, HFONT,
        OUT_TT_PRECIS,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Shutdown::{EWX_LOGOFF, ExitWindowsEx, LockWorkStation, SHUTDOWN_REASON};
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage, RegisterClassW, TranslateMessage,
        WM_DESTROY, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows::core::{PCWSTR, PWSTR, Result, w};

    use crate::keys::{self, Action, WM_ACTION};
    use crate::{desktop, env, launcher, settings, switcher};

    // In 96 DPI pixels; scaled to the DPI of the monitor it's drawn on.
    const FONT_HEIGHT: i32 = 18;
    // Installed by configs/default.nix (`fonts`). GDI quietly substitutes
    // something else if it's missing.
    const FONT_FACE: windows::core::PCWSTR = w!("Iosevka NFM");
    const CREATE_NEW_CONSOLE: u32 = 0x10;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    thread_local! {
        /// The DPI the launcher and switcher draw at: that of the monitor
        /// they last opened on (Per-Monitor V2: each monitor has its own).
        static DPI: Cell<i32> = const { Cell::new(96) };
        static FONT: Cell<Option<(i32, HFONT)>> = const { Cell::new(None) };
    }

    pub fn scaled(px: i32) -> i32 {
        px * DPI.get() / 96
    }

    pub fn scaled_at(px: i32, dpi: u32) -> i32 {
        px * dpi as i32 / 96
    }

    /// The overlays are about to open on a monitor at `dpi`.
    pub fn use_dpi(dpi: u32) {
        DPI.set(dpi as i32);
    }

    /// The shell's font at the current DPI, made the first time it's needed
    /// at that DPI.
    pub fn font() -> HFONT {
        let dpi = DPI.get();
        if let Some((at, f)) = FONT.get()
            && at == dpi
        {
            return f;
        }
        let f = unsafe {
            CreateFontW(
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
            )
        };
        if let Some((_, old)) = FONT.replace(Some((dpi, f))) {
            unsafe {
                let _ = DeleteObject(old.into());
            }
        }
        f
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
            // Local time, as apps.log has it, so the two line up.
            let t = unsafe { GetLocalTime() };
            let _ = writeln!(
                f,
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {line}",
                t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
            );
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
            env::log_path();

            // Before the hook thread, which reads the shortcuts.
            let json = std::env::current_exe().ok().and_then(|exe| std::fs::read_to_string(exe.with_file_name(settings::FILE)).ok());
            if let Err(e) = settings::init(json.as_deref()) {
                log(&e);
            }

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
            if let Err(e) = launcher::create(instance) {
                log(&format!("creating the launcher failed: {e}"));
            }
            if let Err(e) = switcher::create(instance) {
                log(&format!("creating the window switcher failed: {e}"));
            }
            desktop::start(main);
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

    /// A configured Spawn: the command line as Windows parses one, program
    /// first, like a shortcut's target, rather than split by a shell.
    fn spawn_line(line: &str) {
        let mut wide: Vec<u16> = line.encode_utf16().chain([0]).collect();
        let home: Option<Vec<u16>> = std::env::var_os("USERPROFILE").map(|h| h.encode_wide().chain([0]).collect());
        let startup = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut info = PROCESS_INFORMATION::default();
        let dir = home.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let started = unsafe {
            CreateProcessW(None, Some(PWSTR(wide.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, dir, &startup, &mut info)
        };
        match started {
            Ok(()) => unsafe {
                let _ = CloseHandle(info.hThread);
                let _ = CloseHandle(info.hProcess);
            },
            Err(e) => log(&format!("starting {line:?} failed: {e}")),
        }
    }

    /// Media and volume keys do whatever Windows does with the real key.
    fn press(key: VIRTUAL_KEY) {
        let input = |flags| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key, dwFlags: flags, ..Default::default() } } };
        unsafe {
            SendInput(&[input(Default::default()), input(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
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
                        Action::Spawn(line) => spawn_line(line),
                        Action::Media(key) => press(key),
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
                        Action::Wm(c, hold) => desktop::command(c, hold),
                        Action::Drag(d) => desktop::drag(d),
                        Action::Pointer(p) => desktop::pointer_moved(p),
                        Action::Switcher(s) => switcher::on(s),
                    }
                }
                LRESULT(0)
            }
            desktop::WM_FRAME => {
                desktop::on_frame();
                LRESULT(0)
            }
            WM_TIMER => {
                desktop::on_timer(wparam.0);
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                desktop::screen_changed();
                LRESULT(0)
            }
            WM_SETTINGCHANGE if env::is_environment(lparam.0) => {
                env::reload();
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
