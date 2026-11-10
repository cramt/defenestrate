//! Rewrites defenestrate's own files on an installed Windows from Linux,
//! while Windows is shut down, so a fix to the shell or the WSL payload
//! doesn't take a rebuild and reflash. On a dual boot machine the Windows
//! disk is right there.
//!
//! Only files the install already has are written, and in place (truncated
//! and rewritten), which keeps each one's NTFS security descriptor. A file
//! ntfs-3g creates gets a descriptor of its own instead, Everyone: full
//! control, and several of these run elevated. Nothing is lost by that: a
//! file the install doesn't have (a new font, the shell under a new name, the
//! WSL payload when wsl.linux is new) also needs registering, which only
//! setup does.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::config::Config;
use crate::disk;
use crate::render::{self, InstallFile, ReadAt, SETUP_DIR};
use crate::tools::{self, FINDMNT, LSBLK, NTFS3G, NTFS3G_PROBE};
use crate::unattend::Variant;

#[derive(clap::Args)]
pub struct Args {
    /// Whole disk Windows is installed on, by /dev/disk/by-id/ [default: the config's installTarget]
    target: Option<String>,
    /// List what would change, writing nothing.
    #[arg(long)]
    dry_run: bool,
    /// Update the NTFS volume in this file instead of a disk's Windows
    /// partition, skipping every disk check. For tests.
    #[arg(long, hide = true, value_name = "FILE", conflicts_with = "target")]
    volume_image: Option<PathBuf>,
}

const USAGE: &str = "dfnstrate update -c CONFIG... /dev/disk/by-id/<the disk Windows is on>";

pub fn run(config: &Config, cache: &Cache, args: &Args) -> Result<()> {
    tools::require(&[NTFS3G, NTFS3G_PROBE])?;
    let volume = match &args.volume_image {
        Some(file) => file.clone(),
        None => {
            tools::require(&[LSBLK, FINDMNT])?;
            let disk = disk::unused(disk::target(args.target.as_deref(), config.install_target.clone(), USAGE)?)?;
            let part = disk.partition(disk::WINDOWS_PART);
            if !std::fs::metadata(&part).is_ok_and(|m| m.file_type().is_block_device()) {
                bail!("{} has no partition {}; is Windows on it?", disk.target.as_str(), disk::WINDOWS_PART);
            }
            let fs_type = tools::output_string(LSBLK.cmd().args(["-no", "FSTYPE"]).arg(&part))?;
            if fs_type.trim() != "ntfs" {
                bail!("{part} is {:?}, not NTFS; is Windows on this disk?", fs_type.trim());
            }
            if !disk::is_root() {
                // Rendered as the caller first: that fails early on a bad
                // config, and downloads into the caller's cache, which root
                // then reads offline rather than filling its own.
                Scratch::new("render")?.render(config, cache)?;
                let mut extra: Vec<OsString> = Vec::new();
                if args.target.is_none() {
                    extra.push(disk.target.as_str().into());
                }
                extra.extend(["--cache".into(), std::path::absolute(&cache.dir)?.into(), "--offline".into()]);
                disk::rerun_as_root(extra)?;
            }
            PathBuf::from(part)
        }
    };

    let scratch = Scratch::new("render")?;
    let files = scratch.render(config, cache)?;
    probe(&volume, !args.dry_run)?;
    let mount = Mount::new(&volume, std::env::temp_dir().join(format!("dfnstrate-update-{}", std::process::id())), !args.dry_run)?;
    let root = mount.dir.as_path();
    if !root.join(SETUP_DIR).is_dir() {
        bail!("{} has no C:\\{}; not a defenestrate install", volume.display(), windows(SETUP_DIR));
    }

    let changes = files.iter().map(|f| change(f, root).map(|c| (f, c))).collect::<Result<Vec<_>>>()?;
    let mut written = 0;
    for (f, c) in &changes {
        if *c == Change::Write {
            if !args.dry_run {
                write_in_place(&f.source, &root.join(&f.dest)).with_context(|| format!("writing C:\\{}", windows(&f.dest)))?;
            }
            println!("{} C:\\{}", if args.dry_run { "would update" } else { "updated" }, windows(&f.dest));
            written += 1;
        }
    }
    mount.unmount()?;

    let unchanged = changes.iter().filter(|(_, c)| *c == Change::None).count();
    println!("{written} {}, {unchanged} already up to date", if args.dry_run { "to update" } else { "updated" });
    let list = |want: Change| changes.iter().filter(|(_, c)| *c == want).map(|(f, _)| format!("\n  C:\\{}", windows(&f.dest))).collect::<String>();
    let setup_only = list(Change::SetupOnly);
    if !setup_only.is_empty() {
        eprintln!("left alone, since only setup reads them; these changes take a reflash:{setup_only}");
    }
    let missing = list(Change::Missing);
    if !missing.is_empty() {
        eprintln!("not on this install, and only setup can add them; these take a reflash:{missing}");
    }
    if written > 0 && !args.dry_run {
        println!("They take effect at the next logon.");
    }
    Ok(())
}

/// What an update does with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// The install has it already.
    None,
    /// Differs, and Windows reads it again: rewritten.
    Write,
    /// Differs, but only setup reads it, so rewriting it would change nothing.
    SetupOnly,
    /// Not on the install. Never created (see the module docs).
    Missing,
}

fn change(file: &InstallFile, root: &Path) -> Result<Change> {
    let dest = root.join(&file.dest);
    let meta = match std::fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Change::Missing),
        Err(e) => return Err(e).with_context(|| dest.display().to_string()),
    };
    if !meta.is_file() {
        bail!("C:\\{} on the install isn't a file", windows(&file.dest));
    }
    if same_contents(&file.source, &dest)? {
        return Ok(Change::None);
    }
    Ok(match file.read {
        ReadAt::Setup => Change::SetupOnly,
        ReadAt::EveryLogon => Change::Write,
    })
}

fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    let (mut fa, mut fb) = (File::open(a).with_context(|| a.display().to_string())?, File::open(b).with_context(|| b.display().to_string())?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        if n != read_full(&mut fb, &mut bb)? || ba[..n] != bb[..n] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Truncates and rewrites `dest` instead of replacing it, so to NTFS it stays
/// the same file, security descriptor included. Never creates it.
fn write_in_place(source: &Path, dest: &Path) -> Result<()> {
    let mut from = File::open(source).with_context(|| source.display().to_string())?;
    let mut to = File::options().write(true).truncate(true).open(dest)?;
    std::io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    Ok(())
}

fn windows(dest: &str) -> String {
    dest.replace('/', "\\")
}

/// ntfs-3g mounts a volume it won't write read-only without failing, so
/// whether it would write is asked first, for a message that says why not.
fn probe(volume: &Path, write: bool) -> Result<()> {
    let out = NTFS3G_PROBE.cmd().arg(if write { "--readwrite" } else { "--readonly" }).arg(volume).stdin(Stdio::null()).output()?;
    let v = volume.display();
    // Exit codes from ntfs-3g's ntfs_volume_error().
    match out.status.code() {
        Some(0) => Ok(()),
        Some(12) => bail!("{v} isn't NTFS"),
        Some(14) => bail!(
            "Windows on {v} is hibernated, or was shut down with Fast Startup, so some of its files are still open in a saved memory image. \
             Writing under it would corrupt them. Boot Windows and shut it down fully (shutdown /s /t 0), then retry. \
             The default config turns Fast Startup off (HiberbootEnabled)."
        ),
        Some(15) => bail!("{v} wasn't shut down cleanly (a crash or power loss). Boot Windows once and shut it down, then retry."),
        _ => bail!("ntfs-3g.probe {v} failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()),
    }
}

/// A volume mounted with ntfs-3g, unmounted on drop.
struct Mount {
    dir: PathBuf,
    /// ntfs-3g's stderr: it chats there when not detached, and only says why
    /// it failed when it does.
    log: PathBuf,
    /// Run with no_detach, so it stays our child: unmounting waits for it to
    /// exit, which is when it has written the volume out. umount alone
    /// returns before that.
    ntfs3g: Option<Child>,
}

const FUSE_SUPER_MAGIC: i64 = 0x6573_5546;

impl Mount {
    fn new(volume: &Path, dir: PathBuf, write: bool) -> Result<Self> {
        std::fs::create_dir(&dir).with_context(|| dir.display().to_string())?;
        let log = dir.with_extension("log");
        let child = NTFS3G
            .cmd()
            .arg("-o")
            .arg(if write { "rw,no_detach" } else { "ro,no_detach" })
            .arg(volume)
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log)?)
            .spawn()
            .context("running ntfs-3g")?;
        let mut m = Self { dir, log, ntfs3g: Some(child) };
        for _ in 0..300 {
            if statfs(&m.dir)?.f_type as i64 == FUSE_SUPER_MAGIC {
                if write && read_only(&m.dir)? {
                    bail!("ntfs-3g mounted {} read-only", volume.display());
                }
                return Ok(m);
            }
            if let Some(status) = m.ntfs3g.as_mut().unwrap().try_wait()? {
                m.ntfs3g = None;
                bail!("ntfs-3g couldn't mount {} ({status}):\n{}", volume.display(), m.said());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("ntfs-3g didn't mount {} within 30 seconds", volume.display())
    }

    fn unmount(mut self) -> Result<()> {
        self.finish()
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(mut child) = self.ntfs3g.take() {
            if statfs(&self.dir)?.f_type as i64 == FUSE_SUPER_MAGIC {
                let c = std::ffi::CString::new(self.dir.as_os_str().as_encoded_bytes())?;
                // SAFETY: c is a valid NUL-terminated path.
                if unsafe { libc::umount(c.as_ptr()) } != 0 {
                    let e = std::io::Error::last_os_error();
                    // Still mounted: leave it, and the directory, alone.
                    bail!("unmounting {}: {e}", self.dir.display());
                }
            } else {
                child.kill().ok();
            }
            let status = child.wait()?;
            if !status.success() {
                bail!("ntfs-3g exited with {status} after unmounting:\n{}", self.said());
            }
        }
        // Not remove_dir_all: if this were still a mount, that would empty
        // the Windows volume.
        std::fs::remove_dir(&self.dir).ok();
        std::fs::remove_file(&self.log).ok();
        Ok(())
    }

    fn said(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default().trim().to_owned()
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            eprintln!("warning: {e:#}");
        }
    }
}

fn statfs(dir: &Path) -> Result<libc::statfs> {
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
    // SAFETY: c is a valid path and st is written by the call before use.
    unsafe {
        let mut st: libc::statfs = std::mem::zeroed();
        if libc::statfs(c.as_ptr(), &mut st) != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| dir.display().to_string());
        }
        Ok(st)
    }
}

fn read_only(dir: &Path) -> Result<bool> {
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
    // SAFETY: as in statfs.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| dir.display().to_string());
        }
        Ok(st.f_flag & libc::ST_RDONLY != 0)
    }
}

/// A directory for rendering into, removed on drop. Never holds a mount.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("dfnstrate-{tag}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn render(&self, config: &Config, cache: &Cache) -> Result<Vec<InstallFile>> {
        Ok(render::render(config, &Variant::from_config(config), cache, &self.0.join("work"), &self.0.join("out"))?.files)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dfnstrate-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_in_place() {
        let d = tmp("in-place");
        let (src, dest) = (d.join("new"), d.join("old"));
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dest, b"old contents, longer").unwrap();
        let ino = std::fs::metadata(&dest).unwrap().ino();
        write_in_place(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&dest).unwrap().ino(), ino, "replaced rather than rewritten");
        assert!(write_in_place(&src, &d.join("absent")).is_err());
        assert!(!d.join("absent").exists(), "created a file");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn classifies_changes() {
        let d = tmp("changes");
        let (src, root) = (d.join("src"), d.join("c"));
        std::fs::create_dir_all(src.join("x")).unwrap();
        std::fs::create_dir_all(root.join("x")).unwrap();
        let file = |name: &str, ours: &str, theirs: Option<&str>, read: ReadAt| {
            std::fs::write(src.join(name), ours).unwrap();
            if let Some(t) = theirs {
                std::fs::write(root.join("x").join(name), t).unwrap();
            }
            InstallFile::new(format!("x/{name}"), src.join(name), read)
        };
        let cases = [
            (file("same", "a", Some("a"), ReadAt::EveryLogon), Change::None),
            (file("same-setup", "a", Some("a"), ReadAt::Setup), Change::None),
            (file("newer", "b", Some("a"), ReadAt::EveryLogon), Change::Write),
            (file("same-size", "ab", Some("ba"), ReadAt::EveryLogon), Change::Write),
            (file("setup", "b", Some("a"), ReadAt::Setup), Change::SetupOnly),
            (file("missing", "b", None, ReadAt::EveryLogon), Change::Missing),
        ];
        for (f, want) in &cases {
            assert_eq!(change(f, &root).unwrap(), *want, "{}", f.dest);
        }
        std::fs::create_dir(root.join("x/dir")).unwrap();
        assert!(change(&file("dir", "a", None, ReadAt::EveryLogon), &root).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn compares_past_the_first_buffer() {
        let d = tmp("compare");
        let mut a = vec![7u8; (1 << 20) + 5];
        std::fs::write(d.join("a"), &a).unwrap();
        std::fs::write(d.join("b"), &a).unwrap();
        assert!(same_contents(&d.join("a"), &d.join("b")).unwrap());
        *a.last_mut().unwrap() = 8;
        std::fs::write(d.join("b"), &a).unwrap();
        assert!(!same_contents(&d.join("a"), &d.join("b")).unwrap());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
