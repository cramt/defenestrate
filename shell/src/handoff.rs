//! The two builds of this crate (Cargo.toml's [[bin]]s, one source, one
//! build) and how the session goes from one to the other.
//!
//! defenestrate-shell-uiaccess.exe has a manifest asking for uiAccess
//! (build.rs) and is signed (sign.sh), which lets it reach elevated windows.
//! A plain CreateProcess of it from a process without uiAccess fails with
//! ERROR_ELEVATION_REQUIRED: like an elevation, it has to go through
//! ShellExecuteEx, which hands it to the AppInfo service to check the
//! signature and grant uiAccess. userinit only does CreateProcess, so as
//! the Shell value it starts nothing at all (all seen in the VM,
//! 2026-11-15; cmd.exe, which retries a 740 with ShellExecuteEx, starts it
//! fine). So the Shell value names defenestrate-shell.exe, which has no
//! manifest: it hands over to the uiAccess build and exits, or, when Windows
//! won't start that, is the shell itself, without uiAccess.

use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::Threading::{CreateMutexW, GetProcessId};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Which binary this is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Build {
    /// defenestrate-shell.exe, what Winlogon starts.
    Launcher,
    /// defenestrate-shell-uiaccess.exe.
    UiAccess,
}

/// The uiAccess build's file name, next to the launcher.
pub const UIACCESS_EXE: &str = "defenestrate-shell-uiaccess.exe";

pub const BUILD: Build = if same(env!("CARGO_BIN_NAME").as_bytes(), b"defenestrate-shell-uiaccess") {
    Build::UiAccess
} else if same(env!("CARGO_BIN_NAME").as_bytes(), b"defenestrate-shell") {
    Build::Launcher
} else {
    panic!("a binary Cargo.toml doesn't declare")
};

/// str equality isn't const yet.
const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// How long the launcher waits on the hand-over before being the shell
/// itself. At logon, an earlier launcher's CreateProcess of the uiAccess
/// build hung with no error (VM); ShellExecuteEx hasn't, but a session with
/// no shell at all is the one outcome worth a timeout.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Handover {
    Started { pid: u32 },
    /// Why not, for the log.
    Failed(String),
}

/// Starts the uiAccess build from the launcher's directory.
pub fn hand_over() -> Handover {
    let exe = match std::env::current_exe() {
        Ok(me) => me.with_file_name(UIACCESS_EXE),
        Err(e) => return Handover::Failed(format!("finding {UIACCESS_EXE}: {e}")),
    };
    let (tx, rx) = mpsc::channel();
    // Its own thread, so a launch that never returns can't keep the session
    // from having a shell: the timeout below runs it here instead, and if the
    // other one turns up after all, claim_session() makes it leave.
    std::thread::spawn(move || {
        let _ = tx.send(shell_execute(&exe));
    });
    match rx.recv_timeout(HANDOVER_TIMEOUT) {
        Ok(Ok(pid)) => Handover::Started { pid },
        Ok(Err(e)) => Handover::Failed(explain(&e)),
        Err(_) => Handover::Failed(format!("starting {UIACCESS_EXE} didn't return within {HANDOVER_TIMEOUT:?}")),
    }
}

fn shell_execute(exe: &std::path::Path) -> windows::core::Result<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // No error dialogs (there may be nothing to show them over yet), and
        // done before returning, since this thread exits right after.
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        // ShellExecuteEx wants COM, single-threaded, as its docs say.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        ShellExecuteExW(&mut info)?;
        let pid = GetProcessId(info.hProcess);
        let _ = CloseHandle(info.hProcess);
        Ok(pid)
    }
}

fn explain(e: &windows::core::Error) -> String {
    // HRESULT_FROM_WIN32 of the error codes ShellExecuteEx sets.
    let win32 = |code: u32| (0x8007_0000 | code) as i32;
    let why = match e.code().0 {
        c if c == win32(2) || c == win32(3) => {
            "it isn't installed (an install from before it existed: `dfnstrate update` adds it)"
        }
        // ERROR_DS_REFERRAL, "A referral was returned from the server": what
        // ShellExecuteEx said with our certificate taken out of Root (VM).
        c if c == win32(8235) => {
            "Windows doesn't trust its signature: is its certificate (CN=defenestrate shell) still in the machine's Trusted Root store?"
        }
        _ => "unexpected",
    };
    format!("starting {UIACCESS_EXE} failed ({e}): {why}")
}

/// Makes this process the session's one shell. False when another already
/// is: say the uiAccess build arriving after the launcher gave up waiting
/// and became the shell itself. The mutex is held until the process exits.
pub fn claim_session() -> bool {
    // HANDLE has no Drop, so the mutex stays open for the life of the process.
    match unsafe { CreateMutexW(None, false, w!("Local\\defenestrate-shell")) } {
        Ok(_) => (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS,
        // Exists, made by a process this one can't open (the uiAccess build).
        Err(e) if e.code() == E_ACCESSDENIED => false,
        // Better two shells than none.
        Err(_) => true,
    }
}
