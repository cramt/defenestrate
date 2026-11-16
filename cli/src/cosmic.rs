//! The shell's COSMIC settings (`cosmic` in the config), under cosmic-comp's
//! own names, so a COSMIC user's settings carry over key for key:
//! `compositor` is com.system76.CosmicComp, `theme` the bits of
//! com.system76.CosmicTheme.Dark the shell draws with, `shortcuts` the
//! custom shortcuts of com.system76.CosmicSettings.Shortcuts, layered over
//! COSMIC's default bindings as COSMIC layers them, `window_rules`
//! com.system76.CosmicSettings.WindowRules, and `mode`
//! com.system76.CosmicTheme.Mode.
//!
//! Only what the shell does is accepted: an unknown key, or an action the
//! shell has no counterpart for, fails the build rather than doing nothing on
//! the machine. `render` turns it into shell.json, next to the shell's exe,
//! which the shell reads at startup.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cosmic {
    pub compositor: Compositor,
    pub theme: Theme,
    pub mode: Mode,
    #[serde(default)]
    pub shortcuts: Shortcuts,
    #[serde(default)]
    pub window_rules: WindowRules,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    /// Windows' dark mode for apps and the system, set at every logon.
    pub is_dark: bool,
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WindowRules {
    /// Windows that float although the shell would tile them.
    #[serde(default)]
    pub tiling_exception_custom: Vec<ApplicationException>,
}

/// COSMIC's form: both unanchored regexes, so "" matches anything. On
/// Windows the app ID is the exe's name without `.exe`, e.g.
/// "LeagueClientUx"; the shell's log names it for every window it sees.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationException {
    pub appid: Pattern,
    pub title: Pattern,
    #[serde(default = "enabled", skip_serializing)]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

/// A regex the shell's regex-lite accepts, so a bad one fails the build
/// rather than leaving the shell on its defaults.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(String);

impl TryFrom<String> for Pattern {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        regex_lite::Regex::new(&s).map_err(|e| format!("{s:?}: {e}"))?;
        Ok(Pattern(s))
    }
}

impl From<Pattern> for String {
    fn from(p: Pattern) -> String {
        p.0
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Compositor {
    /// New workspaces tile.
    pub autotile: bool,
    pub autotile_behavior: TileBehavior,
    /// The focus border at all; `theme.active_hint` is how thick.
    pub active_hint: bool,
    pub focus_follows_cursor: bool,
    /// Milliseconds the pointer has to stay over a window before it gets focus.
    pub focus_follows_cursor_delay: u64,
    /// The pointer jumps to the middle of a window focused from the keyboard.
    pub cursor_follows_focus: bool,
    pub activation_policy: ActivationPolicy,
}

/// What Super+Y (ToggleTiling) toggles.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileBehavior {
    /// Every workspace at once.
    Global,
    /// The one being shown; `autotile` only decides how new ones start.
    PerWorkspace,
}

/// What a window raising itself gets (xdg-activation in COSMIC).
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPolicy {
    /// Its workspace is switched to and it gets focus.
    Focus,
    /// Focus if it's on the workspace being shown, else it's marked urgent.
    FocusIfActiveWorkspace,
    /// Only ever marked urgent.
    Urgent,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// (outer, inner) in pixels at 100% scale, as cosmic-comp lays them out:
    /// `outer` around the whole tiling, `inner` around each tile, halved
    /// (rounding down) on sides that face another tile.
    pub gaps: (u16, u16),
    /// Width of the focus border, in pixels at 100% scale; 0 draws none.
    pub active_hint: u8,
    /// The focus border's colour.
    pub accent: Srgb,
}

/// COSMIC's colour form: channels from 0 to 1.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(try_from = "RawSrgb")]
pub struct Srgb {
    red: f32,
    green: f32,
    blue: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSrgb {
    red: f32,
    green: f32,
    blue: f32,
}

impl TryFrom<RawSrgb> for Srgb {
    type Error = String;
    fn try_from(c: RawSrgb) -> Result<Self, String> {
        if [c.red, c.green, c.blue].iter().all(|v| (0.0..=1.0).contains(v)) {
            Ok(Srgb { red: c.red, green: c.green, blue: c.blue })
        } else {
            Err(format!("colour channels go from 0 to 1, got red {} green {} blue {}", c.red, c.green, c.blue))
        }
    }
}

impl Srgb {
    fn bytes(self) -> [u8; 3] {
        [self.red, self.green, self.blue].map(|v| (v * 255.0).round() as u8)
    }
}

/// Key combination -> what it does, replacing COSMIC's default binding for
/// that combination if there is one.
#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(try_from = "BTreeMap<String, Action>")]
pub struct Shortcuts(BTreeMap<Keys, Action>);

impl TryFrom<BTreeMap<String, Action>> for Shortcuts {
    type Error = String;
    fn try_from(raw: BTreeMap<String, Action>) -> Result<Self, String> {
        let mut out = BTreeMap::new();
        let mut spelled: BTreeMap<Keys, String> = BTreeMap::new();
        for (text, action) in raw {
            let keys = Keys::parse(&text)?;
            // "Super+y" and "super+Y" are the same keys; two actions for them
            // would silently pick one.
            if let Some(first) = spelled.insert(keys, text.clone()) {
                return Err(format!("{first:?} and {text:?} are the same keys"));
            }
            out.insert(keys, action);
        }
        Ok(Shortcuts(out))
    }
}

/// Modifiers plus one key, which the shell matches as a Windows virtual-key
/// code.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Keys {
    #[serde(rename = "super")]
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub vk: u8,
}

impl Keys {
    /// COSMIC's spelling (cosmic-settings-config's Binding::from_str):
    /// modifiers in any case, `+`-separated, one key named as its xkb keysym
    /// or as the character itself.
    pub fn parse(text: &str) -> Result<Keys, String> {
        let mut keys = Keys { logo: false, ctrl: false, alt: false, shift: false, vk: 0 };
        let mut key = None;
        for token in text.split('+').map(str::trim) {
            match token.to_ascii_lowercase().as_str() {
                "super" => keys.logo = true,
                "ctrl" => keys.ctrl = true,
                "alt" => keys.alt = true,
                "shift" => keys.shift = true,
                _ if key.is_some() => return Err(format!("{text:?}: only one key besides the modifiers")),
                _ => key = Some(vk(token).ok_or_else(|| format!("{text:?}: no key {token:?} (an xkb keysym name, e.g. space, Return, period, a, F1)"))?),
            }
        }
        keys.vk = key.ok_or_else(|| format!("{text:?}: names no key besides the modifiers"))?;
        Ok(keys)
    }
}

/// The virtual-key code for an xkb keysym name. Letters, digits, F-keys and
/// named keys are the same key on every Windows layout, as are period, comma
/// and minus (VK_OEM_PERIOD and co. are defined by the character). Other
/// punctuation varies by layout, so only slash is here, as the key it is on
/// the US layout (VK_OEM_2), for COSMIC's Super+slash.
fn vk(name: &str) -> Option<u8> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c.to_ascii_uppercase() {
            c @ ('A'..='Z' | '0'..='9') => Some(c as u8),
            '.' => Some(0xBE),
            ',' => Some(0xBC),
            '-' => Some(0xBD),
            '/' => Some(0xBF),
            ' ' => Some(0x20),
            _ => None,
        };
    }
    if let Some(n) = name.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u8>().ok()) {
        return (1..=24).contains(&n).then(|| 0x6F + n);
    }
    const NAMED: &[(&str, u8)] = &[
        ("space", 0x20),
        ("Return", 0x0D),
        ("Escape", 0x1B),
        ("Tab", 0x09),
        ("BackSpace", 0x08),
        ("Delete", 0x2E),
        ("Insert", 0x2D),
        ("Home", 0x24),
        ("End", 0x23),
        ("Page_Up", 0x21),
        ("Prior", 0x21),
        ("Page_Down", 0x22),
        ("Next", 0x22),
        ("Left", 0x25),
        ("Up", 0x26),
        ("Right", 0x27),
        ("Down", 0x28),
        ("Print", 0x2C),
        ("period", 0xBE),
        ("comma", 0xBC),
        ("minus", 0xBD),
        ("slash", 0xBF),
        ("XF86AudioMute", 0xAD),
        ("XF86AudioLowerVolume", 0xAE),
        ("XF86AudioRaiseVolume", 0xAF),
        ("XF86AudioNext", 0xB0),
        ("XF86AudioPrev", 0xB1),
        ("XF86AudioPlay", 0xB3),
    ];
    // Case-sensitive first, as xkb's lookup is, then not.
    NAMED.iter().find(|(n, _)| *n == name).or_else(|| NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))).map(|&(_, vk)| vk)
}

/// COSMIC's shortcut actions (cosmic-settings-config's `Action`), the ones
/// the shell has.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
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

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// 1–9: the shell has nine workspaces.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "u8")]
pub struct WorkspaceNumber(u8);

impl TryFrom<u8> for WorkspaceNumber {
    type Error = String;
    fn try_from(n: u8) -> Result<Self, String> {
        if (1..=9).contains(&n) { Ok(Self(n)) } else { Err(format!("workspace {n}: the shell has workspaces 1 to 9")) }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
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

/// What the shell reads (shell/src/settings.rs), with the choices made: no
/// flag that only means something next to another one.
#[derive(Serialize, Debug, PartialEq)]
pub struct ShellSettings {
    pub autotile: bool,
    pub autotile_behavior: TileBehavior,
    pub focus_follows_cursor: Option<FocusFollowsCursor>,
    pub cursor_follows_focus: bool,
    pub activation_policy: ActivationPolicy,
    pub gaps: Gaps,
    pub active_hint: Option<ActiveHint>,
    pub shortcuts: Vec<Shortcut>,
    /// Only the enabled ones.
    pub tiling_exceptions: Vec<ApplicationException>,
    /// Not COSMIC's: the config's `displays`.
    pub displays: Vec<crate::config::DisplayMode>,
    pub dark: bool,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct FocusFollowsCursor {
    pub delay_ms: u64,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Gaps {
    pub outer: u16,
    pub inner: u16,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct ActiveHint {
    pub thickness: u8,
    pub colour: [u8; 3],
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Shortcut {
    pub keys: Keys,
    pub action: Action,
}

impl Cosmic {
    pub fn shell_settings(&self, displays: &[crate::config::DisplayMode]) -> ShellSettings {
        let c = &self.compositor;
        let t = &self.theme;
        ShellSettings {
            autotile: c.autotile,
            autotile_behavior: c.autotile_behavior,
            focus_follows_cursor: c.focus_follows_cursor.then_some(FocusFollowsCursor { delay_ms: c.focus_follows_cursor_delay }),
            cursor_follows_focus: c.cursor_follows_focus,
            activation_policy: c.activation_policy,
            gaps: Gaps { outer: t.gaps.0, inner: t.gaps.1 },
            active_hint: (c.active_hint && t.active_hint > 0).then_some(ActiveHint { thickness: t.active_hint, colour: t.accent.bytes() }),
            shortcuts: self.shortcuts.0.iter().map(|(&keys, action)| Shortcut { keys, action: action.clone() }).collect(),
            tiling_exceptions: self.window_rules.tiling_exception_custom.iter().filter(|e| e.enabled).cloned().collect(),
            displays: displays.to_vec(),
            dark: self.mode.is_dark,
        }
    }

    pub fn render(&self, displays: &[crate::config::DisplayMode]) -> String {
        let mut json = serde_json::to_string_pretty(&self.shell_settings(displays)).expect("plain data");
        json.push('\n');
        json
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_spelled_like_cosmic() {
        let k = Keys::parse("Super+Shift+space").unwrap();
        assert_eq!(k, Keys { logo: true, ctrl: false, alt: false, shift: true, vk: 0x20 });
        assert_eq!(Keys::parse("super+Y").unwrap(), Keys::parse("Super+y").unwrap());
        assert_eq!(Keys::parse("Super+period").unwrap().vk, 0xBE);
        assert_eq!(Keys::parse("Super+.").unwrap().vk, 0xBE);
        assert_eq!(Keys::parse("Super+F1").unwrap().vk, 0x70);
        assert_eq!(Keys::parse("Super+F24").unwrap().vk, 0x87);
        assert_eq!(Keys::parse("Super+escape").unwrap().vk, 0x1B, "names fall back to any case");
        assert_eq!(Keys::parse("Ctrl+Alt+Delete").unwrap().vk, 0x2E);
        for bad in ["Super", "Super+a+b", "Super+F25", "Super+Hyper", "Super+bracketleft", ""] {
            assert!(Keys::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn shortcuts_take_cosmics_action_forms() {
        let toml = r#"
            "Super+y" = "Disable"
            "Super+t" = { Spawn = "wt.exe -p cmd" }
            "Super+d" = { System = "Launcher" }
            "Super+Shift+space" = { System = "PlayPause" }
            "Super+u" = { Focus = "Left" }
            "Super+Alt+3" = { MoveToWorkspace = 3 }
        "#;
        let s: Shortcuts = toml::from_str(toml).unwrap();
        assert_eq!(s.0.len(), 6);
        assert_eq!(s.0[&Keys::parse("Super+t").unwrap()], Action::Spawn(CommandLine("wt.exe -p cmd".into())));
        let bad = [
            r#""Super+y" = "ZoomIn""#,
            r#""Super+y" = { Focus = "In" }"#,
            r#""Super+y" = { Workspace = 10 }"#,
            r#""Super+y" = { System = "Screenshot" }"#,
            r#""Super+y" = { Spawn = "" }"#,
            "\"Super+y\" = \"Disable\"\n\"super+Y\" = \"Disable\"",
        ];
        for b in bad {
            assert!(toml::from_str::<Shortcuts>(b).is_err(), "{b}");
        }
    }

    #[test]
    fn theme_rejects_what_cosmic_would() {
        let ok = "gaps = [0, 1]\nactive_hint = 1\naccent = { red = 0.5, green = 1, blue = 0 }";
        let t: Theme = toml::from_str(ok).unwrap();
        assert_eq!(t.accent.bytes(), [128, 255, 0]);
        assert!(toml::from_str::<Theme>(&ok.replace("blue = 0", "blue = 1.5")).is_err());
        assert!(toml::from_str::<Theme>(&ok.replace("gaps = [0, 1]", "gaps = [-1, 1]")).is_err());
        assert!(toml::from_str::<Theme>(&format!("{ok}\nradius = 3")).is_err());
    }

    #[test]
    fn tiling_exceptions_render_only_when_enabled_and_valid() {
        let rules = |body: &str| toml::from_str::<WindowRules>(body);
        let r = rules("tiling_exception_custom = [{ appid = \"^LeagueClientUx$\", title = \"\" }, { appid = \"x\", title = \"\", enabled = false }]").unwrap();
        let shown: Vec<_> = r.tiling_exception_custom.iter().filter(|e| e.enabled).collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(serde_json::to_value(shown[0]).unwrap(), serde_json::json!({ "appid": "^LeagueClientUx$", "title": "" }));
        assert!(rules("tiling_exception_custom = [{ appid = \"(\", title = \"\" }]").is_err());
        assert!(rules("tiling_exception_custom = [{ appid = \"a\" }]").is_err(), "COSMIC always has a title");
    }

    #[test]
    fn active_hint_off_either_way() {
        let mut c: Cosmic = toml::from_str(
            r#"
            [compositor]
            autotile = true
            autotile_behavior = "Global"
            active_hint = false
            focus_follows_cursor = false
            focus_follows_cursor_delay = 250
            cursor_follows_focus = false
            activation_policy = "Focus"
            [theme]
            gaps = [0, 8]
            active_hint = 3
            accent = { red = 0.39, green = 0.69, blue = 0.89 }
            [mode]
            is_dark = false
            "#,
        )
        .unwrap();
        let s = c.shell_settings(&[]);
        assert_eq!((s.active_hint, s.focus_follows_cursor), (None, None));
        assert!(!s.dark);
        c.compositor.active_hint = true;
        assert_eq!(c.shell_settings(&[]).active_hint, Some(ActiveHint { thickness: 3, colour: [99, 176, 227] }));
        c.theme.active_hint = 0;
        assert_eq!(c.shell_settings(&[]).active_hint, None);
    }
}
