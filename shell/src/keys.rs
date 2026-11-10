//! Keybindings, COSMIC's defaults (cosmic-comp's data/keybindings.ron)
//! where this shell has the feature, through a low-level keyboard hook:
//! RegisterHotKey can't have most Win+ combos even with explorer gone
//! ("Hot key is already registered"), and only a hook sees a lone Super
//! tap.
//!
//! Super+L is "focus right" here, but Windows locks the session on Win+L
//! before any hook can swallow it, so configs/default.nix turns that off
//! (DisableLockWorkstation); Super+Escape locks instead, as in COSMIC,
//! by disconnecting the session (main.rs), since the policy blocks
//! LockWorkStation as well.
//!
//! Windows silently and permanently drops a low-level hook whose thread
//! doesn't answer within LowLevelHooksTimeout. On a flashed machine's first
//! boot (drivers settling, apps installing) the shell's GUI thread stalls
//! long enough for that, and the keys were gone for the session. So the
//! hook gets a thread of its own, at high priority, that only decides
//! which binding a key is and queues it; the GUI thread does the work when
//! WM_ACTION arrives.
//!
//! A low-level mouse hook on the same thread turns Super+left-drag and
//! Super+right-drag into COSMIC's move and resize grabs. Like COSMIC, the
//! grab lasts until the button comes up, even if Super goes first.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU, VK_OEM_2, VK_RETURN, VK_RIGHT,
    VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PostMessageW,
    SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
};

use crate::tree::{Direction, Point};
use crate::wm::{Command, Step, WorkspaceId};

pub const WM_ACTION: u32 = WM_APP + 2;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Launcher,
    Terminal,
    HomeFolder,
    Lock,
    LogOut,
    Wm(Command),
    Drag(Drag),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Moves the window.
    Left,
    /// Resizes it from the corner nearest the pointer.
    Right,
}

/// A Super+drag, in screen pixels.
#[derive(Clone, Copy, Debug)]
pub enum Drag {
    Start(Button, Point),
    Move(Point),
    End(Point),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Super {
    Up,
    /// Held; `chorded` once another key went down with it, so releasing
    /// it isn't a tap.
    Down { chorded: bool },
}

/// COSMIC's Super+R: until Enter or Escape, the direction keys alone move
/// the focused window's edges.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Resize { outwards: bool },
}

// Hook thread only.
thread_local! {
    static SUPER: Cell<Super> = const { Cell::new(Super::Up) };
    static MODE: Cell<Mode> = const { Cell::new(Mode::Normal) };
    static NOTIFY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// The button holding a Super+drag, until it comes up.
    static DRAGGING: Cell<Option<Button>> = const { Cell::new(None) };
}

// Filled by the hook thread, drained by the GUI thread.
static QUEUE: Mutex<VecDeque<Action>> = Mutex::new(VecDeque::new());

/// Starts the hook thread; returns once its hook is in (or failed).
pub fn install(notify: HWND) -> windows::core::Result<()> {
    // HWNDs aren't Send; the address is all the thread needs.
    let notify = notify.0 as isize;
    let (done, installed) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        NOTIFY.set(HWND(notify as *mut _));
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        let module = GetModuleHandleW(None).ok();
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module.map(Into::into), 0);
        let ok = hook.is_ok();
        // Dragging is a nicety; the keys still work without it.
        if ok && let Err(e) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module.map(Into::into), 0) {
            crate::log(&format!("installing the mouse hook failed: {e}"));
        }
        let _ = done.send(hook.map(|_| ()));
        if ok {
            // The hook is called from this loop, so it lives as long as it runs.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        }
    });
    installed.recv().unwrap_or_else(|_| Err(windows::core::Error::from_thread()))
}

/// The queued actions, oldest first.
pub fn take() -> Vec<Action> {
    QUEUE.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

fn letter(key: VIRTUAL_KEY) -> Option<char> {
    char::from_u32(u32::from(key.0)).filter(char::is_ascii_alphanumeric)
}

/// Vim keys and arrows, as in COSMIC.
fn direction(key: VIRTUAL_KEY) -> Option<Direction> {
    match (key, letter(key)) {
        (VK_LEFT, _) | (_, Some('H')) => Some(Direction::Left),
        (VK_DOWN, _) | (_, Some('J')) => Some(Direction::Down),
        (VK_UP, _) | (_, Some('K')) => Some(Direction::Up),
        (VK_RIGHT, _) | (_, Some('L')) => Some(Direction::Right),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct Mods {
    shift: bool,
    ctrl: bool,
    alt: bool,
}

enum Binding {
    Do(Action),
    EnterResize { outwards: bool },
}

fn binding(key: VIRTUAL_KEY, m: Mods) -> Option<Binding> {
    use Command::*;
    let wm = |c| Some(Binding::Do(Action::Wm(c)));
    let act = |a| Some(Binding::Do(a));
    if let Some(dir) = direction(key) {
        let step = match dir {
            Direction::Left | Direction::Up => Step::Previous,
            Direction::Right | Direction::Down => Step::Next,
        };
        return match (m.shift, m.ctrl, m.alt) {
            (false, false, false) => wm(Focus(dir)),
            (true, false, false) => wm(Move(dir)),
            (false, true, false) => wm(SwitchStep(step)),
            (true, true, false) => wm(SendStep(step)),
            // Super+Alt is for other monitors, not managed yet.
            _ => None,
        };
    }
    if m.ctrl || m.alt {
        return None;
    }
    match (key, m.shift) {
        (VK_ESCAPE, false) => act(Action::Lock),
        (VK_ESCAPE, true) => act(Action::LogOut),
        // Not COSMIC's, but what this shell had before; COSMIC's is Super+T.
        (VK_RETURN, false) => act(Action::Terminal),
        (VK_OEM_2, false) => act(Action::Launcher),
        _ => match (letter(key)?, m.shift) {
            (d @ '1'..='9', shift) => {
                let ws = WorkspaceId::from_key(d.to_digit(10)?)?;
                wm(if shift { Send(ws) } else { Switch(ws) })
            }
            ('Q', false) => wm(Close),
            ('O', false) => wm(ToggleOrientation),
            ('Y', false) => wm(ToggleTiling),
            ('G', false) => wm(ToggleFloating),
            ('M', false) => wm(Maximize),
            ('R', shift) => Some(Binding::EnterResize { outwards: !shift }),
            ('T', false) => act(Action::Terminal),
            ('F', false) => act(Action::HomeFolder),
            // COSMIC's App Library; the launcher is the closest thing.
            ('A', false) => act(Action::Launcher),
            _ => None,
        },
    }
}

fn held(key: VIRTUAL_KEY) -> bool {
    // The high bit is "down right now".
    unsafe { GetAsyncKeyState(key.0.into()) < 0 }
}

fn queue(action: Action) {
    let Ok(mut q) = QUEUE.lock() else { return };
    // Only where the pointer is now matters, and a mouse sends a lot of
    // moves; the GUI thread mustn't fall behind on them.
    if let Action::Drag(Drag::Move(at)) = action
        && let Some(Action::Drag(Drag::Move(last))) = q.back_mut()
    {
        *last = at;
        return;
    }
    // One WM_ACTION drains the whole queue, so one is enough until then.
    let idle = q.is_empty();
    q.push_back(action);
    drop(q);
    if idle {
        unsafe {
            let _ = PostMessageW(Some(NOTIFY.get()), WM_ACTION, WPARAM(0), LPARAM(0));
        }
    }
}

/// True if the key was handled and must not reach the focused app.
fn on_key_down(key: VIRTUAL_KEY) -> bool {
    // Low-level hooks see the left/right variants (VK_LSHIFT...), never
    // VK_SHIFT. Modifiers going down neither end a mode nor chord Super.
    if (0xA0..=0xA5).contains(&key.0) {
        return false;
    }
    if let Mode::Resize { outwards } = MODE.get() {
        if let Some(dir) = direction(key) {
            queue(Action::Wm(Command::Resize { dir, outwards }));
            return true;
        }
        MODE.set(Mode::Normal);
        // Enter and Escape only end the mode; anything else ends it and
        // still goes where it was going.
        if key == VK_RETURN || key == VK_ESCAPE {
            return true;
        }
    }
    if SUPER.get() == Super::Up {
        return false;
    }
    SUPER.set(Super::Down { chorded: true });
    let mods = Mods { shift: held(VK_SHIFT), ctrl: held(VK_CONTROL), alt: held(VK_MENU) };
    match binding(key, mods) {
        Some(Binding::Do(action)) => queue(action),
        Some(Binding::EnterResize { outwards }) => MODE.set(Mode::Resize { outwards }),
        None => return false,
    }
    true
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let key = VIRTUAL_KEY(unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }.vkCode as u16);
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if key == VK_LWIN || key == VK_RWIN {
            match (down, SUPER.get()) {
                // Autorepeat sends more downs; they mustn't reset `chorded`.
                (true, Super::Up) => SUPER.set(Super::Down { chorded: false }),
                (true, Super::Down { .. }) => {}
                (false, state) => {
                    SUPER.set(Super::Up);
                    if state == (Super::Down { chorded: false }) {
                        queue(Action::Launcher);
                    }
                }
            }
        } else if down && on_key_down(key) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Super+button starts a drag, which then swallows that button's release
/// too, so the app under the pointer never sees half a click. A drag counts
/// as a chord: releasing Super afterwards doesn't open the launcher.
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let pt = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) }.pt;
        let at = Point { x: pt.x, y: pt.y };
        match (wparam.0 as u32, DRAGGING.get()) {
            (msg @ (WM_LBUTTONDOWN | WM_RBUTTONDOWN), None) if SUPER.get() != Super::Up => {
                SUPER.set(Super::Down { chorded: true });
                let button = if msg == WM_LBUTTONDOWN { Button::Left } else { Button::Right };
                DRAGGING.set(Some(button));
                queue(Action::Drag(Drag::Start(button, at)));
                return LRESULT(1);
            }
            (WM_MOUSEMOVE, Some(_)) => queue(Action::Drag(Drag::Move(at))),
            (WM_LBUTTONUP, Some(Button::Left)) | (WM_RBUTTONUP, Some(Button::Right)) => {
                DRAGGING.set(None);
                queue(Action::Drag(Drag::End(at)));
                return LRESULT(1);
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
