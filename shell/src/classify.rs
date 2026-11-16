//! Which windows tile. Pure, so the rules are tested on the build machine;
//! desktop.rs gathers the facts from the real window.
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
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

/// What the shell knows about a visible window when deciding.
pub struct Facts<'a> {
    pub root: bool,
    pub owned: bool,
    pub style: u32,
    pub ex_style: u32,
    /// The exe's name without `.exe`, Windows' nearest thing to an app ID.
    /// None when its process can't be opened (a protected one, say, as
    /// anti-cheat makes games); then no tiling exception can match it.
    pub app: Option<&'a str>,
    pub title: &'a str,
    pub ours: bool,
    /// Elevated, or a process the shell can't even open, while the shell
    /// runs without uiAccess and so may not be able to move it.
    pub out_of_reach: bool,
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
    Owned,
    ToolWindow,
    NoActivate,
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
            Why::Owned => f.write_str("owned"),
            Why::ToolWindow => f.write_str("WS_EX_TOOLWINDOW"),
            Why::NoActivate => f.write_str("WS_EX_NOACTIVATE"),
            Why::Dialog => f.write_str("no WS_THICKFRAME or WS_MINIMIZEBOX"),
            Why::Ours => f.write_str("the shell's own"),
            Why::OutOfReach => f.write_str("elevated or unopenable, and the shell has no uiAccess"),
            Why::Exception(n) => write!(f, "tiling_exception_custom[{n}]"),
        }
    }
}

pub fn classify(w: &Facts, exceptions: &[TilingException]) -> Verdict {
    let float = Verdict::Float;
    if !w.root {
        return float(Why::NotRoot);
    }
    if w.style & WS_CHILD != 0 {
        return float(Why::Child);
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
    if w.ours {
        return float(Why::Ours);
    }
    // A window the shell can't move would sit on top of the tiles.
    if w.out_of_reach {
        return float(Why::OutOfReach);
    }
    if w.style & (WS_THICKFRAME | WS_MINIMIZEBOX) == 0 {
        return float(Why::Dialog);
    }
    if let Some(n) = w.app.and_then(|app| exceptions.iter().position(|e| e.matches(app, w.title))) {
        return float(Why::Exception(n));
    }
    Verdict::Tile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(style: u32, ex_style: u32) -> Facts<'static> {
        Facts { root: true, owned: false, style, ex_style, app: Some("app"), title: "t", ours: false, out_of_reach: false }
    }

    /// Captured on saturn, 2026-11-16 (windows-bugs.md #5).
    #[test]
    fn fixed_size_and_borderless_app_windows_tile() {
        let riot_client = facts(0x14C2_0000, 0x0020_0100);
        let league_lobby = facts(0x1402_0000, 0);
        assert_eq!(classify(&riot_client, &[]), Verdict::Tile);
        assert_eq!(classify(&league_lobby, &[]), Verdict::Tile);
    }

    #[test]
    fn resizable_windows_tile() {
        // WS_OVERLAPPEDWINDOW | WS_VISIBLE
        assert_eq!(classify(&facts(0x10CF_0000, 0x100), &[]), Verdict::Tile);
    }

    #[test]
    fn dialogs_float() {
        // WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE, unowned: no way
        // to size or minimize it.
        assert_eq!(classify(&facts(0x94C8_0000, 0), &[]), Verdict::Float(Why::Dialog));
        let owned = Facts { owned: true, ..facts(0x10CF_0000, 0) };
        assert_eq!(classify(&owned, &[]), Verdict::Float(Why::Owned));
    }

    #[test]
    fn exceptions_float_what_would_tile() {
        let ex = [TilingException::new("^other$", ".*").unwrap(), TilingException::new("^LeagueClientUx$", "").unwrap()];
        let lobby = Facts { app: Some("LeagueClientUx"), title: "League of Legends", ..facts(0x1402_0000, 0) };
        assert_eq!(classify(&lobby, &ex), Verdict::Float(Why::Exception(1)));
        assert_eq!(classify(&facts(0x1402_0000, 0), &ex), Verdict::Tile);
    }

    /// Tiled before window rules existed, with uiAccess: that the shell
    /// can't name the process mustn't change it.
    #[test]
    fn unopenable_processes_still_tile() {
        let protected = Facts { app: None, ..facts(0x14CF_0000, 0) };
        assert_eq!(classify(&protected, &[TilingException::new("", "").unwrap()]), Verdict::Tile);
        let without_ui_access = Facts { out_of_reach: true, ..protected };
        assert_eq!(classify(&without_ui_access, &[]), Verdict::Float(Why::OutOfReach));
    }

    #[test]
    fn popups_menus_and_tooltips_go_unlogged() {
        assert!(!classify(&facts(0x9400_0000, WS_EX_TOOLWINDOW), &[]).worth_logging());
        assert!(classify(&facts(0x94C8_0000, 0), &[]).worth_logging());
    }
}
