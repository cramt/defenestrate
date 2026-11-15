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
//!
//! Every change in which modifiers are held starts a new `Hold`, COSMIC's
//! `last_modifier_change`: a few of its moves across monitors depend on
//! whether Super stayed down in between.
//!
//! The config's `cosmic.shortcuts` (settings.rs) go first, as COSMIC's
//! custom shortcuts replace its defaults: a combination bound there does
//! what it says, and one set to Disable reaches Windows and the app as if
//! the shell had no binding for it.
//!
//! Alt+Tab and Super+Tab (Shift for backwards) are COSMIC's window
//! switcher, swallowed here: without explorer, Windows' own Alt+Tab still
//! switches but shows nothing, and can't reach windows on other workspaces,
//! which the shell has hidden. As in cosmic-launcher, the switch happens
//! once none of Alt, Super and Ctrl is held any more, and Escape cancels.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL,
    VK_DOWN, VK_ESCAPE, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LWIN, VK_MENU, VK_OEM_2, VK_RCONTROL, VK_RETURN, VK_RIGHT,
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_RMENU, VK_RWIN, VK_SHIFT, VK_TAB, VK_UP,
    VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PostMessageW,
    SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::settings::{self, Keys, System};
use crate::tree::{Direction, Point};
use crate::wm::{Command, Hold, Step, WorkspaceId};

pub const WM_ACTION: u32 = WM_APP + 2;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Launcher,
    Terminal,
    HomeFolder,
    Lock,
    LogOut,
    /// A configured command line (the settings live for the whole run).
    Spawn(&'static str),
    /// A media or volume key, sent on to whatever handles it.
    Media(VIRTUAL_KEY),
    Wm(Command, Hold),
    Drag(Drag),
    Switcher(Switch),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// Opens the switcher if it isn't, then moves the selection.
    Step(Step),
    /// The held modifiers came up: switch to the selection.
    Done,
    Cancel,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    /// COSMIC's Super+R: until Enter or Escape, the direction keys alone
    /// move the focused window's edges.
    Resize { outwards: bool },
    /// The window switcher is up until the modifiers that opened it are
    /// let go.
    Switching,
}

/// What keeps the switcher open.
const SWITCH_MODIFIERS: [VIRTUAL_KEY; 6] = [VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL];
/// Unassigned, so no app does anything with it (AutoHotkey's menu mask key).
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

// Hook thread only.
thread_local! {
    static SUPER: Cell<Super> = const { Cell::new(Super::Up) };
    static MODE: Cell<Mode> = const { Cell::new(Mode::Normal) };
    static NOTIFY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    /// The button holding a Super+drag, until it comes up.
    static DRAGGING: Cell<Option<Button>> = const { Cell::new(None) };
    /// Which modifier keys are down, one bit each, so autorepeat doesn't
    /// count as a change.
    static MODIFIERS: Cell<u32> = const { Cell::new(0) };
}

static HOLD: AtomicU64 = AtomicU64::new(0);

fn hold() -> Hold {
    Hold(HOLD.load(Ordering::Relaxed))
}

/// Starts a new hold if `key` is a modifier whose state changed.
fn track_modifier(key: VIRTUAL_KEY, down: bool) {
    let bit = match key.0 {
        k @ 0xA0..=0xA5 => 1 << (k - 0xA0),
        _ if key == VK_LWIN => 1 << 6,
        _ if key == VK_RWIN => 1 << 7,
        _ => return,
    };
    let was = MODIFIERS.get();
    let now = if down { was | bit } else { was & !bit };
    if now != was {
        MODIFIERS.set(now);
        HOLD.fetch_add(1, Ordering::Relaxed);
    }
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
    /// Bound in COSMIC, but does nothing: still kept from the app.
    Nothing,
}

fn binding(key: VIRTUAL_KEY, m: Mods) -> Option<Binding> {
    use Command::*;
    let wm = |c| Some(Binding::Do(Action::Wm(c, hold())));
    let act = |a| Some(Binding::Do(a));
    if let Some(dir) = direction(key) {
        let step = match dir {
            Direction::Left | Direction::Up => Step::Previous,
            Direction::Right | Direction::Down => Step::Next,
        };
        // COSMIC's Previous/NextWorkspace bindings ignore left and right
        // while workspaces are laid out vertically, its default.
        let vertical = matches!(dir, Direction::Up | Direction::Down);
        return match (m.shift, m.ctrl, m.alt) {
            (false, false, false) => wm(Focus(dir)),
            (true, false, false) => wm(Move(dir)),
            (false, true, false) if vertical => wm(SwitchStep(step)),
            (true, true, false) if vertical => wm(MoveStep(step)),
            (_, true, false) => Some(Binding::Nothing),
            (false, false, true) => wm(SwitchOutput(dir)),
            (true, false, true) => wm(MoveToOutput(dir)),
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
                wm(if shift { MoveTo(ws) } else { Switch(ws) })
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

/// What a configured shortcut does; None for Disable, or a workspace that
/// doesn't exist (settings.rs refuses those).
fn configured(action: &'static settings::Action) -> Option<Action> {
    use Command::*;
    use settings::Action as A;
    let dir = |d| match d {
        settings::Direction::Left => Direction::Left,
        settings::Direction::Right => Direction::Right,
        settings::Direction::Up => Direction::Up,
        settings::Direction::Down => Direction::Down,
    };
    let ws = |n: u8| WorkspaceId::from_key(n.into());
    let wm = |c| Some(Action::Wm(c, hold()));
    match *action {
        A::Disable => None,
        A::Spawn(ref line) => Some(Action::Spawn(line)),
        A::System(s) => Some(match s {
            System::Launcher | System::AppLibrary => Action::Launcher,
            System::Terminal => Action::Terminal,
            System::HomeFolder => Action::HomeFolder,
            System::LockScreen => Action::Lock,
            System::LogOut => Action::LogOut,
            System::Mute => Action::Media(VK_VOLUME_MUTE),
            System::VolumeLower => Action::Media(VK_VOLUME_DOWN),
            System::VolumeRaise => Action::Media(VK_VOLUME_UP),
            System::PlayPause => Action::Media(VK_MEDIA_PLAY_PAUSE),
            System::PlayNext => Action::Media(VK_MEDIA_NEXT_TRACK),
            System::PlayPrev => Action::Media(VK_MEDIA_PREV_TRACK),
        }),
        A::Close => wm(Close),
        A::Maximize => wm(Maximize),
        A::ToggleTiling => wm(ToggleTiling),
        A::ToggleWindowFloating => wm(ToggleFloating),
        A::ToggleOrientation => wm(ToggleOrientation),
        A::Focus(d) => wm(Focus(dir(d))),
        A::Move(d) => wm(Move(dir(d))),
        A::Workspace(n) => wm(Switch(ws(n)?)),
        A::MoveToWorkspace(n) => wm(MoveTo(ws(n)?)),
        A::NextWorkspace => wm(SwitchStep(Step::Next)),
        A::PreviousWorkspace => wm(SwitchStep(Step::Previous)),
        A::MoveToNextWorkspace => wm(MoveStep(Step::Next)),
        A::MoveToPreviousWorkspace => wm(MoveStep(Step::Previous)),
        A::SwitchOutput(d) => wm(SwitchOutput(dir(d))),
        A::MoveToOutput(d) => wm(MoveToOutput(dir(d))),
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

/// The app under an Alt+Tab saw Alt go down and will see it come up; with
/// nothing in between, that's a tap, and a tap of Alt opens its menu bar.
/// Windows' own Alt+Tab doesn't have that problem; ours sends a key that
/// means nothing in between.
fn mask_alt() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: MASK_KEY, dwFlags: flags, ..Default::default() } },
    };
    unsafe {
        SendInput(&[key(Default::default()), key(KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
    }
}

fn on_switching_key(key: VIRTUAL_KEY) -> bool {
    let step = match key {
        VK_TAB if held(VK_SHIFT) => Step::Previous,
        VK_TAB | VK_DOWN => Step::Next,
        VK_UP => Step::Previous,
        VK_ESCAPE => {
            MODE.set(Mode::Normal);
            queue(Action::Switcher(Switch::Cancel));
            return true;
        }
        _ => return false,
    };
    queue(Action::Switcher(Switch::Step(step)));
    true
}

/// True if the key was handled and must not reach the focused app.
fn on_key_down(key: VIRTUAL_KEY) -> bool {
    // Low-level hooks see the left/right variants (VK_LSHIFT...), never
    // VK_SHIFT. Modifiers going down neither end a mode nor chord Super.
    if (0xA0..=0xA5).contains(&key.0) {
        return false;
    }
    if MODE.get() == Mode::Switching {
        if SWITCH_MODIFIERS.iter().any(|&m| held(m)) {
            return on_switching_key(key);
        }
        // Their release went somewhere the hook can't see (an elevated
        // window took focus); a plain Tab mustn't stay swallowed.
        MODE.set(Mode::Normal);
        queue(Action::Switcher(Switch::Cancel));
    }
    if let Mode::Resize { outwards } = MODE.get() {
        if let Some(dir) = direction(key) {
            queue(Action::Wm(Command::Resize { dir, outwards }, hold()));
            return true;
        }
        MODE.set(Mode::Normal);
        // Enter and Escape only end the mode; anything else ends it and
        // still goes where it was going.
        if key == VK_RETURN || key == VK_ESCAPE {
            return true;
        }
    }
    let super_down = SUPER.get() != Super::Up;
    let alt = held(VK_MENU);
    // Virtual-key codes all fit a byte.
    let keys = Keys { logo: super_down, ctrl: held(VK_CONTROL), alt, shift: held(VK_SHIFT), vk: key.0 as u8 };
    if let Some(action) = settings::get().shortcut(keys) {
        if super_down {
            SUPER.set(Super::Down { chorded: true });
        }
        return match configured(action) {
            Some(action) => {
                queue(action);
                true
            }
            None => false,
        };
    }
    // COSMIC binds exactly Alt+Tab and Super+Tab, each with or without Shift.
    if key == VK_TAB && !held(VK_CONTROL) && (alt != super_down) {
        if super_down {
            SUPER.set(Super::Down { chorded: true });
        } else {
            // Not from inside the hook: input sent from here goes through the
            // hook again, and a hook that's slow to return gets bypassed, which
            // let the Tab through to Windows' own (invisible) Alt+Tab.
            std::thread::spawn(mask_alt);
        }
        MODE.set(Mode::Switching);
        return on_switching_key(key);
    }
    if !super_down {
        return false;
    }
    SUPER.set(Super::Down { chorded: true });
    let mods = Mods { shift: held(VK_SHIFT), ctrl: held(VK_CONTROL), alt: held(VK_MENU) };
    match binding(key, mods) {
        Some(Binding::Do(action)) => queue(action),
        Some(Binding::EnterResize { outwards }) => MODE.set(Mode::Resize { outwards }),
        Some(Binding::Nothing) => {}
        None => return false,
    }
    // Super+Alt+direction: the app saw Alt go down and will see it come up
    // with nothing in between, which opens its menu bar, as with Alt+Tab.
    if mods.alt {
        std::thread::spawn(mask_alt);
    }
    true
}

/// While switching, the last of Alt, Super and Ctrl coming up switches. The
/// key coming up still reads as held: the hook runs before Windows updates
/// the key state.
fn on_key_up(key: VIRTUAL_KEY) {
    if MODE.get() == Mode::Switching
        && SWITCH_MODIFIERS.contains(&key)
        && !SWITCH_MODIFIERS.iter().any(|&m| m != key && held(m))
    {
        MODE.set(Mode::Normal);
        queue(Action::Switcher(Switch::Done));
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let key = VIRTUAL_KEY(unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }.vkCode as u16);
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        track_modifier(key, down);
        if matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP) {
            on_key_up(key);
        }
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
