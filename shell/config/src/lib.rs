//! shell.json: the shell's COSMIC settings, which dfnstrate writes next to
//! the shell's exe from the config's `cosmic` (cli/src/cosmic.rs, where each
//! one is described under its cosmic-comp name) and the shell reads at
//! startup (shell/src/settings.rs). Every type parses only what the shell can
//! do, so a file the shell would have to second-guess doesn't parse at all.

use std::fmt;
use std::num::{NonZeroU8, NonZeroU32};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "shell.json";

/// The shell has nine workspaces, as COSMIC's keys 1–9 reach.
pub const WORKSPACES: usize = 9;

/// With the choices made: no flag that only means something next to
/// another one.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
    /// COSMIC's custom tiling exceptions, the enabled ones: windows that
    /// float although they'd tile.
    pub tiling_exceptions: Vec<TilingException>,
    /// Not COSMIC's: the config's `displays`.
    pub displays: Vec<DisplayMode>,
    /// COSMIC's Mode is_dark: Windows' dark mode.
    pub dark: bool,
    /// Not COSMIC's: the config's `autostart`, command lines run once per
    /// logon after what explorer would start.
    pub autostart: Vec<String>,
    /// Not COSMIC's: the config's `defaultApps`.
    #[serde(default)]
    pub default_apps: Vec<DefaultApp>,
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
            active_hint: Some(ActiveHint { thickness: NonZeroU8::new(3).expect("not 0"), colour: [0x63, 0xB1, 0xE4] }),
            shortcuts: Vec::new(),
            tiling_exceptions: Vec::new(),
            displays: Vec::new(),
            dark: true,
            autostart: Vec::new(),
            default_apps: Vec::new(),
        }
    }
}

impl Settings {
    pub fn parse(json: &str) -> Result<Settings, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// The shortcut bound to exactly these keys, if the config has one.
    pub fn shortcut(&self, keys: Keys) -> Option<&Action> {
        self.shortcuts.iter().find(|s| s.keys == keys).map(|s| &s.action)
    }
}

/// What Super+Y (ToggleTiling) toggles.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileBehavior {
    /// Every workspace at once.
    Global,
    /// The one being shown; `autotile` only decides how new ones start.
    PerWorkspace,
}

/// What a window raising itself gets (xdg-activation in COSMIC).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPolicy {
    /// Its workspace is switched to and it gets focus.
    Focus,
    /// Focus if it's on the workspace being shown, else it's marked urgent.
    FocusIfActiveWorkspace,
    /// Only ever marked urgent.
    Urgent,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FocusFollowsCursor {
    /// Milliseconds the pointer has to stay over a window before it gets focus.
    pub delay_ms: u64,
}

/// In pixels at 100% scale, as cosmic-comp lays them out: `outer` around
/// the whole tiling, `inner` around each tile, halved (rounding down) on
/// sides that face another tile.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Gaps {
    pub outer: u16,
    pub inner: u16,
}

/// The focus border; no border at all is no `ActiveHint`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActiveHint {
    /// Pixels at 100% scale.
    pub thickness: NonZeroU8,
    pub colour: [u8; 3],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub keys: Keys,
    pub action: Action,
}

/// Exactly these modifiers, and one key as a Windows virtual-key code.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Keys {
    #[serde(rename = "super")]
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub vk: u8,
}

impl fmt::Display for Keys {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (on, name) in [(self.logo, "Super+"), (self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
            if on {
                f.write_str(name)?;
            }
        }
        write!(f, "vk {:#04x}", self.vk)
    }
}

/// COSMIC's shortcut actions (cosmic-settings-config's `Action`), the ones
/// the shell has.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// No binding: the keys go to Windows and the focused app.
    Disable,
    /// A command line, started in the user's home as Windows parses it
    /// (CreateProcess), not split by a shell.
    Spawn(CommandLine),
    System(System),
    Close,
    Maximize,
    ToggleTiling,
    ToggleWindowFloating,
    ToggleOrientation,
    Focus(Direction),
    Move(Direction),
    Workspace(WorkspaceNumber),
    /// The window goes there, and so do you (the shell has no SendTo*).
    MoveToWorkspace(WorkspaceNumber),
    NextWorkspace,
    PreviousWorkspace,
    MoveToNextWorkspace,
    MoveToPreviousWorkspace,
    /// Focus the monitor that way.
    SwitchOutput(Direction),
    MoveToOutput(Direction),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// 1 to `WORKSPACES`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "u8")]
pub struct WorkspaceNumber(u8);

impl TryFrom<u8> for WorkspaceNumber {
    type Error = String;
    fn try_from(n: u8) -> Result<Self, String> {
        if (1..=WORKSPACES).contains(&n.into()) { Ok(Self(n)) } else { Err(format!("workspace {n}: the shell has workspaces 1 to {WORKSPACES}")) }
    }
}

impl WorkspaceNumber {
    /// Counted from 0.
    pub fn index(self) -> usize {
        usize::from(self.0) - 1
    }
}

/// Non-empty, one line.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct CommandLine(String);

impl TryFrom<String> for CommandLine {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s.trim().is_empty() || s.chars().any(char::is_control) {
            Err(format!("{s:?} isn't a command line (non-empty, one line)"))
        } else {
            Ok(Self(s))
        }
    }
}

impl CommandLine {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Floats windows whose app ID and title both match, as COSMIC's
/// `tiling_exception_custom` does: unanchored regexes, so "" matches
/// anything. The app ID is the exe's name without `.exe`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TilingException {
    pub appid: Pattern,
    pub title: Pattern,
}

impl TilingException {
    pub fn new(appid: &str, title: &str) -> Result<Self, String> {
        Ok(TilingException { appid: appid.to_owned().try_into()?, title: title.to_owned().try_into()? })
    }

    pub fn matches(&self, app: &str, title: &str) -> bool {
        self.appid.0.is_match(app) && self.title.0.is_match(title)
    }
}

/// A regex as the shell's regex-lite reads it.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(regex_lite::Regex);

impl TryFrom<String> for Pattern {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        regex_lite::Regex::new(&s).map(Pattern).map_err(|e| format!("{s:?}: {e}"))
    }
}

impl From<Pattern> for String {
    fn from(p: Pattern) -> String {
        p.0.as_str().to_owned()
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for Pattern {}

/// One monitor's mode. The monitor is matched by the model name and serial
/// in its EDID: the last two words of the name Linux compositors give it
/// ("Philips Consumer Electronics Company 49M2C8900 AU42447000108"), unlike
/// Windows' connector numbering, which changes when monitors come and go.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisplayMode {
    pub model: EdidText,
    /// Only needed to tell apart two monitors of the same model.
    pub serial: Option<EdidText>,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    /// Hz; the nearest whole one the monitor offers, which is all Windows has.
    pub refresh: Refresh,
}

/// An EDID text descriptor's text: at most 13 bytes, compared trimmed of
/// its padding.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "String", into = "String")]
pub struct EdidText(String);

impl TryFrom<String> for EdidText {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if (1..=13).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_graphic() || b == b' ') && s.trim() == s {
            Ok(EdidText(s))
        } else {
            Err(format!("{s:?} isn't EDID text (1-13 printable ASCII characters, no padding)"))
        }
    }
}

impl From<EdidText> for String {
    fn from(t: EdidText) -> String {
        t.0
    }
}

impl EdidText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EdidText {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hz, finite and above 0.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(try_from = "f64", into = "f64")]
pub struct Refresh(f64);

impl TryFrom<f64> for Refresh {
    type Error = String;
    fn try_from(hz: f64) -> Result<Self, String> {
        if hz.is_finite() && hz > 0.0 { Ok(Refresh(hz)) } else { Err(format!("{hz} isn't a refresh rate")) }
    }
}

impl From<Refresh> for f64 {
    fn from(r: Refresh) -> f64 {
        r.0
    }
}

impl Refresh {
    pub fn hz(self) -> f64 {
        self.0
    }
}

/// A file extension (".html") or URL scheme ("https") and the ProgId it
/// should open with.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultApp {
    pub id: String,
    pub prog_id: String,
}
