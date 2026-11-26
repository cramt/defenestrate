//! cosmic-comp's pointer speed and key repeat (`cosmic.compositor`'s
//! `input_default` and `xkb_config`) as the Windows registry values that make
//! the same hand movement and the same held key do the same thing.
//!
//! # Pointer
//!
//! On Linux, libinput's flat profile multiplies the device's raw counts by
//! `max(0.005, 1 + speed)` and nothing else: `accelerator_set_speed_flat`
//! sets that factor, `accelerator_filter_flat` returns `factor * counts`
//! (libinput 1.31, src/filter-flat.c). The filter is created with the
//! device's DPI but never reads it, unlike the adaptive profiles'
//! `normalize_for_dpi` (src/filter-private.h), so there's no normalising to
//! 1000 dpi: one count is `factor` pixels whatever the mouse's resolution.
//! cosmic-comp adds that delta, which smithay types as logical
//! (`PointerMotionEvent::delta`), to the pointer's position in its global
//! logical space (cosmic-comp src/input/mod.rs, `InputEvent::PointerMotion`),
//! so on an output at scale `s` a count is `factor * s` physical pixels.
//!
//! Windows' "Enhance pointer precision" curve can be any straight line, which
//! is how a multiplier off the pointer speed slider's steps is reached
//! exactly (Mark Cranness's "MarkC" fix). Windows 8 and later turn `counts`
//! per packet into `pixels` as (Mark Cranness's reverse engineering, as his
//! Fix Builder 2.6 computes it, Builder.vbs:629-651 and :874, summarised with
//! its truncating 16.16 helpers in
//! https://github.com/anilmetin0/winmouse/blob/main/docs/research/windows-pointer.md
//! §1.3-1.4 and §1.10; the per-step integer form is his, on
//! https://donewmouseaccel.blogspot.com/2010/03/markc-windows-7-mouse-acceleration-fix.html):
//!
//! ```text
//! pixels = counts * (Y * max(96, DPI) / 120 * MouseSensitivity / 10) / (X * 3.5)
//! ```
//!
//! for a curve segment from the origin to (X, Y), in 16.16 fixed point with
//! each step truncated. 3.5 is Windows' counts-to-curve-units constant (MS's
//! 125 Hz / 400 dpi reference mouse, rounded), DPI the system's (96 at
//! 100 %), and the refresh rate only entered on XP and Vista. MarkC's
//! shipped 1-to-1 curves for Windows 10 (X 12.8, Y 56 per step at 100 %; X 16
//! at 125 %) come out of [`windows_gain`] at exactly 1, which the tests pin.
//!
//! Both sides scale with the display: Windows' DPI factor grows the pixels
//! per count as cosmic-comp's logical space does. So with Windows at the
//! Linux output's scale (`displayScale`, which this also sets as Windows'
//! one scale for every monitor), the target is `factor * scale` physical
//! pixels per count on both. The curve is four equal steps from the origin
//! (MarkC: one segment from the origin would leave a pixel of error at a
//! reversal), and X is even so `X * 3.5` has no fraction for Windows to drop
//! per segment. What it reaches is the 16.16 multiplier nearest the factor,
//! within 1/65536.
//!
//! # Keyboard
//!
//! FilterKeys ("Keyboard Response") does take repeat delay and rate in
//! milliseconds, but only while it's on, and on with BounceKeys off it also
//! requires a nonzero `iWaitMSec`, the SlowKeys hold every key needs before
//! Windows accepts it
//! (https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-filterkeys,
//! Remarks), besides its click sound flag. That's a delay on every
//! keystroke, so instead: `KeyboardDelay` 0-3 is about 250-1000 ms in 250 ms
//! steps, and `KeyboardSpeed` 0-31 about 2.5-30 Hz on a linear scale
//! (SPI_SETKEYBOARDDELAY and SPI_SETKEYBOARDSPEED,
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-systemparametersinfoa),
//! and the nearest of each is taken. COSMIC's 600 ms and 25 Hz are 500 ms
//! and about 24.7 Hz there.

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::config::RegValue;

/// cosmic-comp's `input_default` (cosmic-comp-config's InputConfig): only
/// what has a Windows counterpart. The other InputConfig keys fail the build.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InputDefault {
    /// Only `Enabled`: Windows has no switch for "no pointer".
    pub state: DeviceState,
    pub acceleration: Acceleration,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Enabled,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Acceleration {
    /// Only `Flat`: libinput's adaptive curve depends on velocity in
    /// physical units, Windows' on counts per USB packet, and there's no
    /// mapping between the two.
    pub profile: Profile,
    pub speed: Speed,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Flat,
}

/// libinput's speed, -1 to 1.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(try_from = "f64")]
pub struct Speed(f64);

impl TryFrom<f64> for Speed {
    type Error = String;
    fn try_from(v: f64) -> Result<Self, String> {
        if (-1.0..=1.0).contains(&v) { Ok(Speed(v)) } else { Err(format!("libinput's speed goes from -1 to 1, got {v}")) }
    }
}

impl Speed {
    /// libinput's flat multiplier (src/filter-flat.c, accelerator_set_speed_flat).
    pub fn factor(self) -> f64 {
        (1.0 + self.0).max(0.005)
    }
}

/// cosmic-comp's `xkb_config`, only its repeat: the layout is `inputLocale`'s,
/// in Windows' own terms.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct XkbRepeat {
    /// Milliseconds before a held key repeats.
    pub repeat_delay: u32,
    /// Repeats a second.
    pub repeat_rate: Rate,
}

/// Repeats a second; Windows can't turn repeat off, so not 0.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "u32")]
pub struct Rate(u32);

impl TryFrom<u32> for Rate {
    type Error = String;
    fn try_from(v: u32) -> Result<Self, String> {
        if v > 0 { Ok(Rate(v)) } else { Err("repeat_rate 0 (no key repeat) has no Windows counterpart".into()) }
    }
}

/// Windows' one display scale for every monitor, as COSMIC writes an
/// output's scale (1.0 is 100 %). Kept as Windows' LogPixels, 96 per 100 %,
/// which only goes from 100 % to 500 % in whole steps of 1/96.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(try_from = "f64")]
pub struct DisplayScale {
    log_pixels: u32,
}

impl TryFrom<f64> for DisplayScale {
    type Error = String;
    fn try_from(scale: f64) -> Result<Self, String> {
        let log_pixels = scale * 96.0;
        if (1.0..=5.0).contains(&scale) && log_pixels.fract() == 0.0 {
            Ok(DisplayScale { log_pixels: log_pixels as u32 })
        } else {
            Err(format!("a display scale goes from 1 to 5 in steps Windows has (96ths: 1.25, 1.5, 1.75, ...), got {scale}"))
        }
    }
}

impl DisplayScale {
    fn scale(self) -> f64 {
        f64::from(self.log_pixels) / 96.0
    }

    /// "Let me choose one scaling level for all my displays" with this one
    /// (https://learn.microsoft.com/windows-hardware/manufacture/desktop/dpi-related-apis-and-registry-settings,
    /// Win8DpiScaling and LogPixels): the pointer curve is computed for one
    /// DPI, and per-monitor scaling would leave which one Windows uses to
    /// Windows.
    pub fn registry(self) -> Vec<(&'static str, &'static str, RegValue)> {
        vec![
            (DESKTOP, "Win8DpiScaling", RegValue::Dword(1)),
            (DESKTOP, "LogPixels", RegValue::Dword(self.log_pixels)),
        ]
    }
}

const DESKTOP: &str = "Control Panel\\Desktop";
const MOUSE: &str = "Control Panel\\Mouse";
const KEYBOARD: &str = "Control Panel\\Keyboard";

/// 1.0 in Windows' 16.16 fixed point.
const ONE: u64 = 1 << 16;

/// Windows 8+'s multiplier, in 16.16, for a curve segment from the origin to
/// (x, y) (both 16.16) at this LogPixels, MouseSensitivity 10: every step
/// truncated as Windows (and MarkC's Builder.vbs, which matches it) does.
/// MouseSensitivity 10 is a factor of exactly 1, so it drops out.
pub fn windows_gain(x: u64, y: u64, log_pixels: u32) -> u64 {
    let dpi_factor = u64::from(log_pixels.max(96)) * ONE / 120;
    let pixels = y * dpi_factor / ONE;
    let counts = x * (7 * ONE / 2) / ONE;
    pixels * ONE / counts
}

/// The straight-line curve: X and Y of points 1-4 (point 0 is the origin),
/// in 16.16, and the multiplier Windows makes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Curve {
    pub x_step: u64,
    pub y_step: u64,
    pub gain: u64,
}

impl Curve {
    /// The curve whose multiplier is the 16.16 value nearest `target`
    /// physical pixels per count. MarkC's Y step of 56 first, then larger
    /// ones, which space the reachable multipliers more finely, if 56 can't
    /// hit it (a large multiplier at a large scale).
    pub fn for_gain(target: f64, scale: DisplayScale) -> Result<Curve> {
        let gain = (target * ONE as f64).round() as u64;
        if gain == 0 {
            bail!("a pointer multiplier of {target} is below what Windows can represent");
        }
        for y in 56..=1024u64 {
            let y_step = y * ONE;
            let pixels = y_step * (u64::from(scale.log_pixels.max(96)) * ONE / 120) / ONE;
            // gain = pixels * ONE / counts, so counts near this, and
            // counts = x * 3.5 for an even x.
            let ideal_x = (pixels * ONE) as f64 / gain as f64 / 3.5;
            let around = (ideal_x / 2.0).round() as u64 * 2;
            let found = (0..16u64)
                .flat_map(|d| [around.saturating_sub(2 * d), around + 2 * d])
                .filter(|&x| x > 0)
                .find(|&x| windows_gain(x, y_step, scale.log_pixels) == gain);
            if let Some(x_step) = found {
                // Point 4 has to stay a positive 32-bit 16.16 value.
                if 4 * x_step > i32::MAX as u64 {
                    bail!("a pointer multiplier of {target} at scale {} needs a curve longer than Windows' 16.16 holds", scale.scale());
                }
                return Ok(Curve { x_step, y_step, gain });
            }
        }
        bail!("no straight Windows curve reaches a pointer multiplier of {target} at scale {}", scale.scale())
    }

    fn points(step: u64) -> Vec<u8> {
        (0..5u64).flat_map(|i| (i * step).to_le_bytes()).collect()
    }

    #[cfg(test)]
    fn multiplier(self) -> f64 {
        self.gain as f64 / ONE as f64
    }
}

/// The pointer's registry values (HKCU), for `scale` both here and on Linux.
pub fn pointer(input: &InputDefault, scale: DisplayScale) -> Result<Vec<(&'static str, &'static str, RegValue)>> {
    let curve = Curve::for_gain(input.acceleration.speed.factor() * scale.scale(), scale)?;
    let sz = |s: &str| RegValue::Sz(s.into());
    Ok(vec![
        // "Enhance pointer precision" on, which is what makes Windows use the
        // curve; the thresholds are only read by the pre-XP doubling
        // acceleration, 0 as Control Panel writes them with it off.
        (MOUSE, "MouseSpeed", sz("1")),
        (MOUSE, "MouseThreshold1", sz("0")),
        (MOUSE, "MouseThreshold2", sz("0")),
        // The slider's middle, a factor of 1; the curve carries the speed.
        (MOUSE, "MouseSensitivity", sz("10")),
        (MOUSE, "SmoothMouseXCurve", RegValue::Binary(Curve::points(curve.x_step))),
        (MOUSE, "SmoothMouseYCurve", RegValue::Binary(Curve::points(curve.y_step))),
    ])
}

/// The nearest of Windows' 250 ms steps from 250 to 1000.
pub fn keyboard_delay(ms: u32) -> u32 {
    ((f64::from(ms) / 250.0).round() as u32).clamp(1, 4) - 1
}

/// The nearest step on Windows' linear 0-31 = 2.5-30 Hz.
pub fn keyboard_speed(rate: Rate) -> u32 {
    ((f64::from(rate.0) - 2.5) * 31.0 / 27.5).round().clamp(0.0, 31.0) as u32
}

pub fn keyboard(repeat: &XkbRepeat) -> Vec<(&'static str, &'static str, RegValue)> {
    vec![
        (KEYBOARD, "KeyboardDelay", RegValue::Sz(keyboard_delay(repeat.repeat_delay).to_string())),
        (KEYBOARD, "KeyboardSpeed", RegValue::Sz(keyboard_speed(repeat.repeat_rate).to_string())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(s: f64) -> DisplayScale {
        DisplayScale::try_from(s).unwrap()
    }

    /// MarkC's Windows_10+8.x_MouseFix_ItemsSize=100%_Scale=1-to-1_@6-of-11.reg
    /// and its 125 % sibling, checked 1-to-1 by their users with
    /// MouseMovementRecorder.
    #[test]
    fn markc_curves_are_one_to_one() {
        assert_eq!(windows_gain(0x0C_CCC0, 56 * ONE, 96), ONE);
        assert_eq!(windows_gain(0x10_0000, 56 * ONE, 120), ONE);
        // And a 1x Linux at 100 % gets MarkC's very curve.
        let c = Curve::for_gain(1.0, scale(1.0)).unwrap();
        assert_eq!((c.x_step, c.y_step), (0x0C_CCC0, 56 * ONE));
        let bytes = Curve::points(c.x_step);
        assert_eq!(&bytes[8..16], &[0xC0, 0xCC, 0x0C, 0, 0, 0, 0, 0]);
        assert_eq!(&bytes[32..40], &[0x00, 0x33, 0x33, 0, 0, 0, 0, 0]);
    }

    /// Every speed libinput takes, at the scales Windows' Settings offers,
    /// lands on the 16.16 multiplier nearest libinput's, and the curve is
    /// one Windows can read: even X, every point a positive 32-bit value.
    #[test]
    fn every_libinput_speed_is_reached() {
        for s in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
            for i in -100..=100 {
                let speed = Speed::try_from(f64::from(i) / 100.0).unwrap();
                let target = speed.factor() * s;
                let c = Curve::for_gain(target, scale(s)).unwrap_or_else(|e| panic!("{e}"));
                assert!((c.multiplier() - target).abs() <= 0.5 / ONE as f64, "speed {} scale {s}: {} for {target}", speed.0, c.multiplier());
                assert_eq!(c.x_step % 2, 0);
                assert!(4 * c.x_step <= i32::MAX as u64 && 4 * c.y_step <= i32::MAX as u64);
                // Each of the four segments has the same slope in Windows' arithmetic.
                for seg in 1..4 {
                    assert_eq!(windows_gain(seg * c.x_step, seg * c.y_step, scale(s).log_pixels), c.gain, "segment {seg}");
                }
            }
        }
    }

    #[test]
    fn saturn() {
        // nixconf's cosmic.nix: flat, 0.6042271248762552, at 100 %.
        let input: InputDefault = toml::from_str("state = \"Enabled\"\nacceleration = { profile = \"Flat\", speed = 0.6042271248762552 }").unwrap();
        let values = pointer(&input, scale(1.0)).unwrap();
        let curve = Curve::for_gain(input.acceleration.speed.factor(), scale(1.0)).unwrap();
        assert!((curve.multiplier() - 1.6042271248762552).abs() < 1e-5, "{}", curve.multiplier());
        assert!(values.contains(&(MOUSE, "MouseSpeed", RegValue::Sz("1".into()))));
        assert!(values.contains(&(MOUSE, "MouseSensitivity", RegValue::Sz("10".into()))));
        let repeat: XkbRepeat = toml::from_str("repeat_delay = 600\nrepeat_rate = 25").unwrap();
        assert_eq!(keyboard(&repeat), [(KEYBOARD, "KeyboardDelay", RegValue::Sz("1".into())), (KEYBOARD, "KeyboardSpeed", RegValue::Sz("25".into()))]);
    }

    #[test]
    fn keyboard_steps_are_the_nearest() {
        let rate = |r| Rate::try_from(r).unwrap();
        assert_eq!([0, 250, 374, 376, 600, 700, 1000, 5000].map(keyboard_delay), [0, 0, 0, 1, 1, 2, 3, 3]);
        assert_eq!([1, 2, 3, 25, 30, 100].map(|r| keyboard_speed(rate(r))), [0, 0, 1, 25, 31, 31]);
        assert!(toml::from_str::<XkbRepeat>("repeat_delay = 600\nrepeat_rate = 0").is_err());
        assert!(toml::from_str::<XkbRepeat>("repeat_delay = 600\nrepeat_rate = 25\nlayout = \"dk\"").is_err());
    }

    #[test]
    fn only_what_windows_can_do_is_taken() {
        let parse = |s: &str| toml::from_str::<InputDefault>(s);
        assert!(parse("state = \"Enabled\"\nacceleration = { profile = \"Flat\", speed = 0.5 }").is_ok());
        for bad in [
            "state = \"Enabled\"\nacceleration = { profile = \"Adaptive\", speed = 0.5 }",
            "state = \"Disabled\"\nacceleration = { profile = \"Flat\", speed = 0.5 }",
            "state = \"Enabled\"\nacceleration = { profile = \"Flat\", speed = 1.5 }",
            "state = \"Enabled\"\nacceleration = { speed = 0.5 }",
            "state = \"Enabled\"\nacceleration = { profile = \"Flat\", speed = 0.5 }\nleft_handed = true",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        for bad in [0.5, 0.99, 1.3, 6.0] {
            assert!(DisplayScale::try_from(bad).is_err(), "{bad}");
        }
        assert_eq!(scale(1.25).registry()[1].2, RegValue::Dword(120));
    }
}
