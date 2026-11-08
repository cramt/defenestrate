//! Keybindings, COSMIC's defaults (cosmic-comp's data/keybindings.ron)
//! where this shell has the feature, through a low-level keyboard hook:
//! RegisterHotKey can't have most Win+ combos even with explorer gone
//! ("Hot key is already registered"), and only a hook sees a lone Super
//! tap.
//!
//! Super+L is "focus right" here, but Windows locks the session on Win+L
//! before any hook can swallow it, so configs/default.nix turns that off
//! (DisableLockWorkstation); Super+Escape locks instead, as in COSMIC.
//!
//! The hook only decides which binding a key is and queues it. Windows
//! silently drops low-level hooks that take too long, so the work happens
//! in the message loop when WM_ACTION arrives.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU, VK_OEM_2, VK_RETURN, VK_RIGHT,
    VK_RWIN, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN,
    WM_SYSKEYDOWN,
};

use crate::tree::Direction;
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

thread_local! {
    static SUPER: Cell<Super> = const { Cell::new(Super::Up) };
    static MODE: Cell<Mode> = const { Cell::new(Mode::Normal) };
    static QUEUE: RefCell<VecDeque<Action>> = const { RefCell::new(VecDeque::new()) };
    static NOTIFY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

pub fn install(instance: HINSTANCE, notify: HWND) -> windows::core::Result<()> {
    NOTIFY.set(notify);
    // The hook lives as long as this thread's message loop runs.
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(instance), 0) }.map(|_| ())
}

/// The queued actions, oldest first.
pub fn take() -> Vec<Action> {
    QUEUE.with_borrow_mut(|q| q.drain(..).collect())
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
    QUEUE.with_borrow_mut(|q| q.push_back(action));
    unsafe {
        let _ = PostMessageW(Some(NOTIFY.get()), WM_ACTION, WPARAM(0), LPARAM(0));
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
