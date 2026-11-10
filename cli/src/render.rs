//! Everything generated from the config that goes onto the install media,
//! written to a directory: what `dfnstrate render` shows and `build iso`
//! puts in the image.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cache::Cache;
use crate::config::{Config, Hive, RegValue, Shell};
use crate::registry::{self, Root};
use crate::unattend::{self, Variant};
use crate::wsl;

/// The shell's file name under C:\Program Files\defenestrate.
pub const BUILTIN_SHELL: &str = "defenestrate-shell.exe";

/// One .reg file: its name (without .reg) and the hive its keys are in.
pub struct RegFile {
    pub name: &'static str,
    pub root: Root,
}

pub const REG_FILES: [RegFile; 4] = [
    RegFile { name: "software", root: Root::Software },
    RegFile { name: "system", root: Root::System },
    RegFile { name: "defaultUser", root: Root::DefaultUser },
    // Applied by FirstLogonCommands instead of specialize: OOBE runs its own
    // session as defaultuser0, and that session needs explorer. With the
    // shell switched at specialize, OOBE died with "Why did my PC restart?".
    RegFile { name: "shell", root: Root::Software },
];

/// Where setup finds our registry files and scripts, relative to C:.
pub const SETUP_DIR: &str = "Windows/Setup/defenestrate";

pub struct Rendered {
    pub dir: PathBuf,
    /// Everything that goes onto C:. `build iso` adds these to the image and
    /// `update` rewrites them on an installed one, so the two can't disagree
    /// about what lives where.
    pub files: Vec<InstallFile>,
}

/// A file the build puts on C:.
#[derive(Debug)]
pub struct InstallFile {
    /// Path on C:, `/`-separated, without the drive or a leading slash.
    pub dest: String,
    pub source: PathBuf,
    pub read: ReadAt,
}

/// When Windows reads a file of ours, which decides whether replacing it on
/// an installed system does anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadAt {
    /// Only during setup or first logon; a newer copy is never looked at.
    Setup,
    /// At every boot or logon.
    EveryLogon,
}

impl InstallFile {
    pub fn new(dest: impl Into<String>, source: impl Into<PathBuf>, read: ReadAt) -> Self {
        Self { dest: dest.into(), source: source.into(), read }
    }
}

/// Writes into `dir`:
/// - reg/<name>.reg (UTF-16, for reg.exe) and reg/ascii/<name>.reg (for hivex)
/// - autounattend.xml
/// - appx-remove.txt, specialize.cmd, remove-appx.ps1
/// - wsl/ (see wsl.rs)
pub fn render(config: &Config, variant: &Variant, cache: &Cache, work: &Path, dir: &Path) -> Result<Rendered> {
    std::fs::create_dir_all(dir.join("reg/ascii"))?;

    let mut files = Vec::new();
    let mut fonts = Vec::new();
    for source in &config.fonts {
        let name = source.file_name()?;
        files.push(InstallFile::new(format!("Windows/Fonts/{name}"), cache.materialize(source, work)?, ReadAt::EveryLogon));
        fonts.push(name);
    }

    // Winlogon starts the shell at each logon, so a newer exe takes effect.
    let mut install_shell = |name: String, path: PathBuf| {
        files.push(InstallFile::new(format!("Program Files/defenestrate/{name}"), path, ReadAt::EveryLogon));
        program_files(&name)
    };
    let shell_command = match &config.shell {
        Shell::Command(c) => c.clone(),
        Shell::Builtin => install_shell(BUILTIN_SHELL.to_owned(), builtin_shell(work)?),
        Shell::Exe(source) => install_shell(source.file_name()?, cache.materialize(source, work)?),
    };

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
    let mut shell = Hive::new();
    set(&mut shell, "Microsoft\\Windows NT\\CurrentVersion\\Winlogon", "Shell", RegValue::Sz(shell_command));

    for f in &REG_FILES {
        let keys = match f.name {
            "software" => &software,
            "system" => &config.registry.system,
            "defaultUser" => &config.registry.default_user,
            _ => &shell,
        };
        let text = registry::render(f.root, keys);
        std::fs::write(dir.join(format!("reg/ascii/{}.reg", f.name)), &text)?;
        let reg = dir.join(format!("reg/{}.reg", f.name));
        std::fs::write(&reg, registry::utf16(&text))?;
        files.push(InstallFile::new(format!("{SETUP_DIR}/{}.reg", f.name), reg, ReadAt::Setup));
    }

    let appx: String = config.appx.remove.iter().map(|n| format!("{n}\r\n")).collect();
    let setup_files = [
        ("appx-remove.txt", appx),
        ("specialize.cmd", crlf(include_str!("../assets/specialize.cmd"))),
        ("remove-appx.ps1", crlf(include_str!("../assets/remove-appx.ps1"))),
    ];
    for (name, data) in setup_files {
        std::fs::write(dir.join(name), data)?;
        files.push(InstallFile::new(format!("{SETUP_DIR}/{name}"), dir.join(name), ReadAt::Setup));
    }

    files.extend(wsl::render(config, cache, work, dir)?);
    files.extend(crate::apps::render(config, dir)?);
    std::fs::write(dir.join("autounattend.xml"), unattend::render(config, variant)?)?;

    Ok(Rendered { dir: dir.to_path_buf(), files })
}

/// cmd.exe misparses scripts with bare LF line endings.
pub fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Shell is a plain REG_SZ, so no %ProgramFiles%; setup always installs to C:.
fn program_files(exe: &str) -> String {
    format!("\"C:\\Program Files\\defenestrate\\{exe}\"")
}

fn set(hive: &mut Hive, key: &str, name: &str, value: RegValue) {
    hive.entry(key.to_owned()).or_default().insert(name.to_owned(), value);
}

/// The shell exe compiled into this binary (see build.rs).
fn builtin_shell(work: &Path) -> Result<PathBuf> {
    #[cfg(dfnstrate_builtin_shell)]
    {
        static EXE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/defenestrate-shell.exe"));
        let path = work.join(BUILTIN_SHELL);
        std::fs::write(&path, EXE)?;
        Ok(path)
    }
    #[cfg(not(dfnstrate_builtin_shell))]
    {
        let _ = work;
        Err(anyhow::anyhow!(
            "this dfnstrate was built without the shell (set DEFENESTRATE_SHELL_EXE when building it, or use the Nix package). \
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
        let mut got: Vec<(&str, ReadAt)> = r.files.iter().map(|f| (f.dest.as_str(), f.read)).collect();
        got.sort();
        let s = "Windows/Setup/defenestrate";
        let mut want = vec![
            ("Windows/Fonts/font.ttf".to_owned(), ReadAt::EveryLogon),
            ("Program Files/defenestrate/my-shell.exe".to_owned(), ReadAt::EveryLogon),
            (format!("{s}/wsl/rootfs.tar"), ReadAt::EveryLogon),
            (format!("{s}/wsl/wsl-logon.ps1"), ReadAt::EveryLogon),
            (format!("{s}/wsl/wsl.json"), ReadAt::EveryLogon),
            (format!("{s}/wsl/task.xml"), ReadAt::Setup),
            (format!("{s}/wsl/wsl.msi"), ReadAt::Setup),
            (format!("{s}/wsl/wsl-install.cmd"), ReadAt::Setup),
            (format!("{s}/software.reg"), ReadAt::Setup),
            (format!("{s}/system.reg"), ReadAt::Setup),
            (format!("{s}/defaultUser.reg"), ReadAt::Setup),
            (format!("{s}/shell.reg"), ReadAt::Setup),
            (format!("{s}/appx-remove.txt"), ReadAt::Setup),
            (format!("{s}/specialize.cmd"), ReadAt::Setup),
            (format!("{s}/remove-appx.ps1"), ReadAt::Setup),
        ];
        want.sort();
        assert_eq!(got, want.iter().map(|(p, r)| (p.as_str(), *r)).collect::<Vec<_>>());
        for f in &r.files {
            assert!(f.source.is_file(), "{} has no source", f.dest);
        }
        assert_eq!(std::fs::read(&r.files.iter().find(|f| f.dest.ends_with("wsl.msi")).unwrap().source).unwrap(), b"package");
        let shell_reg = std::fs::read_to_string(d.join("out/reg/ascii/shell.reg")).unwrap();
        assert!(shell_reg.contains(r#""Shell"="\"C:\\Program Files\\defenestrate\\my-shell.exe\"""#), "{shell_reg}");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
