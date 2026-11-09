//! Stock ISO + config → install ISO.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::config::Config;
use crate::render::{self, REG_FILES};
use crate::tools::{self, HIVEXREGEDIT, SEVENZIP, WIMLIB, XORRISO};
use crate::unattend::Variant;

pub const TOOLS: &[tools::Tool] = &[SEVENZIP, WIMLIB, HIVEXREGEDIT, XORRISO];

pub const ISO_NAME: &str = "defenestrate.iso";

/// Builds `out/defenestrate.iso`. `work` needs ~25 GB and is left for the
/// caller to clean.
pub fn build(config: &Config, variant: &Variant, source: &Path, cache: &Cache, work: &Path, out: &Path) -> Result<()> {
    tools::require(TOOLS)?;
    std::fs::create_dir_all(work)?;
    std::fs::create_dir_all(out)?;

    eprintln!("rendering the config");
    let r = render::render(config, variant, cache, work, &work.join("rendered"))?;

    // Microsoft's ISO is UDF-only (the ISO9660 side is just a README), which
    // rules out xorriso for reading it. -tudf because cached files have no
    // .iso suffix, and by content alone 7-Zip opens the ISO9660 side.
    eprintln!("unpacking {}", source.display());
    let iso = work.join("iso");
    let wim_dir = work.join("wim");
    tools::run(SEVENZIP.cmd().args(["x", "-tudf"]).arg(format!("-o{}", iso.display())).arg(source).arg("-x!sources/install.wim"))?;
    tools::run(SEVENZIP.cmd().args(["x", "-tudf"]).arg(format!("-o{}", wim_dir.display())).arg(source).arg("sources/install.wim"))?;
    if !wim_dir.join("sources/install.wim").is_file() {
        bail!("{} has no sources/install.wim; not a Windows install ISO?", source.display());
    }
    make_writable(&iso)?;

    // Keeping one edition drops ~0.5 GB per extra edition and pins image
    // index 1, which the answer file installs.
    eprintln!("exporting {}", config.edition.name());
    let install_wim = work.join("install.wim");
    tools::run(WIMLIB.cmd().arg("export").arg(wim_dir.join("sources/install.wim")).arg(config.edition.name()).arg(&install_wim))?;
    std::fs::remove_dir_all(&wim_dir)?;

    check_registry(&install_wim, &r.dir, work)?;
    check_appx(config, &install_wim)?;

    eprintln!("adding the payload");
    let payload = work.join("payload");
    std::fs::create_dir_all(&payload)?;
    for f in &REG_FILES {
        std::fs::copy(r.dir.join(format!("reg/{}.reg", f.name)), payload.join(format!("{}.reg", f.name)))?;
    }
    std::fs::copy(r.dir.join("appx-remove.txt"), payload.join("appx-remove.txt"))?;
    std::fs::write(payload.join("specialize.cmd"), crlf(include_str!("../assets/specialize.cmd")))?;
    std::fs::write(payload.join("remove-appx.ps1"), crlf(include_str!("../assets/remove-appx.ps1")))?;

    // Only adds new files. Replacing an existing file this way drops its
    // security descriptor, which is why the registry isn't edited offline.
    std::fs::write(r.wsl.dir.join("wsl-install.cmd"), crlf(include_str!("../assets/wsl-install.cmd")))?;
    let mut commands = vec![
        format!("add {} /Windows/Setup/defenestrate", quote(&payload)?),
        format!("add {} {}", quote(&r.wsl.dir)?, crate::wsl::DIR),
        format!("add {} {}/wsl.msi", quote(&r.wsl.msi)?, crate::wsl::DIR),
    ];
    if let Some(exe) = &r.shell_exe {
        let dir = work.join("program-files");
        std::fs::create_dir_all(&dir)?;
        let name = match &config.shell {
            crate::config::Shell::Exe(s) => s.file_name()?,
            _ => render::BUILTIN_SHELL.to_owned(),
        };
        std::fs::copy(exe, dir.join(name))?;
        commands.push(format!("add {} \"/Program Files/defenestrate\"", quote(&dir)?));
    }
    for (path, name) in &r.fonts {
        commands.push(format!("add {} \"/Windows/Fonts/{name}\"", quote(path)?));
    }
    tools::run_with_input(WIMLIB.cmd().arg("update").arg(&install_wim).arg("1"), &(commands.join("\n") + "\n"))?;

    // A single-edition install.wim is still ~7.5 GB, over ISO9660's 4 GiB
    // file limit. Setup takes split images natively, which keeps the ISO
    // plain ISO9660 and lets it be copied straight onto a FAT32 stick. (A
    // mkisofs UDF image held the whole file, and Windows setup stalled at 0%
    // without ever reading past its start.)
    eprintln!("splitting install.wim");
    tools::run(WIMLIB.cmd().arg("split").arg(&install_wim).arg(iso.join("sources/install.swm")).arg("3800"))?;
    std::fs::remove_file(&install_wim)?;

    std::fs::copy(r.dir.join("autounattend.xml"), iso.join("autounattend.xml"))?;

    // Builds >= 26000 boot into the new "ConX" setup, which drops the
    // specialize and oobeSystem passes of an answer file. WinPE runs whatever
    // winpeshl.ini says instead of its default, so point it at legacy setup
    // with the answer file baked into boot.wim (image 2 is the setup image).
    let winpeshl = work.join("winpeshl.ini");
    std::fs::write(&winpeshl, "[LaunchApps]\r\n%SYSTEMDRIVE%\\sources\\setup.exe, /legacy /unattend:%SYSTEMDRIVE%\\autounattend.xml\r\n")?;
    tools::run_with_input(
        WIMLIB.cmd().arg("update").arg(iso.join("sources/boot.wim")).arg("2"),
        &format!("add {} /autounattend.xml\nadd {} /Windows/System32/winpeshl.ini\n", quote(&r.dir.join("autounattend.xml"))?, quote(&winpeshl)?),
    )?;

    eprintln!("writing {ISO_NAME}");
    let efi_boot = if variant.boot_prompt { "efisys.bin" } else { "efisys_noprompt.bin" };
    tools::run(
        XORRISO
            .cmd()
            .args(["-as", "mkisofs", "-quiet", "-iso-level", "3", "-J", "-joliet-long", "-V", "DEFENESTRATE_X64"])
            .args(["-c", "boot.catalog", "-hide", "boot.catalog", "-hide-joliet", "boot.catalog"])
            .args(["-b", "boot/etfsboot.com", "-no-emul-boot", "-boot-load-size", "8"])
            .args(["-eltorito-alt-boot", "-e", &format!("efi/microsoft/boot/{efi_boot}"), "-no-emul-boot"])
            .arg("-o")
            .arg(out.join(ISO_NAME))
            .arg(&iso),
    )?;
    Ok(())
}

/// Merges each .reg into a scratch copy of the hive it targets. The copies
/// are thrown away (specialize.cmd applies the real thing on Windows), this
/// just makes a malformed .reg fail the build instead of a VM install 20
/// minutes in.
fn check_registry(install_wim: &Path, rendered: &Path, work: &Path) -> Result<()> {
    eprintln!("checking the registry config against the image's hives");
    let hives = work.join("hives");
    std::fs::create_dir_all(&hives)?;
    let mut extract = WIMLIB.cmd();
    extract.arg("extract").arg(install_wim).arg("1");
    let wanted: BTreeSet<&str> = REG_FILES.iter().map(|f| f.root.hive_file()).collect();
    extract.args(&wanted).arg(format!("--dest-dir={}", hives.display())).arg("--no-acls");
    tools::run(&mut extract)?;
    for f in &REG_FILES {
        let hive = hives.join(Path::new(f.root.hive_file()).file_name().unwrap());
        tools::run(HIVEXREGEDIT.cmd().arg("--merge").arg("--prefix").arg(f.root.path()).arg(&hive).arg(rendered.join(format!("reg/ascii/{}.reg", f.name))))
            .with_context(|| format!("{}.reg doesn't merge into the image's hive", f.name))?;
    }
    std::fs::remove_dir_all(&hives)?;
    Ok(())
}

/// A typo'd package name fails here, not as a silent no-op at install time.
/// Every package in the image has a folder named
/// <name>_<version>_<arch>_<resource>_<publisher> in WindowsApps.
fn check_appx(config: &Config, install_wim: &Path) -> Result<()> {
    if config.appx.remove.is_empty() {
        return Ok(());
    }
    let listing = tools::output_string(WIMLIB.cmd().arg("dir").arg(install_wim).arg("1").arg("--path").arg("/Program Files/WindowsApps"))?;
    let shipped: BTreeSet<&str> = listing
        .lines()
        .filter_map(|l| l.strip_prefix("/Program Files/WindowsApps/"))
        .filter(|rest| !rest.contains('/'))
        .filter_map(|rest| rest.split_once('_').map(|(name, _)| name))
        .collect();
    let missing: Vec<&str> = config.appx.remove.iter().map(|n| n.as_str()).filter(|n| !shipped.contains(n)).collect();
    if !missing.is_empty() {
        bail!("appx.remove names packages this image doesn't ship:\n  {}", missing.join("\n  "));
    }
    Ok(())
}

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// wimlib's update commands take double-quoted paths.
fn quote(p: &Path) -> Result<String> {
    let s = p.to_str().context("non-UTF-8 path")?;
    if s.contains('"') {
        bail!("{s}: paths with \" can't go through wimlib's update commands");
    }
    Ok(format!("\"{s}\""))
}

fn make_writable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in walk(dir)? {
        let meta = entry.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o200);
        std::fs::set_permissions(&entry, perm)?;
    }
    Ok(())
}

fn walk(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = vec![dir.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        if out[i].is_dir() && !out[i].is_symlink() {
            for e in std::fs::read_dir(&out[i])? {
                out.push(e?.path());
            }
        }
        i += 1;
    }
    Ok(out)
}
