//! The shell's COSMIC settings: shell.json next to the exe, in the format
//! shell/config defines for dfnstrate and the shell both. Read once at
//! startup.
//!
//! Without the file (an install from before it existed, or a shell started
//! by hand) the shell behaves like stock cosmic-comp, so a bad or missing
//! file still leaves a working desktop; why goes to the log.

use std::sync::OnceLock;

pub use shell_config::*;

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
        assert_eq!(s.active_hint, Some(ActiveHint { thickness: std::num::NonZeroU8::MIN, colour: [0x8b, 0x86, 0x83] }));
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
        assert_eq!(with_mode.displays.len(), 1);
        assert!(Settings::parse(&RENDERED.replacen("\"displays\": []", &mode.replace("5120", "0"), 1)).is_err());
        assert!(Settings::parse(&RENDERED.replacen("\"displays\": []", &mode.replace("240.49", "-1"), 1)).is_err());
        assert!(Settings::parse(&RENDERED.replacen("\"Disable\"", "{\"Workspace\": 1}", 1)).is_ok());
        assert!(Settings::parse(&RENDERED.replacen("\"Disable\"", "{\"Workspace\": 10}", 1)).is_err());
        assert!(Settings::parse(&RENDERED.replacen("\"Disable\"", "{\"MoveToWorkspace\": 0}", 1)).is_err());
        let bad_regex = RENDERED.replacen("\"^Discord Updater$\"", "\"(\"", 1);
        assert_ne!(bad_regex, RENDERED);
        assert!(Settings::parse(&bad_regex).is_err());
    }
}
