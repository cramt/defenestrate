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
    /// Setup and OS UI language. Must be one the source ISO ships, which
    /// `build iso` checks.
    #[serde(default = "defaults::language")]
    pub language: Language,
    /// Keyboard layout(s), e.g. 0406:00000406 for Danish.
    #[serde(default = "defaults::input_locale")]
    pub input_locale: InputLocale,
    /// Windows time zone ID (tzutil /l), not an IANA name. Must be one the
    /// image knows, which `build iso` checks.
    #[serde(default = "defaults::time_zone")]
    pub time_zone: TimeZone,
    #[serde(default = "defaults::computer_name")]
    pub computer_name: WindowsName,
    pub user: User,
    #[serde(default)]
    pub disk: SetupDisk,
    /// Whole disk `flash` erases when given none. Ignored by the ISO, which
    /// goes by `disk`.
    pub install_target: Option<ByIdPath>,
    /// Keep "Press any key to boot from CD or DVD". Off boots straight into
    /// setup, which is only safe when the disk comes first in the boot order.
    #[serde(default = "defaults::yes")]
    pub boot_prompt: bool,
    pub shell: Shell,
    /// How the built-in shell behaves, under cosmic-comp's own setting names
    /// (cosmic.rs).
    pub cosmic: crate::cosmic::Cosmic,
    /// Monitor modes the shell sets at logon and when a monitor appears;
    /// Windows' own pick for one it doesn't know can be 60 Hz on a 240 Hz
    /// monitor. Only with the built-in shell.
    #[serde(default)]
    pub displays: Vec<DisplayMode>,
    /// Windows' one display scale for every monitor, as COSMIC writes an
    /// output's (1.0 is 100 %); unset, Windows picks one per monitor. Set it
    /// to the Linux output's scale: `cosmic.compositor.input_default` is
    /// converted for the two being the same (input.rs).
    pub display_scale: Option<crate::input::DisplayScale>,
    /// Command lines the shell runs once per logon, after the Run keys and
    /// Startup folders it runs as explorer would. Only with the built-in
    /// shell; carried in shell.json, so `update` brings changes.
    #[serde(default)]
    pub autostart: Vec<String>,
    /// TrueType/OpenType files installed for everyone into C:\Windows\Fonts.
    /// File names must be new there: an existing file can't be replaced
    /// safely (see the README on ACLs).
    #[serde(default)]
    pub fonts: Vec<FileSource>,
    /// winget package IDs, installed at the machine's first real boot with
    /// whatever version winget has then. Not pinned: game launchers update
    /// themselves anyway.
    #[serde(default)]
    pub apps: Vec<AppId>,
    pub gpu: Gpu,
    /// Driver packages: archives 7-Zip opens (a .cab from the Microsoft
    /// Update Catalog, say) with .inf files in them. Added to the image and
    /// put in the driver store during specialize (pnputil), which also
    /// installs each onto any matching device; one the build machine's VM
    /// doesn't have is installed when Windows finds the device on the real
    /// machine. Pinned, unlike `gpu`'s, and there with no network.
    #[serde(default)]
    pub drivers: Vec<FileSource>,
    /// Windows updates (.msu from the Update Catalog), added in this order
    /// to the applied image by DISM in setup's WinPE, before Windows first
    /// starts (iso.rs). The image's patch level is the ISO's plus these;
    /// Windows Update itself is off (configs/default.toml).
    #[serde(default)]
    pub updates: Vec<FileSource>,
    #[serde(default)]
    pub appx: Appx,
    pub wsl: Wsl,
    /// Which firmware boot entry "Reboot to Linux" boots once.
    #[serde(default)]
    pub linux_boot_entry: LinuxBootEntry,
    #[serde(default)]
    pub registry: Registry,
    /// Default apps by file extension (".html") or URL scheme ("https"):
    /// the ProgId each should go to, as `dism /online
    /// /export-defaultappassociations` lists them. Windows keeps this the
    /// user's own choice (UserChoice's hash, UCPD.sys), and the policy for
    /// it only applies on domain-joined machines (saturn, 2026-11-20), so
    /// the shell asks: at each logon, while one isn't set and the app that
    /// registers its ProgId is installed, it opens Settings at that app's
    /// default-apps page once, for one click on "Set default".
    #[serde(default)]
    pub default_apps: BTreeMap<AssocId, ProgId>,
    /// What a reflash carries over from the install it replaces (persist.rs).
    #[serde(default)]
    pub persist: Persist,
}

/// What `flash` copies off the Windows it's about to erase, into a store on
/// Linux, and puts back onto the new install before its first boot. One
/// entry per app rather than a list of paths: each app's files only work
/// with its own handling (Firefox's downgrade check, which profile it
/// opens), and an app can't be named twice.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Persist {
    /// The user's whole %APPDATA%\Mozilla\Firefox: profiles.ini,
    /// installs.ini and every profile.
    pub firefox: Option<FirefoxPersist>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FirefoxPersist {
    /// While the store has no Firefox of this machine's (the first install),
    /// a new profile is made from this one's logins instead.
    pub seed: Option<FirefoxSeed>,
}

/// A Gecko browser's profile on this Linux machine (Firefox's, Zen's,
/// LibreWolf's). Only its logged-in state is taken, never the whole
/// profile, which would bring Linux paths and the other browser's version.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FirefoxSeed {
    /// The profile directory, e.g. /home/me/.config/zen/default.
    pub profile: AbsPath,
    /// Also history and bookmarks (places.sqlite, favicons.sqlite).
    #[serde(default)]
    pub history: bool,
}

/// The firmware boot entry the Start Menu's "Reboot to Linux" sets as the
/// one-time boot (UEFI BootNext). Windows can't tell a Linux entry from any
/// other non-Windows one, so `auto` guesses and a description pins it.
#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum LinuxBootEntry {
    /// The first entry in the firmware's boot order that boots a file off a
    /// disk partition other than Windows Boot Manager or the removable-media
    /// fallback (\EFI\BOOT\BOOTX64.EFI), which is what a USB stick left in
    /// would be. Shell, network and setup-menu entries boot no such file.
    #[default]
    Auto,
    /// The entry with exactly this description (`efibootmgr` on Linux lists
    /// them), e.g. "Linux Boot Manager" for systemd-boot.
    Description(BootEntryDescription),
    /// No "Reboot to Linux" at all: a Windows-only machine.
    None,
}

/// WSL, which is always installed, and through it the Linux install on the
/// machine's other disk, if there is one.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Wsl {
    /// The WSL MSI (github.com/microsoft/WSL releases), installed during
    /// setup so nothing is downloaded at first boot.
    pub package: FileSource,
    /// A static x86_64 busybox: the whole userland of the small WSL distro
    /// that mounts the Linux install and runs its programs.
    pub busybox: FileSource,
    pub linux: Option<Linux>,
}

/// The machine's Linux install, run from Windows: its disks are handed to
/// WSL, mounted like NixOS's fileSystems, and its programs run chrooted in
/// it as `user`, from the Start Menu's "Linux terminal".
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Linux {
    pub user: LinuxName,
    /// GPT disk GUIDs (`lsblk -dno PTUUID /dev/…`) of the disks the install
    /// is on. Windows and Linux both name a disk by this.
    pub disks: Vec<crate::gpt::Guid>,
    /// Mounted in order; the first must be "/".
    pub mounts: Vec<Mount>,
    /// Copied from the install into the Windows user's profile at every
    /// logon, replacing what's there: destination under %USERPROFILE% ->
    /// absolute path in the install. Files or directories; symlinks are
    /// followed inside the install, so home-manager's links into the Nix
    /// store work. Read as root, so this is also how rendered secrets get
    /// over: they land with the profile's ACL (the user, SYSTEM and
    /// Administrators). Destinations dropped from here are deleted at the
    /// next logon.
    #[serde(default)]
    pub files: BTreeMap<ProfilePath, AbsPath>,
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
    use super::{InputLocale, Language, TimeZone, WindowsName};
    pub fn language() -> Language {
        Language("en-US".into())
    }
    pub fn input_locale() -> InputLocale {
        InputLocale("0409:00000409".into())
    }
    pub fn time_zone() -> TimeZone {
        TimeZone("UTC".into())
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
#[serde(try_from = "String")]
pub enum Edition {
    Home,
    #[default]
    Pro,
    Education,
    ProForWorkstations,
}

impl TryFrom<String> for Edition {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::ALL.into_iter().find(|e| e.name() == s).ok_or_else(|| {
            let names: Vec<String> = Self::ALL.iter().map(|e| format!("{:?}", e.name())).collect();
            format!("{s:?} isn't one of {}", names.join(", "))
        })
    }
}

impl Edition {
    const ALL: [Edition; 4] = [Self::Home, Self::Pro, Self::Education, Self::ProForWorkstations];

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

/// Which disk setup installs to. Not disk::Disk, a disk on this machine.
#[derive(Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum SetupDisk {
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

/// A file the build needs, or with `member` the file of that path inside
/// it (a .tar.xz).
#[derive(Deserialize, Debug, Clone)]
#[serde(try_from = "RawFileSource")]
pub struct FileSource {
    pub origin: Origin,
    pub member: Option<String>,
}

/// Fetched ones are checked against their hash and cached by it, which is
/// also how Nix hands them over: it fills the cache from fixed-output
/// derivations. Tracked ones are whatever's newest when dfnstrate runs,
/// which Nix can't do: `dfnstrate pins` turns them into Fetched ones.
#[derive(Debug, Clone)]
pub enum Origin {
    Fetched {
        url: String,
        hash: Sri,
        /// What `dfnstrate pins` moves this pin along with. The build
        /// ignores it (a pin is a pin) but parses it, so a malformed one
        /// fails there too rather than only when pins runs, which reads the
        /// file itself.
        #[allow(dead_code)]
        track: Option<Track>,
    },
    Tracked(Track),
    Local(PathBuf),
}

/// Every key a file source may have, so a wrong combination gets an error
/// that says which, rather than serde's "did not match any variant".
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawFileSource {
    url: Option<String>,
    hash: Option<Sri>,
    track: Option<Track>,
    path: Option<PathBuf>,
    member: Option<String>,
}

impl TryFrom<RawFileSource> for FileSource {
    type Error = &'static str;
    fn try_from(r: RawFileSource) -> Result<Self, Self::Error> {
        let origin = match (r.url, r.hash, r.track, r.path) {
            (Some(url), Some(hash), track, None) => Origin::Fetched { url, hash, track },
            (None, None, Some(track), None) => Origin::Tracked(track),
            (None, None, None, Some(path)) => Origin::Local(path),
            (Some(_), None, _, None) => return Err("a url needs its hash (`dfnstrate pins` finds it)"),
            (None, Some(_), _, None) => return Err("a hash without the url it's of"),
            (_, _, _, Some(_)) => return Err("a path is a local file: no url, hash or track with it"),
            (None, None, None, None) => return Err("a file is a url and hash, a track, or a path"),
        };
        Ok(Self { origin, member: r.member })
    }
}

/// Where the newest version of a download is found.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum Track {
    /// The asset matching `asset` (`*` is any run of characters) of the
    /// repo's latest release, prereleases left out.
    Github { github: GithubRepo, asset: String },
    /// The newest driver the Microsoft Update Catalog has for a hardware
    /// ID, `VEN_1002&DEV_747E` say: the .cab from its search results,
    /// Windows Server ones left out.
    Catalog { catalog: CatalogQuery },
    /// The newest non-preview update in the Microsoft Update Catalog whose
    /// title contains this ("Cumulative Update for Windows 11, version 26H2
    /// for x64-based Systems"): its .msu named after the title's KB, leaving
    /// out the checkpoint updates the catalog lists alongside it.
    Update { update: CatalogQuery },
}

impl fmt::Display for Track {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Github { github, asset } => write!(f, "github {github} {asset}"),
            Self::Catalog { catalog } => write!(f, "catalog {catalog}"),
            Self::Update { update } => write!(f, "update {update}"),
        }
    }
}

/// Name a file gets on Windows: the member's, or else the last part of
/// where it came from (a URL or a path).
pub fn file_name(member: Option<&str>, from: &str) -> Result<String> {
    let full = member.unwrap_or_else(|| from.split(['?', '#']).next().unwrap_or(from));
    let name = full.rsplit('/').next().unwrap_or(full);
    if name.is_empty() || name.contains('\\') {
        bail!("can't tell a file name from {full:?}");
    }
    Ok(name.to_owned())
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

/// GPU drivers, installed by the apps task at the first real boot: the
/// image is built on a VM's virtual GPU, so only then is the real one known.
/// For a pinned driver that's there from the start, see `drivers`.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gpu {
    /// Have Windows Update install its display drivers at first boot rather
    /// than whenever it gets round to it. The only route that installs an
    /// actual driver without a click (see default.toml).
    pub windows_update: bool,
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

/// One monitor's mode, as the shell reads it (shell/config).
pub use shell_config::DisplayMode;

macro_rules! checked_string {
    ($name:ident, $what:literal, $check:expr) => {
        #[derive(Deserialize, serde::Serialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(try_from = "String", into = "String")]
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

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
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

checked_string!(AppId, "a winget package ID", |s| {
    s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
});
// Any printable text: it goes to the reboot script as JSON, never through a
// shell, and UEFI caps descriptions nowhere near this.
checked_string!(BootEntryDescription, "a boot entry description (1-255 printable characters)", |s| {
    (1..=255).contains(&s.chars().count()) && !s.chars().any(char::is_control)
});
checked_string!(LinuxName, "a Linux user name", |s| {
    (1..=32).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
});
// What unattend's <UILanguage> takes: a BCP-47 tag like en-US or
// sr-Latn-RS. Whether the ISO has it, only the ISO can say (iso.rs).
checked_string!(Language, "a language tag like en-US", |s| {
    let mut parts = s.split('-');
    parts.next().is_some_and(|p| (2..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphabetic()))
        && s.contains('-')
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
});
// Up to five, ;-separated, each language:layout in hex (0406:00000406), or
// an IME's language:{profile GUID}{TIP GUID}.
checked_string!(InputLocale, "keyboard layouts like 0406:00000406, separated by ;", |s| {
    let hex = |p: &str, n: usize| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit());
    let guid = |p: &str| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')).and_then(crate::gpt::Guid::parse).is_some();
    let ime = |p: &str| p.len() == 76 && p.find("}{").is_some_and(|i| guid(&p[..=i]) && guid(&p[i + 1..]));
    let one = |l: &str| l.split_once(':').is_some_and(|(lang, layout)| hex(lang, 4) && (hex(layout, 8) || ime(layout)));
    (1..=5).contains(&s.split(';').count()) && s.split(';').all(one)
});
// Windows' IDs are names like "W. Europe Standard Time" and never have a /;
// an IANA name (Europe/Copenhagen) almost always does. The image lists the
// real ones, which iso.rs checks against.
checked_string!(TimeZone, "a Windows time zone ID (tzutil /l), like \"W. Europe Standard Time\", not an IANA name like Europe/Copenhagen", |s| {
    (1..=128).contains(&s.len()) && s.trim() == s && !s.contains(['/', '\\']) && !s.chars().any(char::is_control) && s.is_ascii()
});
// Absolute and plain, since it goes into a shell script.
checked_string!(AbsPath, "an absolute path of plain components", |s| {
    s == "/"
        || (s.starts_with('/')
            && !s.ends_with('/')
            && s[1..].split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))))
});
// Relative to the profile and plain, so it can't reach outside it; `/`
// separated, as everywhere else in the config.
checked_string!(ProfilePath, "a path under the user's profile (plain components separated by /)", |s| {
    s.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
});
checked_string!(GithubRepo, "a GitHub repo (owner/name)", |s| {
    let ok = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    s.split_once('/').is_some_and(|(o, n)| ok(o) && ok(n))
});
// What goes into the catalog's search box; printable, since it's sent as a
// query parameter and nothing else.
checked_string!(CatalogQuery, "an Update Catalog search (a hardware ID like VEN_1002&DEV_747E, or an update's title)", |s| {
    !s.trim().is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)
});
checked_string!(AssocId, "a file extension (.html) or URL scheme (https)", |s| {
    let body = s.strip_prefix('.').unwrap_or(s);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_alphanumeric() || b"-+.".contains(&b))
});
checked_string!(ProgId, "a ProgId (letters, digits, - _ .)", |s| {
    (1..=255).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
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
        if self.cosmic.compositor.input_default.is_some() && self.display_scale.is_none() {
            bail!("config: cosmic.compositor.input_default: Windows' pointer speed depends on its display scale, so set displayScale (the Linux output's scale, 1.0 for 100 %)");
        }
        if let Some(linux) = &self.wsl.linux {
            if linux.mounts.first().is_none_or(|m| m.target.as_str() != "/") {
                bail!("config: wsl.linux.mounts: the first mount must be \"/\"");
            }
            if linux.disks.is_empty() {
                bail!("config: wsl.linux.disks: name at least the disk \"/\" is on");
            }
            // Windows paths ignore case. One inside another (or the same one
            // twice) would be overwritten or deleted by it, depending on the
            // order they're copied in. Sorted, a path's descendants come
            // right after it.
            let mut dests: Vec<String> = linux.files.keys().map(|d| d.as_str().to_ascii_lowercase()).collect();
            dests.sort();
            for pair in dests.windows(2) {
                if pair[1] == pair[0] || pair[1].starts_with(&format!("{}/", pair[0])) {
                    bail!("config: wsl.linux.files: {:?} and {:?} overlap (Windows ignores case)", pair[0], pair[1]);
                }
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

    #[test]
    fn display_modes_are_checked_and_render_as_the_shell_reads_them() {
        let parse = |body: &str| toml::from_str::<DisplayMode>(body);
        let m = parse("model = \"49M2C8900\"\nwidth = 5120\nheight = 1440\nrefresh = 240.49").unwrap();
        // As shell.json has it.
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::json!({ "model": "49M2C8900", "serial": null, "width": 5120, "height": 1440, "refresh": 240.49 })
        );
        assert!(parse("model = \"49M2C8900\"\nwidth = 0\nheight = 1440\nrefresh = 60").is_err());
        assert!(parse("model = \"49M2C8900\"\nwidth = 5120\nheight = 1440\nrefresh = -1").is_err());
        assert!(parse("model = \"a model name too long\"\nwidth = 5120\nheight = 1440\nrefresh = 60").is_err());
        assert!(parse("model = \" padded\"\nwidth = 5120\nheight = 1440\nrefresh = 60").is_err());
    }

    use super::*;

    #[test]
    fn default_apps_keys_and_prog_ids() {
        let ok: BTreeMap<AssocId, ProgId> = toml::from_str("https = \"FirefoxURL-308046B0AF4A39CB\"\n\".html\" = \"FirefoxHTML-308046B0AF4A39CB\"").unwrap();
        assert_eq!(ok.len(), 2);
        for bad in ["\"https:\" = \"X\"", "https = \"a b\"", "\".\" = \"X\"", "https = \"\""] {
            assert!(toml::from_str::<BTreeMap<AssocId, ProgId>>(bad).is_err(), "{bad} parsed");
        }
    }

    #[test]
    fn defaults_parse() {
        let mut t: toml::Table = toml::from_str(DEFAULTS).unwrap();
        t.insert("user".into(), toml::Value::Table(toml::from_str("name = \"x\"").unwrap()));
        let config: Config = toml::Value::Table(t).try_into().unwrap();
        assert!(matches!(config.shell, Shell::Builtin));
        assert_eq!(config.edition, Edition::Pro);
        assert!(config.gpu.windows_update);
    }

    /// winget has no GPU driver to offer (README, "GPU drivers"), so the
    /// per-vendor winget lists a config may still set fail rather than do
    /// nothing.
    #[test]
    fn gpu_vendor_lists_are_gone() {
        let mut t: toml::Table = toml::from_str(DEFAULTS).unwrap();
        merge(&mut t, toml::from_str("[user]\nname = \"x\"\n[gpu]\nnvidia = []").unwrap());
        let err = deserialize::<Config>(t).expect_err("took gpu.nvidia");
        assert!(err.to_string().contains("gpu"), "{err}");
    }

    #[test]
    fn linux_files_stay_in_the_profile_and_dont_overlap() {
        let config = |files: &str| -> Result<Config> {
            let mut t: toml::Table = toml::from_str(DEFAULTS).unwrap();
            let linux = format!("[user]\nname = \"x\"\n[wsl.linux]\nuser = \"x\"\ndisks = [\"fba1d9d1-1150-4352-9ef6-83f00f0f27e8\"]\nmounts = [{{ device = \"LABEL=root\", target = \"/\", fsType = \"btrfs\" }}]\n[wsl.linux.files]\n{files}");
            merge(&mut t, toml::from_str(&linux).unwrap());
            let config: Config = toml::Value::Table(t).try_into()?;
            config.check()?;
            Ok(config)
        };
        let ok = config("\".claude/CLAUDE.md\" = \"/home/x/.claude/CLAUDE.md\"\n\".claude/skills\" = \"/home/x/.claude/skills\"").unwrap();
        assert_eq!(ok.wsl.linux.unwrap().files.len(), 2);
        for bad in [
            "\"../escape\" = \"/a\"",
            "\"/abs\" = \"/a\"",
            "\"C:/abs\" = \"/a\"",
            "\"a\\\\b\" = \"/a\"",
            "\"a//b\" = \"/a\"",
            "\"a\" = \"relative\"",
            "\".claude\" = \"/a\"\n\".claude/x\" = \"/b\"",
            "\".claude\" = \"/a\"\n\".Claude/x\" = \"/b\"",
            "\"key\" = \"/a\"\n\"KEY\" = \"/b\"",
        ] {
            assert!(config(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn persist_forms() {
        let parse = |s: &str| -> Result<Persist> { Ok(toml::from_str::<Persist>(s)?) };
        assert!(parse("").unwrap().firefox.is_none());
        assert!(parse("[firefox]").unwrap().firefox.unwrap().seed.is_none());
        let seed = parse("[firefox]\nseed = { profile = \"/home/x/.config/zen/default\", history = true }").unwrap().firefox.unwrap().seed.unwrap();
        assert_eq!(seed.profile.as_str(), "/home/x/.config/zen/default");
        assert!(seed.history);
        assert!(!parse("[firefox]\nseed = { profile = \"/a\" }").unwrap().firefox.unwrap().seed.unwrap().history);
        for bad in [
            "firefox = true",
            "[firefox]\nseed = { profile = \"relative/zen\" }",
            "[firefox]\nseed = { profile = \"/home/x/../y\" }",
            "[firefox]\nseed = {}",
            "[firefox]\npaths = [\"Profiles\"]",
            "[discord]",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn linux_boot_entry_forms() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct T {
            linux_boot_entry: LinuxBootEntry,
        }
        let parse = |s: &str| toml::from_str::<T>(s).map(|t| t.linux_boot_entry);
        assert_eq!(parse("linuxBootEntry = \"auto\"").unwrap(), LinuxBootEntry::Auto);
        assert_eq!(parse("linuxBootEntry = \"none\"").unwrap(), LinuxBootEntry::None);
        assert_eq!(
            parse("linuxBootEntry = { description = \"Linux Boot Manager\" }").unwrap(),
            LinuxBootEntry::Description(BootEntryDescription::try_from("Linux Boot Manager".to_owned()).unwrap())
        );
        assert!(parse("linuxBootEntry = { description = \"\" }").is_err());
        assert!(parse("linuxBootEntry = \"Linux Boot Manager\"").is_err());
    }

    #[test]
    fn file_sources_say_what_is_wrong() {
        let parse = |s: &str| toml::from_str::<FileSource>(s).map_err(|e| e.to_string());
        let hash = "hash = \"sha256-h3aIe3mSWrvdQH/N/iV9g4K1TiS+UqZGZCa5WoBNZ5A=\"";
        assert!(matches!(parse(&format!("url = \"https://x/y\"\n{hash}\nmember = \"a.ttf\"")).unwrap(), FileSource { origin: Origin::Fetched { .. }, member: Some(_) }));
        assert!(matches!(parse("path = \"/a\"").unwrap().origin, Origin::Local(_)));
        assert!(matches!(parse("track = { github = \"a/b\", asset = \"*.msi\" }").unwrap().origin, Origin::Tracked(_)));
        for (bad, says) in [
            ("url = \"https://x/y\"", "needs its hash"),
            (hash, "without the url"),
            ("path = \"/a\"\nurl = \"https://x/y\"", "no url, hash or track"),
            ("member = \"a\"", "a url and hash, a track, or a path"),
            ("pth = \"/a\"", "unknown field"),
        ] {
            let err = parse(bad).unwrap_err();
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn editions_by_their_wim_names() {
        let parse = |s: &str| Edition::try_from(s.to_owned());
        assert_eq!(parse("Windows 11 Pro for Workstations").unwrap(), Edition::ProForWorkstations);
        for e in Edition::ALL {
            assert_eq!(parse(e.name()).unwrap(), e);
        }
        assert!(parse("Windows 11 Enterprise").unwrap_err().contains("\"Windows 11 Home\""));
    }

    #[test]
    fn locales_and_time_zones_are_shaped_right() {
        for ok in ["en-US", "da-DK", "sr-Latn-RS"] {
            assert!(Language::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["en", "english", "en_US", "en-US-", "0409"] {
            assert!(Language::try_from(bad.to_owned()).is_err(), "{bad}");
        }
        for ok in ["0409:00000409", "0406:00000406;0409:00000409", "0411:{03B5835F-F03C-411B-9CE2-AA23E1171E36}{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}"] {
            assert!(InputLocale::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["", "da-DK", "0406", "0406:406", "0406:00000406;", "0411:{03B5835F-F03C-411B-9CE2-AA23E1171E36}"] {
            assert!(InputLocale::try_from(bad.to_owned()).is_err(), "{bad}");
        }
        for ok in ["UTC", "W. Europe Standard Time", "Romance Standard Time"] {
            assert!(TimeZone::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["Europe/Copenhagen", "", " UTC"] {
            assert!(TimeZone::try_from(bad.to_owned()).is_err(), "{bad}");
        }
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
