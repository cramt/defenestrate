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

/// EFI_IMAGE_SECURITY_DATABASE_GUID, the namespace of Secure Boot's db.
pub const DB_GUID: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";

/// Which of Windows' two boot managers this firmware's Secure Boot db lets
/// start. Since the Windows Production PCA 2011 expired (October 2026),
/// Windows ships two: the frozen last 2011-signed one, and one signed by
/// Windows UEFI CA 2023. Setup puts the 2023 one on the ESP only where db
/// trusts that CA, the 2011 one elsewhere, and Windows' own servicing moves
/// the ESP to 2023 once it has added the CA to db:
/// https://support.microsoft.com/en-us/servicing/os/secure-boot/2026/10/kb5132953-windows-production-ca-2011-expiration-bootloader-updates
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsBootManager {
    Ca2023,
    Pca2011,
}

impl WindowsBootManager {
    /// From the db efivar's bytes (attributes first, as efivarfs has them),
    /// or None when there's no db at all (setup mode: whatever keys go in
    /// later, PCA 2011 is in every Microsoft key set, the 2023 CA not yet).
    pub fn for_db(db: Option<&[u8]>) -> Self {
        match db.and_then(|b| b.get(4..)) {
            Some(lists) if x509_entries(lists).any(|cert| contains(cert, b"Windows UEFI CA 2023")) => Self::Ca2023,
            _ => Self::Pca2011,
        }
    }
}

/// EFI_CERT_X509_GUID (a5c059a1-94e4-4aa7-87b5-ab155c2bf072), as stored.
const CERT_X509: [u8; 16] = [0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0, 0x72];

/// The DER certificates in a run of EFI_SIGNATURE_LISTs (UEFI spec 32.4.1).
/// Stops quietly at anything malformed: what's past it can't be trusted to
/// mean anything.
fn x509_entries(mut lists: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut certs = Vec::new();
    let u32_at = |b: &[u8], at: usize| b.get(at..at + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize);
    while let (Some(list), Some(header), Some(size)) = (u32_at(lists, 16), u32_at(lists, 20), u32_at(lists, 24)) {
        let Some(body) = lists.get(28 + header..list) else { break };
        // Each entry is the owner's GUID, then the signature data.
        if lists[..16] == CERT_X509 && size > 16 {
            certs.extend(body.chunks_exact(size).map(|e| &e[16..]));
        }
        lists = &lists[list..];
    }
    certs.into_iter()
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig_list(kind: [u8; 16], entries: &[&[u8]]) -> Vec<u8> {
        let size = 16 + entries[0].len();
        let mut v = kind.to_vec();
        v.extend(((28 + size * entries.len()) as u32).to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend((size as u32).to_le_bytes());
        for e in entries {
            v.extend([0x77; 16]);
            v.extend(*e);
        }
        v
    }

    #[test]
    fn boot_manager_by_db() {
        let attrs = [7, 0, 0, 0];
        let pca = b"..CN=Microsoft Windows Production PCA 2011..".as_slice();
        let ca23 = b"......CN=Windows UEFI CA 2023......".as_slice();
        let sha = [0u8; 32];
        let db = |lists: &[Vec<u8>]| [attrs.to_vec(), lists.concat()].concat();
        let both = db(&[sig_list([1; 16], &[&sha, &sha]), sig_list(CERT_X509, &[pca]), sig_list(CERT_X509, &[ca23])]);
        assert_eq!(WindowsBootManager::for_db(Some(&both)), WindowsBootManager::Ca2023);
        let old = db(&[sig_list(CERT_X509, &[pca])]);
        assert_eq!(WindowsBootManager::for_db(Some(&old)), WindowsBootManager::Pca2011);
        // The name in a hash list, or past a list that runs off the end, isn't a certificate.
        let not_x509 = db(&[sig_list([1; 16], &[ca23])]);
        assert_eq!(WindowsBootManager::for_db(Some(&not_x509)), WindowsBootManager::Pca2011);
        let mut truncated = db(&[sig_list(CERT_X509, &[pca]), sig_list(CERT_X509, &[ca23])]);
        truncated.truncate(truncated.len() - 3);
        assert_eq!(WindowsBootManager::for_db(Some(&truncated)), WindowsBootManager::Pca2011);
        assert_eq!(WindowsBootManager::for_db(None), WindowsBootManager::Pca2011);
    }

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
