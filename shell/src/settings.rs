//! The shell's COSMIC settings: shell.json next to the exe, which dfnstrate
//! writes from the config's `cosmic` (cli/src/cosmic.rs, where each one is
//! described under its cosmic-comp name). Read once at startup.
//!
//! Without the file (an install from before it existed, or a shell started
//! by hand) the shell behaves like stock cosmic-comp, so a bad or missing
//! file still leaves a working desktop; why goes to the log.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::display::DisplayMode;

pub const FILE: &str = "shell.json";

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub autotile: bool,
    pub autotile_behavior: TileBehavior,
    pub focus_follows_cursor: Option<FocusFollowsCursor>,
    pub cursor_follows_focus: bool,
    pub activation_policy: ActivationPolicy,
    pub gaps: Gaps,
    pub active_hint: Option<ActiveHint>,
    pub shortcuts: Vec<Shortcut>,
    /// COSMIC's custom tiling exceptions: windows that float although
    /// they'd tile (classify.rs).
    pub tiling_exceptions: Vec<TilingException>,
    /// Not COSMIC's: the config's `displays`, display.rs.
    pub displays: Vec<DisplayMode>,
    /// COSMIC's Mode is_dark: Windows' dark mode (theme.rs).
    pub dark: bool,
    /// Not COSMIC's: the config's `autostart`, command lines run once per
    /// logon after what explorer would start (autostart.rs).
    pub autostart: Vec<String>,
}

/// Floats windows whose app ID and title both match, as COSMIC's
/// `tiling_exception_custom` does: unanchored regexes, so "" matches
/// anything. The app ID here is the exe's name without `.exe`.
#[derive(Deserialize, Debug, Clone)]
#[serde(try_from = "RawException")]
pub struct TilingException {
    appid: regex_lite::Regex,
    title: regex_lite::Regex,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawException {
    appid: String,
    title: String,
}

impl TryFrom<RawException> for TilingException {
    type Error = String;
    fn try_from(raw: RawException) -> Result<Self, String> {
        TilingException::new(&raw.appid, &raw.title)
    }
}

impl TilingException {
    pub fn new(appid: &str, title: &str) -> Result<Self, String> {
        let compile = |re: &str| regex_lite::Regex::new(re).map_err(|e| format!("tiling exception {re:?}: {e}"));
        Ok(TilingException { appid: compile(appid)?, title: compile(title)? })
    }

    pub fn matches(&self, app: &str, title: &str) -> bool {
        self.appid.is_match(app) && self.title.is_match(title)
    }
}

impl PartialEq for TilingException {
    fn eq(&self, other: &Self) -> bool {
        (self.appid.as_str(), self.title.as_str()) == (other.appid.as_str(), other.title.as_str())
    }
}

impl Eq for TilingException {}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileBehavior {
    Global,
    PerWorkspace,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPolicy {
    Focus,
    FocusIfActiveWorkspace,
    Urgent,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusFollowsCursor {
    pub delay_ms: u64,
}

/// In pixels at 100% scale.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Gaps {
    pub outer: u16,
    pub inner: u16,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActiveHint {
    /// Pixels at 100% scale, at least 1.
    pub thickness: u8,
    pub colour: [u8; 3],
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub keys: Keys,
    pub action: Action,
}

/// Exactly these modifiers, and the key as a Windows virtual-key code.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    #[serde(rename = "super")]
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub vk: u8,
}

/// COSMIC's shortcut actions, the ones the shell has (cli/src/cosmic.rs).
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Disable,
    Spawn(String),
    System(System),
    Close,
    Maximize,
    ToggleTiling,
    ToggleWindowFloating,
    ToggleOrientation,
    Focus(Direction),
    Move(Direction),
    Workspace(u8),
    MoveToWorkspace(u8),
    NextWorkspace,
    PreviousWorkspace,
    MoveToNextWorkspace,
    MoveToPreviousWorkspace,
    SwitchOutput(Direction),
    MoveToOutput(Direction),
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    AppLibrary,
    HomeFolder,
    Launcher,
    LockScreen,
    LogOut,
    Terminal,
    Mute,
    VolumeLower,
    VolumeRaise,
    PlayPause,
    PlayNext,
    PlayPrev,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Default for Settings {
    /// cosmic-comp's and COSMIC's dark theme's own defaults.
    fn default() -> Self {
        Settings {
            autotile: false,
            autotile_behavior: TileBehavior::Global,
            focus_follows_cursor: None,
            cursor_follows_focus: false,
            activation_policy: ActivationPolicy::Focus,
            gaps: Gaps { outer: 0, inner: 8 },
            // COSMIC's default accent, #63b1e4, as the drop preview has it.
            active_hint: Some(ActiveHint { thickness: 3, colour: [0x63, 0xB1, 0xE4] }),
            shortcuts: Vec::new(),
            tiling_exceptions: Vec::new(),
            displays: Vec::new(),
            dark: true,
            autostart: Vec::new(),
        }
    }
}

impl Settings {
    pub fn parse(json: &str) -> Result<Settings, String> {
        let s: Settings = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if s.active_hint.is_some_and(|h| h.thickness == 0) {
            return Err("active_hint.thickness is 0; no hint is null".into());
        }
        for d in &s.displays {
            if d.width == 0 || d.height == 0 || !(d.refresh.is_finite() && d.refresh > 0.0) {
                return Err(format!("display mode {d:?} can't be set"));
            }
        }
        for sc in &s.shortcuts {
            if let Action::Workspace(n) | Action::MoveToWorkspace(n) = sc.action
                && !(1..=9).contains(&n)
            {
                return Err(format!("workspace {n} doesn't exist"));
            }
        }
        Ok(s)
    }

    /// The shortcut bound to exactly these keys, if the config has one.
    pub fn shortcut(&self, keys: Keys) -> Option<&Action> {
        self.shortcuts.iter().find(|s| s.keys == keys).map(|s| &s.action)
    }
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Reads `json` (shell.json's contents, None if there is none) once; later
/// calls change nothing. Returns what went wrong, if anything did.
pub fn init(json: Option<&str>) -> Result<(), String> {
    let (settings, problem) = match json.map(Settings::parse) {
        Some(Ok(s)) => (s, None),
        Some(Err(e)) => (Settings::default(), Some(format!("{FILE}: {e}; using COSMIC's defaults"))),
        None => (Settings::default(), Some(format!("no {FILE}; using COSMIC's defaults"))),
    };
    let _ = SETTINGS.set(settings);
    problem.map_or(Ok(()), Err)
}

pub fn get() -> &'static Settings {
    SETTINGS.get_or_init(Settings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What dfnstrate renders the distro defaults to; its render test checks
    /// the same file.
    const RENDERED: &str = include_str!("../../configs/shell-settings.json");

    #[test]
    fn reads_what_dfnstrate_writes() {
        let s = Settings::parse(RENDERED).unwrap();
        assert_eq!(s.gaps, Gaps { outer: 0, inner: 1 });
        assert_eq!(s.active_hint, Some(ActiveHint { thickness: 1, colour: [0x8b, 0x86, 0x83] }));
        assert_eq!(s.focus_follows_cursor, Some(FocusFollowsCursor { delay_ms: 100 }));
        assert_eq!(s.activation_policy, ActivationPolicy::FocusIfActiveWorkspace);
        let super_ = |vk| Keys { logo: true, ctrl: false, alt: false, shift: false, vk };
        assert_eq!(s.shortcut(super_(b'Y')), Some(&Action::Disable));
        assert_eq!(s.shortcut(super_(b'D')), Some(&Action::System(System::Launcher)));
        assert_eq!(s.shortcut(Keys { shift: true, ..super_(0x20) }), Some(&Action::System(System::PlayPause)));
        assert_eq!(s.shortcut(super_(b'Q')), None, "unbound here: COSMIC's default applies");
        assert_eq!(s.tiling_exceptions, vec![TilingException::new("^Discord(PTB|Canary)?$", "^Discord Updater$").unwrap()]);
        assert!(s.tiling_exceptions[0].matches("Discord", "Discord Updater"));
        assert!(!s.tiling_exceptions[0].matches("Discord", "Discord"));
        assert_eq!(s.displays, Vec::new());
        assert!(s.dark);
        assert_eq!(s.autostart, Vec::<String>::new());
    }

    #[test]
    fn rejects_what_it_doesnt_know() {
        assert!(Settings::parse(&RENDERED.replacen("\"autotile\"", "\"autotiles\"", 1)).is_err());
        assert!(Settings::parse(&RENDERED.replacen("\"Disable\"", "\"ZoomIn\"", 1)).is_err());
        assert!(Settings::parse(&RENDERED.replacen("\"thickness\": 1", "\"thickness\": 0", 1)).is_err());
        let mode = r#""displays": [{"model": "49M2C8900", "serial": null, "width": 5120, "height": 1440, "refresh": 240.49}]"#;
        let with_mode = Settings::parse(&RENDERED.replacen("\"displays\": []", mode, 1)).unwrap();
        assert_eq!(with_mode.displays.len(), 1, "what cli/src/config.rs's DisplayMode renders to");
        assert!(Settings::parse(&RENDERED.replacen("\"displays\": []", &mode.replace("5120", "0"), 1)).is_err());
        let bad_regex = RENDERED.replacen("\"^Discord Updater$\"", "\"(\"", 1);
        assert_ne!(bad_regex, RENDERED);
        assert!(Settings::parse(&bad_regex).is_err());
    }
}
