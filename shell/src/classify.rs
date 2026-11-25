//! Which windows tile, and which the window switcher lists. Pure, so the
//! rules are tested on the build machine; desktop.rs gathers the facts from
//! the real window.
//!
//! "Has a sizing border" alone missed real app windows: launchers and
//! installers are often a fixed size (Riot Client: caption, minimize box, no
//! sizing border), and some draw their own frame (League's lobby: no
//! caption at all, still a minimize box). A minimize box is what sets an
//! app window apart from a dialog, which nothing minimizes on its own.
//! Whatever still tiles wrongly is what COSMIC's tiling exceptions are for
//! (settings.rs).

use std::fmt;

use crate::settings::TilingException;

// winuser.h. WS_MINIMIZEBOX shares its bit with WS_GROUP, which only means
// something on a child window, and children never tile.
const WS_CHILD: u32 = 0x4000_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;
const WS_MINIMIZEBOX: u32 = 0x0002_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

/// What the shell knows about a visible window from the window itself.
pub struct Facts<'a> {
    pub root: bool,
    pub owned: bool,
    pub style: u32,
    pub ex_style: u32,
    pub title: &'a str,
    pub class: &'a str,
    pub ours: bool,
}

/// What the window's process says. Only tiling asks: the switcher lists
/// windows without opening anyone's process.
pub struct Process<'a> {
    /// `app_id` of its exe, Windows' nearest thing to an app ID. None when
    /// its process can't be opened (a protected one, say, as anti-cheat
    /// makes games); then no tiling exception can match it.
    pub app: Option<&'a str>,
    /// Elevated, or a process the shell can't even open, while the shell
    /// runs without uiAccess and so may not be able to move it.
    pub out_of_reach: bool,
}

impl Facts<'_> {
    /// Of the visible windows, what Windows' Alt+Tab lists: unowned
    /// top-level windows with a title that aren't tool windows, here not
    /// the shell's own.
    pub fn switchable(&self) -> bool {
        self.root && !self.title.is_empty() && !self.owned && self.ex_style & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) == 0 && !self.ours
    }
}

/// What tiling exceptions match as the app ID: the exe's file name, less a
/// `.exe` or `.EXE` (any other casing stays on).
pub fn app_id(exe_path: &str) -> String {
    let name = exe_path.rsplit('\\').next().unwrap_or(exe_path);
    name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Tile,
    Float(Why),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    NotRoot,
    Child,
    /// The stand-in Windows shows for a window that stopped responding
    /// (class `Ghost`), which goes when the app answers again. Tiling it
    /// made a second tile for the same app, and placing it failed with
    /// "Access is denied" (Discord, saturn 2026-11-19).
    Ghost,
    Owned,
    ToolWindow,
    NoActivate,
    /// Always on top: splash screens (Squirrel's "Installing..."), overlays
    /// and fullscreen games, none of which belong in a tile.
    Topmost,
    /// Neither sizable nor minimizable.
    Dialog,
    Ours,
    OutOfReach,
    /// Matched `tiling_exception_custom[n]`.
    Exception(usize),
}

impl Verdict {
    /// Whether a log line about this verdict says anything: popups, menus
    /// and tooltips float every time they show, and would drown the rest.
    pub fn worth_logging(self) -> bool {
        !matches!(self, Verdict::Float(Why::NotRoot | Why::Child | Why::ToolWindow | Why::NoActivate | Why::Ours))
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let why = match self {
            Verdict::Tile => return f.write_str("tiled"),
            Verdict::Float(why) => why,
        };
        f.write_str("floating: ")?;
        match why {
            Why::NotRoot => f.write_str("not a top-level window"),
            Why::Child => f.write_str("WS_CHILD"),
            Why::Ghost => f.write_str("a hung window's ghost"),
            Why::Owned => f.write_str("owned"),
            Why::ToolWindow => f.write_str("WS_EX_TOOLWINDOW"),
            Why::NoActivate => f.write_str("WS_EX_NOACTIVATE"),
            Why::Topmost => f.write_str("WS_EX_TOPMOST"),
            Why::Dialog => f.write_str("no WS_THICKFRAME or WS_MINIMIZEBOX"),
            Why::Ours => f.write_str("the shell's own"),
            Why::OutOfReach => f.write_str("elevated or unopenable, and the shell has no uiAccess"),
            Why::Exception(n) => write!(f, "tiling_exception_custom[{n}]"),
        }
    }
}

pub fn classify(w: &Facts, process: &Process, exceptions: &[TilingException]) -> Verdict {
    let float = Verdict::Float;
    if !w.root {
        return float(Why::NotRoot);
    }
    if w.style & WS_CHILD != 0 {
        return float(Why::Child);
    }
    if w.class == "Ghost" {
        return float(Why::Ghost);
    }
    if w.owned {
        return float(Why::Owned);
    }
    if w.ex_style & WS_EX_TOOLWINDOW != 0 {
        return float(Why::ToolWindow);
    }
    if w.ex_style & WS_EX_NOACTIVATE != 0 {
        return float(Why::NoActivate);
    }
    if w.ex_style & WS_EX_TOPMOST != 0 {
        return float(Why::Topmost);
    }
    if w.ours {
        return float(Why::Ours);
    }
    // A window the shell can't move would sit on top of the tiles.
    if process.out_of_reach {
        return float(Why::OutOfReach);
    }
    if w.style & (WS_THICKFRAME | WS_MINIMIZEBOX) == 0 {
        return float(Why::Dialog);
    }
    if let Some(n) = process.app.and_then(|app| exceptions.iter().position(|e| e.matches(app, w.title))) {
        return float(Why::Exception(n));
    }
    Verdict::Tile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(style: u32, ex_style: u32) -> Facts<'static> {
        Facts { root: true, owned: false, style, ex_style, title: "t", class: "C", ours: false }
    }

    const APP: Process = Process { app: Some("app"), out_of_reach: false };

    /// Captured on saturn, 2026-11-16 (windows-bugs.md #5).
    #[test]
    fn fixed_size_and_borderless_app_windows_tile() {
        let riot_client = facts(0x14C2_0000, 0x0020_0100);
        let league_lobby = facts(0x1402_0000, 0);
        assert_eq!(classify(&riot_client, &APP, &[]), Verdict::Tile);
        assert_eq!(classify(&league_lobby, &APP, &[]), Verdict::Tile);
    }

    /// Squirrel's "Installing..." splash, Discord's installer (windows-bugs.md #15).
    #[test]
    fn topmost_splashes_float() {
        assert_eq!(classify(&facts(0x160f_0000, 0x000c_0108), &APP, &[]), Verdict::Float(Why::Topmost));
    }

    /// Discord's ghost while it hung, saturn 2026-11-19: sizable, with a
    /// process the shell can't open, which tiled.
    #[test]
    fn a_hung_windows_ghost_floats() {
        let ghost = Facts { class: "Ghost", title: "discord.com/app - Discord", ..facts(0x14CF_0000, 0) };
        assert_eq!(classify(&ghost, &Process { app: None, ..APP }, &[]), Verdict::Float(Why::Ghost));
    }

    #[test]
    fn resizable_windows_tile() {
        // WS_OVERLAPPEDWINDOW | WS_VISIBLE
        assert_eq!(classify(&facts(0x10CF_0000, 0x100), &APP, &[]), Verdict::Tile);
    }

    #[test]
    fn dialogs_float() {
        // WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE, unowned: no way
        // to size or minimize it.
        assert_eq!(classify(&facts(0x94C8_0000, 0), &APP, &[]), Verdict::Float(Why::Dialog));
        let owned = Facts { owned: true, ..facts(0x10CF_0000, 0) };
        assert_eq!(classify(&owned, &APP, &[]), Verdict::Float(Why::Owned));
    }

    #[test]
    fn exceptions_float_what_would_tile() {
        let ex = [TilingException::new("^other$", ".*").unwrap(), TilingException::new("^LeagueClientUx$", "").unwrap()];
        let lobby = Facts { title: "League of Legends", ..facts(0x1402_0000, 0) };
        assert_eq!(classify(&lobby, &Process { app: Some("LeagueClientUx"), ..APP }, &ex), Verdict::Float(Why::Exception(1)));
        assert_eq!(classify(&facts(0x1402_0000, 0), &APP, &ex), Verdict::Tile);
    }

    /// Tiled before window rules existed, with uiAccess: that the shell
    /// can't name the process mustn't change it.
    #[test]
    fn unopenable_processes_still_tile() {
        let window = facts(0x14CF_0000, 0);
        let protected = Process { app: None, ..APP };
        assert_eq!(classify(&window, &protected, &[TilingException::new("", "").unwrap()]), Verdict::Tile);
        let without_ui_access = Process { out_of_reach: true, ..protected };
        assert_eq!(classify(&window, &without_ui_access, &[]), Verdict::Float(Why::OutOfReach));
    }

    #[test]
    fn popups_menus_and_tooltips_go_unlogged() {
        assert!(!classify(&facts(0x9400_0000, WS_EX_TOOLWINDOW), &APP, &[]).worth_logging());
        assert!(classify(&facts(0x94C8_0000, 0), &APP, &[]).worth_logging());
    }

    #[test]
    fn the_switcher_lists_titled_unowned_top_level_windows() {
        let app = facts(0x10CF_0000, 0x100);
        assert!(app.switchable());
        // Neither tiles, but Alt+Tab has both.
        assert!(facts(0x94C8_0000, 0).switchable(), "a dialog");
        assert!(facts(0x160f_0000, WS_EX_TOPMOST).switchable(), "a topmost window");
        assert!(!Facts { root: false, ..facts(0x10CF_0000, 0) }.switchable());
        assert!(!Facts { title: "", ..facts(0x10CF_0000, 0) }.switchable());
        assert!(!Facts { owned: true, ..facts(0x10CF_0000, 0) }.switchable());
        assert!(!facts(0x10CF_0000, WS_EX_TOOLWINDOW).switchable());
        assert!(!facts(0x10CF_0000, WS_EX_NOACTIVATE).switchable());
        assert!(!Facts { ours: true, ..app }.switchable());
    }

    #[test]
    fn app_ids_drop_only_an_all_lower_or_all_upper_exe() {
        assert_eq!(app_id(r"C:\Riot Games\League of Legends\LeagueClientUx.exe"), "LeagueClientUx");
        assert_eq!(app_id(r"C:\WINDOWS\SYSTEM32\CMD.EXE"), "CMD");
        assert_eq!(app_id(r"C:\odd\Setup.Exe"), "Setup.Exe");
        assert_eq!(app_id(r"C:\Windows\System32\scrnsave.scr"), "scrnsave.scr");
        assert_eq!(app_id("bare.exe"), "bare");
    }
}

