//! Display modes from the config (`displays`): which monitor a mode is for,
//! and which of the modes it offers is meant. Pure; modes.rs reads the EDID
//! and sets the mode on Windows.
//!
//! Without this Windows picks its own default mode, which for a 240 Hz
//! monitor was 60 Hz.

use crate::settings::DisplayMode;

/// The text descriptors of a monitor's EDID that name it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Edid {
    pub model: Option<String>,
    pub serial: Option<String>,
}

impl Edid {
    /// The base block's four 18-byte descriptors at 54, 72, 90 and 108: a
    /// display descriptor starts 00 00 00, has its tag at 3 (FC model name,
    /// FF serial) and up to 13 bytes of text from 5, ended by a newline and
    /// padded with spaces.
    pub fn parse(edid: &[u8]) -> Edid {
        let mut out = Edid::default();
        for at in [54, 72, 90, 108] {
            let Some(d) = edid.get(at..at + 18) else { break };
            if d[..3] != [0, 0, 0] {
                continue;
            }
            let text = &d[5..];
            let text = &text[..text.iter().position(|&c| c == b'\n').unwrap_or(text.len())];
            let text = String::from_utf8_lossy(text).trim_end().to_owned();
            match d[3] {
                0xFC => out.model = Some(text),
                0xFF => out.serial = Some(text),
                _ => {}
            }
        }
        out
    }
}

/// Whether `mode` is for the monitor with this EDID: its model, and its
/// serial if the mode names one.
pub fn is_for(mode: &DisplayMode, edid: &Edid) -> bool {
    edid.model.as_deref() == Some(mode.model.as_str()) && mode.serial.as_ref().is_none_or(|s| edid.serial.as_deref() == Some(s.as_str()))
}

/// Of the refresh rates the monitor offers at `mode`'s resolution, the one
/// nearest to its refresh rate: Windows only has whole ones.
pub fn pick(mode: &DisplayMode, offered: impl IntoIterator<Item = (u32, u32, u32)>) -> Option<u32> {
    let want = mode.refresh.hz();
    offered
        .into_iter()
        .filter(|&(w, h, _)| (w, h) == (mode.width.get(), mode.height.get()))
        .map(|(_, _, hz)| hz)
        .min_by(|&a, &b| (f64::from(a) - want).abs().total_cmp(&(f64::from(b) - want).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(tag: u8, text: &str) -> [u8; 18] {
        let mut d = [0u8; 18];
        d[3] = tag;
        let mut body = text.as_bytes().to_vec();
        body.push(b'\n');
        body.resize(13, b' ');
        d[5..].copy_from_slice(&body);
        d
    }

    /// saturn's monitor, as Linux names it: "Philips Consumer Electronics
    /// Company 49M2C8900 AU42447000108".
    fn philips() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[54..72].copy_from_slice(&[0x56, 0x5e, 0, 0xa0, 0xa0, 0xa0, 0x29, 0x50, 0x30, 0x20, 0x35, 0, 0x55, 0x50, 0x21, 0, 0, 0x1a]);
        edid[72..90].copy_from_slice(&descriptor(0xFF, "AU42447000108"));
        edid[90..108].copy_from_slice(&descriptor(0xFD, ""));
        edid[108..126].copy_from_slice(&descriptor(0xFC, "49M2C8900"));
        edid
    }

    fn mode(serial: Option<&str>) -> DisplayMode {
        let text = |s: &str| s.to_owned().try_into().unwrap();
        let size = |n| std::num::NonZeroU32::new(n).unwrap();
        DisplayMode { model: text("49M2C8900"), serial: serial.map(text), width: size(5120), height: size(1440), refresh: 240.49.try_into().unwrap() }
    }

    #[test]
    fn reads_model_and_serial() {
        assert_eq!(Edid::parse(&philips()), Edid { model: Some("49M2C8900".into()), serial: Some("AU42447000108".into()) });
        assert_eq!(Edid::parse(&philips()[..60]), Edid::default(), "truncated");
    }

    #[test]
    fn matches_by_model_and_serial_when_given() {
        let edid = Edid::parse(&philips());
        assert!(is_for(&mode(None), &edid));
        assert!(is_for(&mode(Some("AU42447000108")), &edid));
        assert!(!is_for(&mode(Some("other")), &edid));
        assert!(!is_for(&DisplayMode { model: "49M2C8901".to_owned().try_into().unwrap(), ..mode(None) }, &edid));
    }

    /// What the driver offered at 5120x1440 (windows-bugs.md #8).
    #[test]
    fn picks_the_nearest_whole_hz() {
        let offered = [48, 60, 75, 100, 120, 165, 240].map(|hz| (5120, 1440, hz)).into_iter().chain([(3840, 1080, 241)]);
        assert_eq!(pick(&mode(None), offered.clone()), Some(240));
        assert_eq!(pick(&DisplayMode { refresh: 59.94.try_into().unwrap(), ..mode(None) }, offered.clone()), Some(60));
        assert_eq!(pick(&DisplayMode { width: std::num::NonZeroU32::new(2560).unwrap(), ..mode(None) }, offered), None);
    }
}

