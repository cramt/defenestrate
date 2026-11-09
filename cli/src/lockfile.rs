//! apps.lock.json: what `dfnstrate lock` writes and `build` reads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lock {
    pub winget_pkgs_rev: String,
    pub apps: BTreeMap<String, LockedApp>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum LockedApp {
    /// Installer pinned and baked into the image, installed offline.
    Baked(Baked),
    /// A winget-pkgs version that winget installs online.
    Winget { version: String },
    /// The store only serves a product's current version, so this pins
    /// nothing; it records that the ID exists.
    Msstore {},
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Baked {
    pub version: String,
    pub url: String,
    /// SRI, as fetchurl takes it.
    pub hash: String,
    /// Store name, and the file name on the machine.
    pub file: String,
    pub installer_type: InstallerType,
    /// Decides how it's run: per-user installers break when elevated
    /// (Discord's Squirrel Update.exe crashes with 0xC0000005), so those
    /// run as the plain user, everything else elevated.
    pub scope: Scope,
    /// Appended verbatim to the command line: Windows hands programs one
    /// string, not argv, and manifests write switches that way.
    pub args: String,
    pub success_codes: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Machine,
    User,
    /// The manifest doesn't say.
    Unspecified,
}

/// The installer types that can run unattended from a local file. msix,
/// zip, portable, msstore and friends aren't here, so they can't be locked
/// as baked at all.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum InstallerType {
    Msi,
    Wix,
    Nullsoft,
    Inno,
    Burn,
    Exe,
}

impl InstallerType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "msi" => Self::Msi,
            "wix" => Self::Wix,
            "nullsoft" => Self::Nullsoft,
            "inno" => Self::Inno,
            "burn" => Self::Burn,
            "exe" => Self::Exe,
            _ => return None,
        })
    }

    /// What winget itself passes when a manifest has no Silent switch.
    /// A bare exe has no convention, so the manifest must say.
    pub fn default_silent(self) -> Option<&'static str> {
        match self {
            Self::Msi | Self::Wix => Some("/qn /norestart"),
            Self::Nullsoft => Some("/S"),
            Self::Inno => Some("/SP- /VERYSILENT /SUPPRESSMSGBOXES /NORESTART"),
            Self::Burn => Some("/quiet /norestart"),
            Self::Exe => None,
        }
    }

    /// 3010 and 1641 are Windows Installer's "done, reboot needed/started".
    pub fn default_success_codes(self) -> &'static [i64] {
        match self {
            Self::Msi | Self::Wix | Self::Burn => &[0, 3010, 1641],
            Self::Nullsoft | Self::Inno | Self::Exe => &[0],
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Msi | Self::Wix => "msi",
            _ => "exe",
        }
    }
}
