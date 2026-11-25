//! Stock ISO + config → install ISO.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::config::Config;
use crate::registry::Root;
use crate::render::{self, REG_FILES};
use crate::tools::{self, HIVEXREGEDIT, SEVENZIP, WIMLIB, XORRISO};
use crate::unattend::Variant;

pub const TOOLS: &[tools::Tool] = &[SEVENZIP, WIMLIB, HIVEXREGEDIT, XORRISO];

pub const ISO_NAME: &str = "defenestrate.iso";
/// Under SETUP_DIR, where specialize.cmd looks for `drivers`.
const DRIVERS_DIR: &str = "drivers";
/// On the ISO, where updates.cmd looks for `updates`. At the root, named to
/// sort after sources/: xorriso writes files in path order, and anything
/// before boot.wim pushes it along (see check_boot_files).
const UPDATES_DIR: &str = "updates";

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

    check_registry(config, &install_wim, &r.dir, work)?;
    check_appx(config, &install_wim)?;

    // Only adds new files. Replacing an existing file this way drops its
    // security descriptor, which is why the registry isn't edited offline.
    // wimlib creates the parent directories, and adds a symlink as a link
    // (to a path Windows doesn't have), hence canonicalize.
    let drivers = unpack_drivers(config, cache, work)?;
    eprintln!("adding the payload");
    let mut commands = r
        .files
        .iter()
        .map(|f| {
            let source = std::fs::canonicalize(&f.source).with_context(|| f.source.display().to_string())?;
            Ok(format!("add {} {}\n", quote(&source)?, quote(Path::new(&format!("/{}", f.dest.rel())))?))
        })
        .collect::<Result<String>>()?;
    // Here rather than in render's file list: `update` would only ever see
    // these as missing, since specialize deletes them once they're in the
    // driver store, and `render` has no use for gigabytes of drivers.
    for (i, dir) in drivers.iter().enumerate() {
        commands += &format!("add {} {}\n", quote(dir)?, quote(Path::new(&format!("/{}", render::SETUP_DIR.join(&format!("{DRIVERS_DIR}/{i}")).rel())))?);
    }
    tools::run_with_input(WIMLIB.cmd().arg("update").arg(&install_wim).arg("1"), &commands)?;

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
    //
    // With `updates`, setup stops after applying the image (/noreboot) and
    // updates.cmd adds them to it offline before Windows ever starts, which
    // is how an updated image is made: specialize, OOBE and first logon then
    // run on the updated system, with nothing left for a first boot to do.
    // Windows can't be serviced from Linux, hence WinPE.
    let updates = config
        .updates
        .iter()
        .enumerate()
        .map(|(i, source)| {
            let file = cache.materialize(source, work)?;
            // dir /on order is install order.
            Ok((format!("{UPDATES_DIR}/{i:02}-{}", file.name), std::fs::canonicalize(&file.path)?))
        })
        .collect::<Result<Vec<_>>>()?;
    let winpeshl = work.join("winpeshl.ini");
    let updates_cmd = work.join("updates.cmd");
    let mut boot_wim = format!("add {} /autounattend.xml\nadd {} /Windows/System32/winpeshl.ini\n", quote(&r.dir.join("autounattend.xml"))?, quote(&winpeshl)?);
    if updates.is_empty() {
        std::fs::write(&winpeshl, "[LaunchApps]\r\n%SYSTEMDRIVE%\\sources\\setup.exe, /legacy /unattend:%SYSTEMDRIVE%\\autounattend.xml\r\n")?;
    } else {
        std::fs::write(
            &winpeshl,
            "[LaunchApps]\r\n%SYSTEMDRIVE%\\sources\\setup.exe, /legacy /noreboot /unattend:%SYSTEMDRIVE%\\autounattend.xml\r\n%SYSTEMROOT%\\System32\\cmd.exe, /c %SYSTEMROOT%\\System32\\defenestrate-updates.cmd\r\n",
        )?;
        std::fs::write(&updates_cmd, render::crlf(include_str!("../assets/updates.cmd")))?;
        boot_wim += &format!("add {} /Windows/System32/defenestrate-updates.cmd\n", quote(&updates_cmd)?);
    }
    tools::run_with_input(WIMLIB.cmd().arg("update").arg(iso.join("sources/boot.wim")).arg("2"), &boot_wim)?;

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
            // Grafted from the cache rather than copied: they're gigabytes.
            // A cumulative update is over FAT32's 4 GiB, so an ISO with
            // updates no longer copies onto a FAT32 stick as is.
            .arg("-graft-points")
            .arg(&iso)
            .args(updates.iter().map(|(at, from)| format!("{at}={}", from.display()))),
    )?;
    check_boot_files(&out.join(ISO_NAME))
}

/// Windows Boot Manager can't read a boot file that ends past 4 GiB into
/// the CD. With a 4.7 GB update laid out before boot.wim, a VM booting the
/// ISO reset every minute without starting WinPE. Setup's own reads, from
/// install.swm, aren't affected.
fn check_boot_files(iso: &Path) -> Result<()> {
    let report = tools::output_string(XORRISO.cmd().arg("-indev").arg(iso).args(["-find", "/", "-type", "f", "-exec", "report_lba", "--"]))?;
    let late = late_boot_files(&report);
    if !late.is_empty() {
        bail!("{} has boot files past 4 GiB, which Windows Boot Manager can't read: {}", iso.display(), late.join(", "));
    }
    Ok(())
}

/// The boot files in xorriso's report_lba output that end past 4 GiB.
fn late_boot_files(report: &str) -> Vec<String> {
    const LIMIT: u64 = (1 << 32) / 2048;
    report
        .lines()
        .filter_map(|l| l.strip_prefix("File data lba:"))
        .filter_map(|l| {
            let fields: Vec<&str> = l.splitn(5, ',').map(str::trim).collect();
            let [_, start, blocks, _, path] = fields[..] else { return None };
            let path = path.trim_matches('\'');
            let boot = path.starts_with("/boot/") || path.starts_with("/efi/") || path.starts_with("/bootmgr") || path == "/sources/boot.wim";
            let end = start.parse::<u64>().ok()? + blocks.parse::<u64>().ok()?;
            (boot && end > LIMIT).then(|| path.to_owned())
        })
        .collect()
}

/// Each of `drivers` unpacked under work/drivers/<n>. One with no .inf in
/// it fails the build: at specialize it would be a silent no-op.
fn unpack_drivers(config: &Config, cache: &Cache, work: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for (i, source) in config.drivers.iter().enumerate() {
        let archive = cache.materialize(source, work)?;
        eprintln!("unpacking driver {}", archive.name);
        let dir = work.join(DRIVERS_DIR).join(i.to_string());
        tools::run(SEVENZIP.cmd().args(["x", "-y"]).arg(format!("-o{}", dir.display())).arg(&archive.path))?;
        if !has_inf(&dir)? {
            bail!("drivers: {} has no .inf in it; not a driver package?", archive.name);
        }
        dirs.push(std::fs::canonicalize(&dir)?);
    }
    Ok(dirs)
}

fn has_inf(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if has_inf(&path)? {
                return Ok(true);
            }
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("inf")) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Merges each .reg into a scratch copy of the hive it targets. The copies
/// are thrown away (specialize.cmd applies the real thing on Windows), this
/// just makes a malformed .reg fail the build instead of a VM install 20
/// minutes in. The answer file's language and time zone are checked against
/// the image while it's out, which is the only place that knows them.
fn check_registry(config: &Config, install_wim: &Path, rendered: &Path, work: &Path) -> Result<()> {
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
    let software = hives.join(Path::new(Root::Software.hive_file()).file_name().unwrap());
    let zones = tools::output_string(HIVEXREGEDIT.cmd().arg("--export").arg(&software).arg(format!("\\{TIME_ZONES}")))?;
    check_time_zone(config.time_zone.as_str(), &time_zones(&zones))?;
    std::fs::remove_dir_all(&hives)?;
    let info = tools::output_string(WIMLIB.cmd().arg("info").arg(install_wim).arg("1"))?;
    check_language(config.language.as_str(), &info)?;
    Ok(())
}

const TIME_ZONES: &str = "Microsoft\\Windows NT\\CurrentVersion\\Time Zones";

/// The time zone IDs under SOFTWARE's Time Zones key, from hivexregedit's
/// export of it: the keys right under it.
fn time_zones(export: &str) -> BTreeSet<String> {
    let under = format!("\\{TIME_ZONES}\\");
    export
        .lines()
        .filter_map(|l| l.strip_prefix('[')?.strip_suffix(']'))
        .filter_map(|key| Some(key[key.find(&under)? + under.len()..].to_owned()))
        .filter(|zone| !zone.contains('\\'))
        .collect()
}

/// An empty list means the export wasn't what this expects, not that the
/// image has no time zones, so it only warns.
fn check_time_zone(zone: &str, known: &BTreeSet<String>) -> Result<()> {
    if known.is_empty() {
        eprintln!("warning: found no time zones in the image to check timeZone against");
    } else if !known.iter().any(|k| k.eq_ignore_ascii_case(zone)) {
        bail!("config: timeZone: the image has no time zone {zone:?}; `tzutil /l` on Windows lists them (the IDs, not the descriptions)");
    }
    Ok(())
}

/// What `wimlib-imagex info` lists on its Languages line.
fn check_language(language: &str, info: &str) -> Result<()> {
    let Some(line) = info.lines().find_map(|l| l.strip_prefix("Languages:")) else {
        eprintln!("warning: wimlib lists no languages for the image to check language against");
        return Ok(());
    };
    let shipped: Vec<&str> = line.split_whitespace().collect();
    if !shipped.iter().any(|l| l.eq_ignore_ascii_case(language)) {
        bail!("config: language: the image has {}, not {language}; it comes from the ISO (iso.language)", shipped.join(", "));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_zones_and_languages_from_the_image() {
        let export = "Windows Registry Editor Version 5.00\n\n[\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones]\n\"TzVersion\"=dword:1\n\n[\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones\\Romance Standard Time]\n\"Std\"=\"x\"\n\n[\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones\\Romance Standard Time\\Dynamic DST]\n\n[\\Microsoft\\Windows NT\\CurrentVersion\\Time Zones\\UTC]\n";
        let zones = time_zones(export);
        assert_eq!(zones.iter().map(String::as_str).collect::<Vec<_>>(), ["Romance Standard Time", "UTC"]);
        assert!(check_time_zone("utc", &zones).is_ok());
        assert!(check_time_zone("W. Europe Standard Time", &zones).unwrap_err().to_string().contains("tzutil"));
        assert!(check_time_zone("anything", &BTreeSet::new()).is_ok());

        let info = "Index:                  1\nName:                   Windows 11 Pro\nLanguages:              en-US da-DK\nDefault Language:       en-US\n";
        assert!(check_language("da-DK", info).is_ok());
        assert!(check_language("de-DE", info).unwrap_err().to_string().contains("en-US, da-DK"));
        assert!(check_language("de-DE", "Index: 1\n").is_ok());
    }

    #[test]
    fn finds_boot_files_past_4_gib() {
        // From the ISO whose VM reset every minute (2026-11-17), trimmed.
        let report = "Report layout: xt , Startlba ,   Blocks , Filesize , ISO image path
File data lba:  0 ,    11859 ,      232 ,   473364 , '/bootmgr'
File data lba:  0 ,    13591 ,  2097151 , 4730812571 , '/defenestrate/updates/00-windows11.0-kb5132061-x64.msu'
File data lba:  1 ,  2110742 ,   212817 , 4730812571 , '/defenestrate/updates/00-windows11.0-kb5132061-x64.msu'
File data lba:  0 ,  2326123 ,      720 ,  1474560 , '/efi/microsoft/boot/efisys.bin'
File data lba:  0 ,  2340426 ,   299749 , 613885050 , '/sources/boot.wim'
File data lba:  0 ,  2660888 ,  1942836 , 3978927310 , '/sources/install.swm'";
        assert_eq!(late_boot_files(report), ["/efi/microsoft/boot/efisys.bin", "/sources/boot.wim"]);
        // The working layout: install.swm may run past 4 GiB, setup reads it.
        let fine = "File data lba:  0 ,    30454 ,   299749 , 613883931 , '/sources/boot.wim'
File data lba:  0 ,   350916 ,  1942833 , 3978921184 , '/sources/install.swm'
File data lba:  0 ,  2300000 ,  2097151 , 4730812571 , '/updates/00-windows11.0-kb5132061-x64.msu'";
        assert!(late_boot_files(fine).is_empty());
    }
}
