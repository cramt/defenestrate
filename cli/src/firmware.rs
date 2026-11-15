//! UEFI boot entries, as `efibootmgr -v` lists them: what `flash` adds and
//! prunes, and what `reboot` boots into.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// The XXXX of a BootXXXX variable: four hex digits, kept uppercase as
/// efibootmgr prints them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootNum(String);

impl FromStr for BootNum {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.strip_prefix("Boot").unwrap_or(s);
        if s.len() == 4 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(BootNum(s.to_ascii_uppercase()))
        } else {
            Err(format!("{s:?} isn't a boot entry number (four hex digits, as in Boot0003)"))
        }
    }
}

impl fmt::Display for BootNum {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<std::ffi::OsStr> for BootNum {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

/// A "Windows Boot Manager" entry and the partition UUID of the ESP it
/// boots from.
#[derive(Debug, PartialEq, Eq)]
pub struct WindowsEntry {
    pub num: BootNum,
    pub esp: String,
}

impl WindowsEntry {
    /// Whether its ESP is on a disk attached right now. Every flash makes a
    /// new ESP, so older entries point at nothing.
    pub fn live(&self) -> bool {
        Path::new("/dev/disk/by-partuuid").join(&self.esp).exists()
    }
}

pub fn windows_entries(efibootmgr_v: &str) -> Vec<WindowsEntry> {
    efibootmgr_v
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Boot")?;
            let (num, rest) = rest.split_at_checked(4)?;
            let num = num.parse().ok()?;
            let rest = rest.strip_prefix('*').unwrap_or(rest);
            if !rest.starts_with(" Windows Boot Manager") {
                return None;
            }
            let hd = &rest[rest.find("HD(")? + 3..];
            let fields: Vec<&str> = hd.split(',').collect();
            if fields.get(1) != Some(&"GPT") {
                return None;
            }
            Some(WindowsEntry { num, esp: fields.get(2)?.to_string() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_windows_entries() {
        let text = "BootCurrent: 0001\nBoot000A* Windows Boot Manager\tHD(1,GPT,0fd4e1d5-aaaa-bbbb-cccc-0123456789ab,0x800,0x96000)/File(\\EFI\\Microsoft\\Boot\\bootmgfw.efi)\nBoot0001* NixOS\tHD(1,GPT,11111111-2222-3333-4444-555555555555,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)\n";
        assert_eq!(
            windows_entries(text),
            vec![WindowsEntry { num: BootNum("000A".into()), esp: "0fd4e1d5-aaaa-bbbb-cccc-0123456789ab".into() }]
        );
    }

    #[test]
    fn boot_numbers() {
        assert_eq!("0003".parse(), Ok(BootNum("0003".into())));
        assert_eq!("Boot00af".parse(), Ok(BootNum("00AF".into())));
        assert!("3".parse::<BootNum>().is_err());
        assert!("00G1".parse::<BootNum>().is_err());
    }
}
