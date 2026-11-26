//! Everything generated from the config that goes onto the install media,
//! written to a directory: what `dfnstrate render` shows and `build iso`
//! puts in the image.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::cert::ShellCert;
use crate::config::{Config, Hive, RegValue, Shell};
use crate::registry::{self, Root};
use crate::unattend::{self, FirstLogon, Variant};
use crate::{boot, wsl};

/// The builtin shell's two files under C:\Program Files\defenestrate: the
/// launcher Winlogon starts, and the signed uiAccess binary it starts in
/// turn (shell/src/handoff.rs).
pub const BUILTIN_SHELL: &str = "defenestrate-shell.exe";
pub const BUILTIN_SHELL_UIACCESS: &str = "defenestrate-shell-uiaccess.exe";
const SHELL_DIR: CPath = CPath::new("Program Files/defenestrate");
/// The shell's settings (`cosmic`), next to its exe, where only admins can
/// change them.
pub const SHELL_SETTINGS: &str = "shell.json";

/// One .reg file: its name (without .reg), the hive its keys are in, and
/// where they come from.
pub struct RegFile {
    pub name: &'static str,
    pub root: Root,
    keys: RegKeys,
}

enum RegKeys {
    /// The config's registry.software, plus what the build needs there.
    Software,
    System,
    DefaultUser,
    /// The Shell value alone.
    Shell,
}

pub const REG_FILES: [RegFile; 4] = [
    RegFile { name: "software", root: Root::Software, keys: RegKeys::Software },
    RegFile { name: "system", root: Root::System, keys: RegKeys::System },
    RegFile { name: "defaultUser", root: Root::DefaultUser, keys: RegKeys::DefaultUser },
    // Applied by FirstLogonCommands instead of specialize: OOBE runs its own
    // session as defaultuser0, and that session needs explorer. With the
    // shell switched at specialize, OOBE died with "Why did my PC restart?".
    RegFile { name: "shell", root: Root::Software, keys: RegKeys::Shell },
];

/// What one part of the payload (wsl, apps, reboot) puts on C:, and what
/// first logon runs to set it up. Both come from the same decision, so a
/// part that's off has neither.
#[derive(Default)]
pub struct Payload {
    pub files: Vec<InstallFile>,
    pub first_logon: Vec<FirstLogon>,
}

/// Where setup finds our registry files and scripts.
pub const SETUP_DIR: CPath = CPath::new("Windows/Setup/defenestrate");

/// A path on the install's C:, `/`-separated, with no drive or leading
/// slash: the form that joins onto a mounted volume or a WIM's root. Only
/// [`CPath::windows`] spells it the way Windows reads it, so the two forms
/// can't get mixed up in a format string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CPath(Cow<'static, str>);

impl CPath {
    pub const fn new(path: &'static str) -> Self {
        assert!(Self::plain(path), "a C: path is / separated, without a drive or leading slash");
        Self(Cow::Borrowed(path))
    }

    const fn plain(path: &str) -> bool {
        let b = path.as_bytes();
        if b.is_empty() || b[0] == b'/' || b[b.len() - 1] == b'/' {
            return false;
        }
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' || b[i] == b':' {
                return false;
            }
            i += 1;
        }
        true
    }

    /// `rest` is one or more `/`-separated components: our own names, or a
    /// file name config::file_name() already checked.
    pub fn join(&self, rest: &str) -> Self {
        assert!(Self::plain(rest), "{rest:?} isn't a relative C: path");
        Self(Cow::Owned(format!("{}/{rest}", self.0)))
    }

    /// Relative to the volume's root.
    pub fn rel(&self) -> &str {
        &self.0
    }

    /// As Windows names it: C:\Windows\Setup\defenestrate.
    pub fn windows(&self) -> String {
        format!("C:\\{}", self.0.replace('/', "\\"))
    }

    /// Under C:\Windows, as %SystemRoot%\…: the answer file's commands name
    /// our files that way.
    pub fn system_root(&self) -> Option<String> {
        self.0.strip_prefix("Windows/").map(|rest| format!("%SystemRoot%\\{}", rest.replace('/', "\\")))
    }
}

pub struct Rendered {
    pub dir: PathBuf,
    /// Everything that goes onto C:. `build iso` adds these to the image and
    /// `update` rewrites them on an installed one, so the two can't disagree
    /// about what lives where.
    pub files: Vec<InstallFile>,
    /// What the builtin shell's uiAccess binary is signed with, trusted in
    /// the Root store so it gets uiAccess (cert.rs). None for any other shell.
    pub shell_cert: Option<ShellCert>,
    /// The registry config as boot queue actions (boot.rs): `build iso`
    /// puts them in the image, `update` queues the changed ones.
    pub boot: Vec<boot::Action>,
    /// `boot` as a fresh queue, machine/ and user/, for the image.
    pub queue: PathBuf,
}

/// A file the build puts on C:.
#[derive(Debug)]
pub struct InstallFile {
    pub dest: CPath,
    pub source: PathBuf,
    pub read: ReadAt,
    pub if_missing: IfMissing,
}

/// What `update` does about a file the install doesn't have yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfMissing {
    /// Nothing: only setup can add it, since it needs registering too (a
    /// font, a task, the Shell value).
    Reflash,
    /// Create it, with the NTFS security descriptor the file at this dest
    /// has, which setup made: for a file that needs nothing but to exist.
    CreateLike(CPath),
    /// Create it, and any missing parents, with what its parent directory
    /// hands down (ntfs::Mount::inheriting): for a file whatever reads it
    /// registers itself (the boot queue's runner and its tasks).
    Inherit,
}

/// When Windows reads a file of ours, which decides whether replacing it on
/// an installed system does anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadAt {
    /// Only during setup or first logon; a newer copy is never looked at.
    Setup,
    /// At every boot or logon.
    EveryLogon,
    /// Only by setup, but what it does reaches an installed system through
    /// the boot queue instead (the registry .reg files, boot.rs).
    Queued,
}

impl InstallFile {
    pub fn new(dest: CPath, source: impl Into<PathBuf>, read: ReadAt) -> Self {
        Self { dest, source: source.into(), read, if_missing: IfMissing::Reflash }
    }
}

/// The builtin shell as dfnstrate carries it: the launcher and the uiAccess
/// binary, which only work together, the latter signed by one of our
/// certificates (shell/sign.sh).
pub struct BuiltinShell {
    launcher: PathBuf,
    uiaccess: PathBuf,
    cert: ShellCert,
}

impl BuiltinShell {
    pub fn new(launcher: PathBuf, uiaccess: PathBuf) -> Result<Self> {
        let exe = std::fs::read(&uiaccess)?;
        let Some(cert) = ShellCert::from_exe(&exe).with_context(|| uiaccess.display().to_string())? else {
            bail!(
                "{} isn't signed by shell/sign.sh, so Windows would never give it uiAccess; build the shell through sign.sh (README)",
                uiaccess.display()
            );
        };
        Ok(Self { launcher, uiaccess, cert })
    }

    /// Its install files and the Shell value that starts it. Winlogon starts
    /// the shell at each logon, so newer exes take effect; Program Files is
    /// also one of the locations uiAccess requires.
    fn install(self, files: &mut Vec<InstallFile>) -> (String, ShellCert) {
        let launcher = SHELL_DIR.join(BUILTIN_SHELL);
        files.push(InstallFile::new(launcher.clone(), self.launcher, ReadAt::EveryLogon));
        // An install from before the uiAccess binary existed has the launcher
        // (then the whole shell) but not this; `update` can add it because
        // nothing needs registering, only the descriptor setup gives files
        // here, which it copies from the launcher's.
        let mut uiaccess = InstallFile::new(SHELL_DIR.join(BUILTIN_SHELL_UIACCESS), self.uiaccess, ReadAt::EveryLogon);
        uiaccess.if_missing = IfMissing::CreateLike(launcher);
        files.push(uiaccess);
        (program_files(BUILTIN_SHELL), self.cert)
    }
}

/// Writes into `dir`:
/// - reg/<name>.reg (UTF-16, for reg.exe) and reg/ascii/<name>.reg (for hivex)
/// - autounattend.xml
/// - appx-remove.txt, specialize.cmd, remove-appx.ps1
/// - wsl/ (see wsl.rs), apps/ (apps.rs), reboot/ (reboot_to_linux.rs)
pub fn render(config: &Config, variant: &Variant, cache: &Cache, work: &Path, dir: &Path) -> Result<Rendered> {
    std::fs::create_dir_all(dir.join("reg/ascii"))?;

    let mut files = Vec::new();
    let mut fonts = Vec::new();
    for source in &config.fonts {
        let font = cache.materialize(source, work)?;
        files.push(InstallFile::new(CPath::new("Windows/Fonts").join(&font.name), font.path, ReadAt::EveryLogon));
        fonts.push(font.name);
    }

    let (shell_command, shell_cert) = match &config.shell {
        Shell::Command(c) => (c.clone(), None),
        Shell::Builtin => {
            let (command, cert) = builtin_shell(work)?.install(&mut files);
            (command, Some(cert))
        }
        // Someone else's shell, or ours as a single exe: no uiAccess.
        Shell::Exe(source) => {
            let exe = cache.materialize(source, work)?;
            files.push(InstallFile::new(SHELL_DIR.join(&exe.name), exe.path, ReadAt::EveryLogon));
            (program_files(&exe.name), None)
        }
    };
    // Read at each start of the shell, like the exe. Only ours reads it, but
    // an `exe` shell is usually ours, built some other way.
    if !matches!(config.shell, Shell::Command(_)) {
        std::fs::write(dir.join(SHELL_SETTINGS), config.cosmic.render(&config.displays, &config.autostart, &config.default_apps))?;
        files.push(InstallFile::new(SHELL_DIR.join(SHELL_SETTINGS), dir.join(SHELL_SETTINGS), ReadAt::EveryLogon));
    }

    let mut software = config.registry.software.clone();
    // 22H2+ OOBE stops at "connect to a network" without this. The
    // interactive escapes (bypassnro.cmd, ms-cxh:localonly) are gone since
    // 24H2; the registry value still works.
    set(&mut software, "Microsoft\\Windows\\CurrentVersion\\OOBE", "BypassNRO", RegValue::Dword(1));
    // A file in C:\Windows\Fonts only exists to GDI once it's listed here.
    // The value name is just a label; the data is the file name.
    for name in &fonts {
        let label = name.strip_suffix(".ttf").or_else(|| name.strip_suffix(".otf")).unwrap_or(name);
        set(&mut software, "Microsoft\\Windows NT\\CurrentVersion\\Fonts", &format!("{label} (TrueType)"), RegValue::Sz(name.clone()));
    }
    if let Some(cert) = &shell_cert {
        set(&mut software, &cert.root_key(), "Blob", RegValue::Binary(cert.blob()));
    }
    let mut default_user = config.registry.default_user.clone();
    let c = &config.cosmic.compositor;
    // Over the distro's own Control Panel\Mouse values (acceleration off).
    let input = [
        config.display_scale.map(|s| s.registry()),
        c.input_default.map(|i| crate::input::pointer(&i, config.display_scale.expect("checked with the config"))).transpose()?,
        c.xkb_config.map(|r| crate::input::keyboard(&r)),
    ];
    for (key, name, value) in input.into_iter().flatten().flatten() {
        set(&mut default_user, key, name, value);
    }
    let mut shell = Hive::new();
    set(&mut shell, "Microsoft\\Windows NT\\CurrentVersion\\Winlogon", "Shell", RegValue::Sz(shell_command));

    for f in &REG_FILES {
        let keys = match f.keys {
            RegKeys::Software => &software,
            RegKeys::System => &config.registry.system,
            RegKeys::DefaultUser => &default_user,
            RegKeys::Shell => &shell,
        };
        let text = registry::render(f.root, keys);
        std::fs::write(dir.join(format!("reg/ascii/{}.reg", f.name)), &text)?;
        let reg = dir.join(format!("reg/{}.reg", f.name));
        std::fs::write(&reg, registry::utf16(&text))?;
        files.push(InstallFile::new(SETUP_DIR.join(&format!("{}.reg", f.name)), reg, ReadAt::Queued));
    }
    // The same keys again, for the boot queue: a fresh install's first boot
    // imports them once more (changing nothing), which runs the queue on
    // every install the way an `update` will.
    let boot = boot::registry_actions(&software, &config.registry.system, &default_user, &shell);
    let queue = dir.join("boot-queue");
    if queue.exists() {
        std::fs::remove_dir_all(&queue)?;
    }
    boot::initial(&queue, &boot)?;

    let appx: String = config.appx.remove.iter().map(|n| format!("{n}\r\n")).collect();
    let setup_files = [
        ("appx-remove.txt", appx),
        ("specialize.cmd", crlf(include_str!("../assets/specialize.cmd"))),
        ("remove-appx.ps1", crlf(include_str!("../assets/remove-appx.ps1"))),
    ];
    for (name, data) in setup_files {
        std::fs::write(dir.join(name), data)?;
        files.push(InstallFile::new(SETUP_DIR.join(name), dir.join(name), ReadAt::Setup));
    }

    let mut first_logon = Vec::new();
    for part in
        [boot::render(config, dir)?, wsl::render(config, cache, work, dir)?, crate::apps::render(config, dir)?, crate::reboot_to_linux::render(config, dir)?]
    {
        files.extend(part.files);
        first_logon.extend(part.first_logon);
    }
    std::fs::write(dir.join("autounattend.xml"), unattend::render(config, variant, &first_logon)?)?;

    Ok(Rendered { dir: dir.to_path_buf(), files, shell_cert, boot, queue })
}

/// cmd.exe misparses scripts with bare LF line endings.
pub fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Shell is a plain REG_SZ, so no %ProgramFiles%; setup always installs to C:.
fn program_files(exe: &str) -> String {
    format!("\"{}\"", SHELL_DIR.join(exe).windows())
}

fn set(hive: &mut Hive, key: &str, name: &str, value: RegValue) {
    hive.entry(key.to_owned()).or_default().insert(name.to_owned(), value);
}

/// The shell compiled into this binary (see build.rs).
fn builtin_shell(work: &Path) -> Result<BuiltinShell> {
    #[cfg(dfnstrate_builtin_shell)]
    {
        static LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/defenestrate-shell.exe"));
        static UIACCESS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/defenestrate-shell-uiaccess.exe"));
        let (launcher, uiaccess) = (work.join(BUILTIN_SHELL), work.join(BUILTIN_SHELL_UIACCESS));
        std::fs::write(&launcher, LAUNCHER)?;
        std::fs::write(&uiaccess, UIACCESS)?;
        BuiltinShell::new(launcher, uiaccess).context("the shell compiled into dfnstrate")
    }
    #[cfg(not(dfnstrate_builtin_shell))]
    {
        let _ = work;
        Err(anyhow::anyhow!(
            "this dfnstrate was built without the shell (set DEFENESTRATE_SHELL_DIR when building it, or use the Nix package). \
             Point the config at one instead: shell = {{ exe = {{ path = \"defenestrate-shell.exe\" }} }}"
        ))
        .context("shell = \"builtin\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sri;

    /// What `build iso` adds and `update` rewrites, and which of them an
    /// installed system reads again.
    #[test]
    fn install_files() {
        let d = std::env::temp_dir().join(format!("dfnstrate-test-render-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("cache")).unwrap();
        std::fs::write(d.join("font.ttf"), "font").unwrap();
        std::fs::write(d.join("my-shell.exe"), "exe").unwrap();

        let mut t: toml::Table = toml::from_str(crate::config::DEFAULTS).unwrap();
        for (_, key) in [("msi", "package"), ("busybox", "busybox")] {
            let hash = t["wsl"][key]["hash"].as_str().unwrap().to_owned();
            std::fs::write(d.join("cache").join(Sri::try_from(hash).unwrap().cache_key()), key).unwrap();
        }
        let machine = format!(
            r#"
            fonts = [{{ path = "{d}/font.ttf" }}]
            shell = {{ exe = {{ path = "{d}/my-shell.exe" }} }}
            [user]
            name = "u"
            "#,
            d = d.display()
        );
        for (k, v) in toml::from_str::<toml::Table>(&machine).unwrap() {
            t.insert(k, v);
        }
        let linux = r#"user = "u"
            disks = ["d3f3e5a7-5c4e-4b7a-9f1e-6a0d2c7b8e10"]
            mounts = [{ device = "LABEL=x", target = "/", fsType = "btrfs" }]"#;
        t["wsl"].as_table_mut().unwrap().insert("linux".into(), toml::Value::Table(toml::from_str(linux).unwrap()));
        let config: Config = toml::Value::Table(t).try_into().unwrap();
        let cache = Cache { dir: d.join("cache"), offline: true };

        let r = render(&config, &Variant::from_config(&config), &cache, &d.join("work"), &d.join("out")).unwrap();
        let mut got: Vec<(&str, ReadAt)> = r.files.iter().map(|f| (f.dest.rel(), f.read)).collect();
        got.sort();
        let s = "Windows/Setup/defenestrate";
        let mut want = vec![
            ("Windows/Fonts/font.ttf".to_owned(), ReadAt::EveryLogon),
            ("Program Files/defenestrate/my-shell.exe".to_owned(), ReadAt::EveryLogon),
            ("Program Files/defenestrate/shell.json".to_owned(), ReadAt::EveryLogon),
            (format!("{s}/wsl/rootfs.tar"), ReadAt::EveryLogon),
            (format!("{s}/wsl/wsl-logon.ps1"), ReadAt::EveryLogon),
            (format!("{s}/wsl/wsl.json"), ReadAt::EveryLogon),
            (format!("{s}/wsl/task.xml"), ReadAt::Setup),
            (format!("{s}/wsl/wsl.msi"), ReadAt::Setup),
            (format!("{s}/wsl/wsl-install.cmd"), ReadAt::Setup),
            (format!("{s}/software.reg"), ReadAt::Queued),
            (format!("{s}/system.reg"), ReadAt::Queued),
            (format!("{s}/defaultUser.reg"), ReadAt::Queued),
            (format!("{s}/shell.reg"), ReadAt::Queued),
            (format!("{s}/boot/boot.ps1"), ReadAt::EveryLogon),
            (format!("{s}/boot/machine.xml"), ReadAt::EveryLogon),
            (format!("{s}/boot/user.xml"), ReadAt::EveryLogon),
            (format!("{s}/appx-remove.txt"), ReadAt::Setup),
            (format!("{s}/specialize.cmd"), ReadAt::Setup),
            (format!("{s}/remove-appx.ps1"), ReadAt::Setup),
            // No apps, but the defaults' Windows Update pass for GPU drivers.
            (format!("{s}/apps/apps.json"), ReadAt::EveryLogon),
            (format!("{s}/apps/install-apps.ps1"), ReadAt::EveryLogon),
            (format!("{s}/apps/task.xml"), ReadAt::Setup),
            (format!("{s}/apps/limited-task.xml"), ReadAt::EveryLogon),
            (format!("{s}/reboot/reboot.json"), ReadAt::EveryLogon),
            (format!("{s}/reboot/linux-entry.ps1"), ReadAt::EveryLogon),
            (format!("{s}/reboot/reboot-to-linux.ps1"), ReadAt::EveryLogon),
            (format!("{s}/reboot/linux-first.ps1"), ReadAt::EveryLogon),
            (format!("{s}/reboot/register.ps1"), ReadAt::Setup),
            (format!("{s}/reboot/task.xml"), ReadAt::Setup),
            (format!("{s}/reboot/linux-first.xml"), ReadAt::Setup),
        ];
        want.sort();
        assert_eq!(got, want.iter().map(|(p, r)| (p.as_str(), *r)).collect::<Vec<_>>());
        for f in &r.files {
            assert!(f.source.is_file(), "{} has no source", f.dest.rel());
        }
        // install-apps.ps1 registers it, so an install from before it existed gets it from `update`.
        let limited = r.files.iter().find(|f| f.dest.rel() == format!("{s}/apps/limited-task.xml")).unwrap();
        assert_eq!(limited.if_missing, IfMissing::CreateLike(SETUP_DIR.join("apps/install-apps.ps1")));
        assert_eq!(std::fs::read(&r.files.iter().find(|f| f.dest.rel().ends_with("wsl.msi")).unwrap().source).unwrap(), b"package");
        let shell_reg = std::fs::read_to_string(d.join("out/reg/ascii/shell.reg")).unwrap();
        assert!(shell_reg.contains(r#""Shell"="\"C:\\Program Files\\defenestrate\\my-shell.exe\"""#), "{shell_reg}");
        // The shell parses the same file in its tests (shell/src/settings.rs).
        let settings = std::fs::read_to_string(d.join("out").join(SHELL_SETTINGS)).unwrap();
        assert_eq!(settings, include_str!("../../configs/shell-settings.json"), "configs/shell-settings.json is what the defaults render to");
        // Only the builtin shell's uiAccess binary gets a certificate trusted.
        assert!(r.shell_cert.is_none());
        let software = std::fs::read_to_string(d.join("out/reg/ascii/software.reg")).unwrap();
        assert!(!software.contains("SystemCertificates"), "{software}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The launcher and the signed uiAccess binary go in as a pair, and
    /// `update` may add the latter to an install that only has the former.
    #[test]
    fn builtin_shell_pair() {
        let d = std::env::temp_dir().join(format!("dfnstrate-test-builtin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (launcher, uiaccess, unsigned) = (d.join("l.exe"), d.join("u.exe"), d.join("unsigned.exe"));
        std::fs::write(&launcher, "launcher").unwrap();
        std::fs::write(&uiaccess, include_bytes!("../tests/fixtures/signed-headers.exe")).unwrap();
        std::fs::write(&unsigned, b"MZ not even a PE").unwrap();

        let err = BuiltinShell::new(launcher.clone(), launcher.clone()).err().expect("took an unsigned uiAccess binary");
        assert!(format!("{err:#}").contains("not a PE file"), "{err:#}");
        assert!(BuiltinShell::new(launcher.clone(), unsigned).is_err());

        let mut files = Vec::new();
        let (command, cert) = BuiltinShell::new(launcher.clone(), uiaccess.clone()).unwrap().install(&mut files);
        assert_eq!(command, r#""C:\Program Files\defenestrate\defenestrate-shell.exe""#);
        assert_eq!(cert.thumbprint(), include_str!("../tests/fixtures/signed-headers.thumbprint").trim());
        let got: Vec<_> = files.iter().map(|f| (f.dest.rel(), f.source.as_path(), f.read, &f.if_missing)).collect();
        let like = IfMissing::CreateLike(CPath::new("Program Files/defenestrate/defenestrate-shell.exe"));
        assert_eq!(
            got,
            [
                ("Program Files/defenestrate/defenestrate-shell.exe", launcher.as_path(), ReadAt::EveryLogon, &IfMissing::Reflash),
                ("Program Files/defenestrate/defenestrate-shell-uiaccess.exe", uiaccess.as_path(), ReadAt::EveryLogon, &like),
            ]
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
