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

pub struct Rendered {
    pub dir: PathBuf,
    /// Font files and the names they get in C:\Windows\Fonts.
    pub fonts: Vec<(PathBuf, String)>,
    /// The shell exe, when it's ours to install.
    pub shell_exe: Option<PathBuf>,
    pub wsl: wsl::Payload,
}

/// Writes into `dir`:
/// - reg/<name>.reg (UTF-16, for reg.exe) and reg/ascii/<name>.reg (for hivex)
/// - autounattend.xml
/// - appx-remove.txt
/// - wsl/ (see wsl.rs)
pub fn render(config: &Config, variant: &Variant, cache: &Cache, work: &Path, dir: &Path) -> Result<Rendered> {
    std::fs::create_dir_all(dir.join("reg/ascii"))?;

    let mut fonts = Vec::new();
    for source in &config.fonts {
        let path = cache.materialize(source, work)?;
        fonts.push((path, source.file_name()?));
    }

    let (shell_command, shell_exe) = match &config.shell {
        Shell::Command(c) => (c.clone(), None),
        Shell::Builtin => (program_files(BUILTIN_SHELL), Some(builtin_shell(work)?)),
        Shell::Exe(source) => (program_files(&source.file_name()?), Some(cache.materialize(source, work)?)),
    };

    let mut software = config.registry.software.clone();
    // 22H2+ OOBE stops at "connect to a network" without this. The
    // interactive escapes (bypassnro.cmd, ms-cxh:localonly) are gone since
    // 24H2; the registry value still works.
    set(&mut software, "Microsoft\\Windows\\CurrentVersion\\OOBE", "BypassNRO", RegValue::Dword(1));
    // A file in C:\Windows\Fonts only exists to GDI once it's listed here.
    // The value name is just a label; the data is the file name.
    for (_, name) in &fonts {
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
        std::fs::write(dir.join(format!("reg/{}.reg", f.name)), registry::utf16(&text))?;
    }

    let appx: String = config.appx.remove.iter().map(|n| format!("{n}\r\n")).collect();
    std::fs::write(dir.join("appx-remove.txt"), appx)?;

    let wsl = wsl::render(config, cache, work, dir)?;
    std::fs::write(dir.join("autounattend.xml"), unattend::render(config, variant)?)?;

    Ok(Rendered { dir: dir.to_path_buf(), fonts, shell_exe, wsl })
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
