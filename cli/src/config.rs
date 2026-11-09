//! The one config every front end feeds: TOML files on the command line,
//! or JSON from the Nix module. Both are layered over the distro's own
//! defaults (configs/default.toml, compiled in) and land in [`Config`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;

pub const DEFAULTS: &str = include_str!("../../configs/default.toml");

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub iso: IsoPin,
    #[serde(default)]
    pub edition: Edition,
    /// Setup and OS UI language. Must be one the source ISO ships.
    #[serde(default = "defaults::language")]
    pub language: String,
    /// Keyboard layout(s), e.g. 0406:00000406 for Danish.
    #[serde(default = "defaults::input_locale")]
    pub input_locale: String,
    /// Windows time zone ID (tzutil /l), not an IANA name.
    #[serde(default = "defaults::time_zone")]
    pub time_zone: String,
    #[serde(default = "defaults::computer_name")]
    pub computer_name: WindowsName,
    pub user: User,
    #[serde(default)]
    pub disk: Disk,
    /// Whole disk `flash` erases when given none. Ignored by the ISO, which
    /// goes by `disk`.
    pub install_target: Option<ByIdPath>,
    /// Keep "Press any key to boot from CD or DVD". Off boots straight into
    /// setup, which is only safe when the disk comes first in the boot order.
    #[serde(default = "defaults::yes")]
    pub boot_prompt: bool,
    pub shell: Shell,
    /// TrueType/OpenType files installed for everyone into C:\Windows\Fonts.
    /// File names must be new there: an existing file can't be replaced
    /// safely (see the README on ACLs).
    #[serde(default)]
    pub fonts: Vec<FileSource>,
    #[serde(default)]
    pub appx: Appx,
    pub wsl: Wsl,
    #[serde(default)]
    pub registry: Registry,
}

/// WSL, which is always installed, and through it the Linux install on the
/// machine's other disk, if there is one.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Wsl {
    /// The WSL MSI (github.com/microsoft/WSL releases), installed during
    /// setup so nothing is downloaded at first boot.
    pub package: Fetched,
    /// A static x86_64 busybox: the whole userland of the small WSL distro
    /// that mounts the Linux install and runs its programs.
    pub busybox: Fetched,
    pub linux: Option<Linux>,
}

/// The machine's Linux install, run from Windows: its disks are handed to
/// WSL, mounted like NixOS's fileSystems, and its apps run chrooted in it
/// as `user`, showing up in the Start Menu.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Linux {
    pub user: LinuxName,
    /// GPT disk GUIDs (`lsblk -dno PTUUID /dev/…`) of the disks the install
    /// is on. Windows and Linux both name a disk by this.
    pub disks: Vec<Guid>,
    /// Mounted in order; the first must be "/".
    pub mounts: Vec<Mount>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    pub device: Device,
    pub target: AbsPath,
    pub fs_type: FsType,
    #[serde(default)]
    pub options: Vec<MountOption>,
}

/// A filesystem by what's written on it, so it's found whatever device name
/// the disk gets inside WSL.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum Device {
    Label(String),
    Uuid(String),
}

impl TryFrom<String> for Device {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let ok = |v: &str| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        match s.split_once('=') {
            Some(("LABEL", v)) if ok(v) => Ok(Self::Label(v.to_owned())),
            Some(("UUID", v)) if ok(v) => Ok(Self::Uuid(v.to_owned())),
            _ => Err(format!("{s:?} isn't LABEL=<label> or UUID=<uuid> (letters, digits, - _ .)")),
        }
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Label(l) => write!(f, "LABEL={l}"),
            Self::Uuid(u) => write!(f, "UUID={u}"),
        }
    }
}

mod defaults {
    use super::WindowsName;
    pub fn language() -> String {
        "en-US".into()
    }
    pub fn input_locale() -> String {
        "0409:00000409".into()
    }
    pub fn time_zone() -> String {
        "UTC".into()
    }
    pub fn computer_name() -> WindowsName {
        WindowsName("defenestrated".into())
    }
    pub fn yes() -> bool {
        true
    }
}

/// The stock ISO everything is built from. Microsoft's links expire after a
/// day, so this pins the file, and `fetch` gets a fresh link for it.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsoPin {
    pub name: String,
    pub hash: Sri,
    /// As the download page's language picker words it.
    pub language: String,
}

#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Edition {
    #[serde(rename = "Windows 11 Home")]
    Home,
    #[default]
    #[serde(rename = "Windows 11 Pro")]
    Pro,
    #[serde(rename = "Windows 11 Education")]
    Education,
    #[serde(rename = "Windows 11 Pro for Workstations")]
    ProForWorkstations,
}

impl Edition {
    /// What wimlib and the WIM's metadata call it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Home => "Windows 11 Home",
            Self::Pro => "Windows 11 Pro",
            Self::Education => "Windows 11 Education",
            Self::ProForWorkstations => "Windows 11 Pro for Workstations",
        }
    }

    /// Microsoft's published generic install keys. They pick an edition and
    /// activate nothing; activation comes from the machine's digital
    /// license. Editions without a known key aren't offered.
    pub fn generic_key(self) -> &'static str {
        match self {
            Self::Home => "YTMG3-N6DKC-DKB77-7M9GH-8HVX7",
            Self::Pro => "VK7JG-NPHTM-C97JM-9MPGT-3V66T",
            Self::Education => "YNMGQ-8RYV3-4PGQ3-C8XTP-7CFBY",
            Self::ProForWorkstations => "DXG7C-N36C4-C4HTG-X4T3X-2YV77",
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct User {
    pub name: WindowsName,
    /// Plaintext, and it ends up on the ISO. Fine for a VM; change it after
    /// install on anything real.
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub auto_logon: bool,
}

#[derive(Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Disk {
    /// Setup shows its disk picker.
    #[default]
    Interactive,
    /// Setup erases this disk without asking and installs to it (UEFI/GPT).
    Wipe {
        #[serde(rename = "diskId")]
        disk_id: u32,
    },
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Shell {
    /// The shell in this repo (shell/), compiled into this binary.
    Builtin,
    /// Command line of something already on the system, e.g. cmd.exe.
    Command(String),
    /// An exe installed to C:\Program Files\defenestrate and run from there.
    Exe(FileSource),
}

/// A file the build needs. Fetched ones are checked against their hash and
/// cached by it, which is also how Nix hands them over: it fills the cache
/// from fixed-output derivations.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum FileSource {
    Fetched(Fetched),
    Local(Local),
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fetched {
    pub url: String,
    pub hash: Sri,
    /// Path of the file wanted inside a .tar.xz.
    pub member: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Local {
    pub path: PathBuf,
    pub member: Option<String>,
}

impl FileSource {
    pub fn member(&self) -> Option<&str> {
        match self {
            Self::Fetched(f) => f.member.as_deref(),
            Self::Local(l) => l.member.as_deref(),
        }
    }

    /// Name the file gets on Windows.
    pub fn file_name(&self) -> Result<String> {
        let full = match (self.member(), self) {
            (Some(m), _) => m,
            (None, Self::Fetched(f)) => f.url.split(['?', '#']).next().unwrap_or(&f.url),
            (None, Self::Local(l)) => l.path.to_str().context("non-UTF-8 path")?,
        };
        let name = full.rsplit('/').next().unwrap_or(full);
        if name.is_empty() || name.contains('\\') {
            bail!("can't tell a file name from {full:?}");
        }
        Ok(name.to_owned())
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Appx {
    /// Provisioned packages to deprovision during specialize, before any
    /// account exists, so no account ever installs them. The build fails on
    /// a name the image doesn't ship.
    #[serde(default)]
    pub remove: Vec<AppxName>,
}

/// key path (backslash separated, relative to the hive root) -> value name
/// ("" is the default value) -> value.
pub type Hive = BTreeMap<String, BTreeMap<String, RegValue>>;

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registry {
    /// HKLM\SOFTWARE.
    #[serde(default)]
    pub software: Hive,
    /// HKLM\SYSTEM. Applied to the running system at install time, so
    /// CurrentControlSet paths work.
    #[serde(default)]
    pub system: Hive,
    /// HKCU of every account created after install (the Default profile).
    #[serde(default)]
    pub default_user: Hive,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum RegValue {
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    Dword(u32),
    Qword(u64),
    Binary(Vec<u8>),
    /// Removes the value.
    Delete,
}

macro_rules! checked_string {
    ($name:ident, $what:literal, $check:expr) => {
        #[derive(Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(s: String) -> Result<Self, String> {
                let check: fn(&str) -> bool = $check;
                if check(&s) { Ok(Self(s)) } else { Err(format!("{s:?} isn't {}", $what)) }
            }
        }

        #[allow(dead_code)]
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

checked_string!(WindowsName, "a Windows name (1-15 letters, digits or -)", |s| {
    (1..=15).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
});
checked_string!(AppxName, "an AppX package name (the part before the first _)", |s| {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
});
// Kernel names (nvme0n1, sda) reorder between boots; by-id doesn't.
checked_string!(ByIdPath, "a /dev/disk/by-id/ path", |s| { s.strip_prefix("/dev/disk/by-id/").is_some_and(|rest| !rest.is_empty() && !rest.contains('/')) });

checked_string!(LinuxName, "a Linux user name", |s| {
    (1..=32).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
});
// Lowercase: Windows prints GUIDs in uppercase, lsblk in lowercase, and the
// logon script compares them case-insensitively anyway.
checked_string!(Guid, "a GUID (8-4-4-4-12 hex digits)", |s| {
    let parts: Vec<&str> = s.split('-').collect();
    parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12]) && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_hexdigit()))
});
// Absolute and plain, since it goes into a shell script.
checked_string!(AbsPath, "an absolute path of plain components", |s| {
    s == "/"
        || (s.starts_with('/')
            && !s.ends_with('/')
            && s[1..].split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))))
});
checked_string!(FsType, "a filesystem type", |s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
checked_string!(MountOption, "a mount option", |s| { !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:=@/".contains(&b)) });

/// A sha256 in SRI form, as Nix's fetchurl takes it.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct Sri([u8; 32]);

impl TryFrom<String> for Sri {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let bad = || format!("{s:?} isn't a sha256 SRI hash (sha256-<base64>)");
        let b64 = s.strip_prefix("sha256-").ok_or_else(bad)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| bad())?;
        Ok(Self(bytes.try_into().map_err(|_| bad())?))
    }
}

impl Sri {
    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// File name in the cache: the SRI with base64's / and + swapped for
    /// _ and -. The Nix module names its cache entries the same way.
    pub fn cache_key(&self) -> String {
        self.to_string().replace('/', "_").replace('+', "-")
    }
}

impl fmt::Display for Sri {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "sha256-{}", base64::engine::general_purpose::STANDARD.encode(self.0))
    }
}

// Layering.

/// Loads `files` in order over the built-in defaults. Tables merge, lists
/// append, and anything else is replaced by the later file.
pub fn load(files: &[PathBuf], with_defaults: bool) -> Result<Config> {
    let config: Config = deserialize(layered(files, with_defaults)?)?;
    config.check()?;
    Ok(config)
}

/// Just the ISO pin, for `fetch`, which needs no machine config.
pub fn load_iso(files: &[PathBuf], with_defaults: bool) -> Result<IsoPin> {
    #[derive(Deserialize)]
    struct OnlyIso {
        iso: IsoPin,
    }
    let mut table = layered(files, with_defaults)?;
    table.retain(|k, _| k == "iso");
    Ok(deserialize::<OnlyIso>(table)?.iso)
}

fn deserialize<T: serde::de::DeserializeOwned>(table: toml::Table) -> Result<T> {
    serde_path_to_error::deserialize(toml::Value::Table(table)).map_err(|e| {
        let path = e.path().to_string();
        anyhow::anyhow!("config: {}: {}", if path == "." { "top level" } else { &path }, e.inner())
    })
}

fn layered(files: &[PathBuf], with_defaults: bool) -> Result<toml::Table> {
    let mut merged = toml::Table::new();
    if with_defaults {
        let defaults: toml::Table = toml::from_str(DEFAULTS).context("built-in configs/default.toml")?;
        merge(&mut merged, defaults);
    }
    for file in files {
        let layer = read_layer(file).with_context(|| format!("reading {}", file.display()))?;
        merge(&mut merged, layer);
    }
    Ok(merged)
}

fn read_layer(file: &Path) -> Result<toml::Table> {
    let text = std::fs::read_to_string(file)?;
    let mut table = if file.extension().is_some_and(|e| e == "json") {
        let json: serde_json::Value = serde_json::from_str(&text)?;
        match json_to_toml(json, &[]) {
            Some(toml::Value::Table(t)) => t,
            _ => bail!("expected a JSON object"),
        }
    } else {
        toml::from_str(&text)?
    };
    let dir = std::path::absolute(file)?.parent().map(Path::to_path_buf).unwrap_or_default();
    resolve_paths(&mut table, &dir);
    Ok(table)
}

/// JSON (from Nix) has null, TOML doesn't. Nix uses null for a registry
/// value to delete, which TOML spells "delete"; anywhere else it means
/// unset.
fn json_to_toml(v: serde_json::Value, path: &[&str]) -> Option<toml::Value> {
    use serde_json::Value as J;
    Some(match v {
        J::Null if path.len() == 4 && path[0] == "registry" => toml::Value::String("delete".into()),
        J::Null => return None,
        J::Bool(b) => toml::Value::Boolean(b),
        J::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        J::String(s) => toml::Value::String(s),
        J::Array(a) => toml::Value::Array(a.into_iter().filter_map(|v| json_to_toml(v, path)).collect()),
        J::Object(o) => toml::Value::Table(
            o.into_iter()
                .filter_map(|(k, v)| {
                    let mut p = path.to_vec();
                    p.push(&k);
                    let v = json_to_toml(v, &p)?;
                    Some((k, v))
                })
                .collect(),
        ),
    })
}

/// Relative paths mean relative to the file that wrote them. Only `path`
/// keys (of a file source) hold paths.
fn resolve_paths(table: &mut toml::Table, dir: &Path) {
    for (key, value) in table.iter_mut() {
        match value {
            toml::Value::String(s) if key == "path" => {
                *s = dir.join(&*s).to_string_lossy().into_owned();
            }
            toml::Value::Table(t) => resolve_paths(t, dir),
            toml::Value::Array(a) => {
                for item in a {
                    if let toml::Value::Table(t) = item {
                        resolve_paths(t, dir);
                    }
                }
            }
            _ => {}
        }
    }
}

fn merge(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge(a, b),
            (Some(toml::Value::Array(a)), toml::Value::Array(b)) => a.extend(b),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

impl Config {
    fn check(&self) -> Result<()> {
        if let Some(linux) = &self.wsl.linux {
            if linux.mounts.first().is_none_or(|m| m.target.as_str() != "/") {
                bail!("config: wsl.linux.mounts: the first mount must be \"/\"");
            }
            if linux.disks.is_empty() {
                bail!("config: wsl.linux.disks: name at least the disk \"/\" is on");
            }
        }
        for (hive, keys) in [("software", &self.registry.software), ("system", &self.registry.system), ("defaultUser", &self.registry.default_user)] {
            for (key, values) in keys {
                if !is_plain(key) || key.is_empty() || key.starts_with('\\') || key.ends_with('\\') {
                    bail!("config: registry.{hive}: key {key:?}: only printable ASCII paths, relative to the hive");
                }
                for name in values.keys() {
                    if !is_plain(name) {
                        bail!("config: registry.{hive}.{key:?}: value name {name:?}: only printable ASCII names");
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn is_plain(s: &str) -> bool {
    s.chars().all(|c| (' '..='~').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let mut t: toml::Table = toml::from_str(DEFAULTS).unwrap();
        t.insert("user".into(), toml::Value::Table(toml::from_str("name = \"x\"").unwrap()));
        let config: Config = toml::Value::Table(t).try_into().unwrap();
        assert!(matches!(config.shell, Shell::Builtin));
        assert_eq!(config.edition, Edition::Pro);
    }

    #[test]
    fn layers_append_lists_and_replace_scalars() {
        let mut a: toml::Table = toml::from_str("x = 1\nl = [1]\n[t]\na = 1").unwrap();
        merge(&mut a, toml::from_str("x = 2\nl = [2]\n[t]\nb = 2").unwrap());
        assert_eq!(a, toml::from_str("x = 2\nl = [1, 2]\n[t]\na = 1\nb = 2").unwrap());
    }

    #[test]
    fn nix_null_deletes_registry_values_and_unsets_the_rest() {
        let json = serde_json::json!({ "installTarget": null, "registry": { "defaultUser": { "K": { "V": null } } } });
        let t = json_to_toml(json, &[]).unwrap();
        assert_eq!(t, toml::from_str::<toml::Value>("[registry.defaultUser.K]\nV = \"delete\"").unwrap());
    }

    #[test]
    fn sri_round_trips_and_keys_are_file_names() {
        let s = "sha256-h3aIe3mSWrvdQH/N/iV9g4K1TiS+UqZGZCa5WoBNZ5A=";
        let sri = Sri::try_from(s.to_owned()).unwrap();
        assert_eq!(sri.to_string(), s);
        assert_eq!(sri.cache_key(), "sha256-h3aIe3mSWrvdQH_N_iV9g4K1TiS-UqZGZCa5WoBNZ5A=");
        assert!(Sri::try_from("sha256-nope".to_owned()).is_err());
    }
}
