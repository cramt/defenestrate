//! Rewrites defenestrate's own files on an installed Windows from Linux,
//! while Windows is shut down, so a fix to the shell or the WSL payload
//! doesn't take a rebuild and reflash. On a dual boot machine the Windows
//! disk is right there.
//!
//! Files the install already has are written in place (truncated and
//! rewritten), which keeps each one's NTFS security descriptor. A file
//! ntfs-3g creates gets a descriptor of its own instead, Everyone: full
//! control, and several of these run elevated. So a file the install doesn't
//! have is left alone, which loses nothing when it also needs registering,
//! which only setup does (a new font, the shell under a new name, the WSL
//! payload when wsl.linux is new). The exception is a file that needs
//! nothing but to exist next to one setup made (IfMissing::CreateLike): it's
//! created with that file's descriptor, set before anything is written to it.
//!
//! The one registry edit: a new shell build is signed with a new
//! certificate, which goes into the SOFTWARE hive's Root store for the
//! shell to keep its uiAccess, and the previous build's comes out (cert.rs).

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::cert::{self, ShellCert};
use crate::config::Config;
use crate::disk;
use crate::registry::{self, Root};
use crate::render::{self, IfMissing, InstallFile, ReadAt, Rendered, SETUP_DIR};
use crate::tools::{self, FINDMNT, HIVEXREGEDIT, LSBLK, NTFS3G, NTFS3G_PROBE};
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
    tools::require(&[NTFS3G, NTFS3G_PROBE, HIVEXREGEDIT])?;
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
    let Rendered { files, shell_cert, .. } = scratch.render(config, cache)?;
    probe(&volume, !args.dry_run)?;
    let mount = Mount::new(&volume, std::env::temp_dir().join(format!("dfnstrate-update-{}", std::process::id())), !args.dry_run)?;
    let root = mount.dir.as_path();
    if !root.join(SETUP_DIR).is_dir() {
        bail!("{} has no C:\\{}; not a defenestrate install", volume.display(), windows(SETUP_DIR));
    }

    let changes = files.iter().map(|f| change(f, root).map(|c| (f, c))).collect::<Result<Vec<_>>>()?;
    let hive = root.join(Root::Software.hive_file().trim_start_matches('/'));
    let trust = match &shell_cert {
        Some(cert) => Trust::plan(cert, &hive)?,
        None => Trust::default(),
    };
    // The new certificate goes in before the new shell, and the old one comes
    // out after it, so whichever shell is on disk if this stops halfway is
    // trusted.
    if let (Some(cert), true) = (&shell_cert, trust.add) {
        if !args.dry_run {
            merge(&hive, &trust_text(Some(cert), &[]), &scratch.0).context("adding the shell's certificate to the Root store")?;
        }
        println!("{} the shell's certificate {} in the Root store", if args.dry_run { "would trust" } else { "trusted" }, cert.thumbprint());
    }
    let mut written = 0;
    for (f, c) in &changes {
        let dest = root.join(&f.dest);
        let done = match (c, &f.if_missing) {
            (Change::Write, _) => {
                if !args.dry_run {
                    write_in_place(&f.source, &dest).with_context(|| format!("writing C:\\{}", windows(&f.dest)))?;
                }
                if args.dry_run { "would update" } else { "updated" }
            }
            (Change::Create, IfMissing::CreateLike(like)) => {
                if !args.dry_run {
                    create_like(&f.source, &dest, &root.join(like)).with_context(|| format!("creating C:\\{}", windows(&f.dest)))?;
                }
                if args.dry_run { "would create" } else { "created" }
            }
            _ => continue,
        };
        println!("{done} C:\\{}", windows(&f.dest));
        written += 1;
    }
    if !trust.remove.is_empty() {
        if !args.dry_run {
            merge(&hive, &trust_text(None, &trust.remove), &scratch.0).context("removing earlier shell certificates from the Root store")?;
        }
        for t in &trust.remove {
            println!("{} the earlier shell's certificate {t} from the Root store", if args.dry_run { "would remove" } else { "removed" });
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
    if (written > 0 || trust.add) && !args.dry_run {
        println!("They take effect at the next logon.");
    }
    Ok(())
}

/// What the Root store needs for the new shell's certificate.
#[derive(Debug, Default, PartialEq)]
struct Trust {
    add: bool,
    /// Thumbprints of earlier builds' certificates.
    remove: Vec<String>,
}

impl Trust {
    fn plan(cert: &ShellCert, hive: &Path) -> Result<Self> {
        let path = format!("\\{}", cert::ROOT_STORE);
        let out = tools::output_string(HIVEXREGEDIT.cmd().arg("--export").arg(hive).arg(&path))
            .context("reading the Root store out of the SOFTWARE hive")?;
        Ok(Self::from_export(cert, &out))
    }

    /// From hivexregedit's export of the Root store: one section per
    /// certificate, each with its Blob on one hex(3) line.
    fn from_export(cert: &ShellCert, export: &str) -> Self {
        let mut ours = Vec::new();
        let mut thumbprint = None;
        for line in export.lines() {
            if let Some(key) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                thumbprint = key.rsplit_once('\\').map(|(_, t)| t.to_owned());
            } else if let (Some(hex), Some(t)) = (line.strip_prefix("\"Blob\"=hex(3):"), &thumbprint) {
                let blob: Option<Vec<u8>> = hex.split(',').map(|b| u8::from_str_radix(b, 16).ok()).collect();
                if blob.as_deref().and_then(cert::cert_in_blob).is_some_and(|der| ShellCert::from_der(der).is_ok()) {
                    ours.push(t.clone());
                }
            }
        }
        let new = cert.thumbprint();
        Self { add: !ours.contains(&new), remove: ours.into_iter().filter(|t| *t != new).collect() }
    }
}

/// A .reg (ASCII, for hivexregedit) adding `add` to the Root store and
/// deleting the certificates `remove` names.
fn trust_text(add: Option<&ShellCert>, remove: &[String]) -> String {
    let mut keys = crate::config::Hive::new();
    if let Some(cert) = add {
        keys.entry(cert.root_key()).or_default().insert("Blob".into(), crate::config::RegValue::Binary(cert.blob()));
    }
    let mut text = registry::render(Root::Software, &keys);
    for t in remove {
        text += &format!("[-{}\\{}\\{t}]\r\n\r\n", Root::Software.path(), cert::ROOT_STORE);
    }
    text
}

/// Edits the hive in place: hivex rewrites the file it opened (O_TRUNC,
/// not a new file), so like the files above it keeps its NTFS security
/// descriptor, which for a hive matters (see the README's ACL note).
fn merge(hive: &Path, reg: &str, scratch: &Path) -> Result<()> {
    let file = scratch.join("root-store.reg");
    std::fs::write(&file, reg)?;
    tools::run(HIVEXREGEDIT.cmd().arg("--merge").arg("--prefix").arg(Root::Software.path()).arg(hive).arg(&file))
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
    /// Not on the install, and only setup can add it (see the module docs).
    Missing,
    /// Not on the install, but can be created (IfMissing::CreateLike).
    Create,
}

fn change(file: &InstallFile, root: &Path) -> Result<Change> {
    let dest = root.join(&file.dest);
    let meta = match std::fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(match &file.if_missing {
                IfMissing::CreateLike(like) if root.join(like).is_file() => Change::Create,
                _ => Change::Missing,
            });
        }
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

/// ntfs-3g's view of a file's whole NTFS security descriptor.
const NTFS_ACL: &str = "system.ntfs_acl";

/// Creates `dest` with `like`'s security descriptor, then writes `source`
/// into it. If the descriptor can't be set, `dest` is removed again rather
/// than left with ntfs-3g's own (Everyone: full control).
fn create_like(source: &Path, dest: &Path, like: &Path) -> Result<()> {
    let descriptor = xattr(like, NTFS_ACL).with_context(|| format!("reading {}'s security descriptor", like.display()))?;
    File::options().write(true).create_new(true).open(dest)?;
    if let Err(e) = set_xattr(dest, NTFS_ACL, &descriptor) {
        std::fs::remove_file(dest).ok();
        return Err(e).context("setting its security descriptor");
    }
    write_in_place(source, dest)
}

fn xattr(path: &Path, name: &str) -> Result<Vec<u8>> {
    let (p, n) = (std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?, std::ffi::CString::new(name)?);
    // SAFETY: p and n are valid NUL-terminated strings; a null buffer asks
    // for the size, and the second call writes at most buf.len() bytes.
    unsafe {
        let len = libc::getxattr(p.as_ptr(), n.as_ptr(), std::ptr::null_mut(), 0);
        if len < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut buf = vec![0u8; len as usize];
        let got = libc::getxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len());
        if got < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        buf.truncate(got as usize);
        Ok(buf)
    }
}

fn set_xattr(path: &Path, name: &str, value: &[u8]) -> Result<()> {
    let (p, n) = (std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?, std::ffi::CString::new(name)?);
    // SAFETY: p and n are valid NUL-terminated strings and value is read for
    // value.len() bytes.
    if unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
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

    fn render(&self, config: &Config, cache: &Cache) -> Result<Rendered> {
        render::render(config, &Variant::from_config(config), cache, &self.0.join("work"), &self.0.join("out"))
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

    fn shell_cert() -> ShellCert {
        ShellCert::from_exe(include_bytes!("../tests/fixtures/signed-headers.exe")).unwrap().unwrap()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(",")
    }

    #[test]
    fn plans_the_root_store() {
        let cert = shell_cert();
        let store = "[\\Microsoft\\SystemCertificates\\ROOT\\Certificates]\n\n";
        let entry = |thumbprint: &str, blob: &[u8]| {
            format!("[\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\{thumbprint}]\n\"Blob\"=hex(3):{}\n\n", hex(blob))
        };
        // Someone else's root: a certificate whose names aren't ours.
        let mut theirs = cert.blob();
        let at = theirs.windows(18).position(|w| w == b"defenestrate shell").unwrap();
        theirs[at] = b'D';
        let other = entry("0000000000000000000000000000000000000000", &theirs);
        let current = entry(&cert.thumbprint(), &cert.blob());
        let earlier = entry("1111111111111111111111111111111111111111", &cert.blob());

        assert_eq!(Trust::from_export(&cert, &format!("{store}{other}")), Trust { add: true, remove: vec![] });
        assert_eq!(Trust::from_export(&cert, &format!("{store}{other}{current}")), Trust { add: false, remove: vec![] });
        assert_eq!(
            Trust::from_export(&cert, &format!("{store}{earlier}{other}")),
            Trust { add: true, remove: vec!["1111111111111111111111111111111111111111".into()] }
        );
    }

    #[test]
    fn root_store_reg() {
        let cert = shell_cert();
        let add = trust_text(Some(&cert), &[]);
        let key = format!("[HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\{}]", cert.thumbprint());
        assert!(add.contains(&format!("{key}\r\n\"Blob\"=hex:{}\r\n", hex(&cert.blob()))), "{add}");
        let remove = trust_text(None, &["AB".into()]);
        assert!(remove.ends_with("[-HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\SystemCertificates\\ROOT\\Certificates\\AB]\r\n\r\n"), "{remove}");
        assert!(!remove.contains("Blob"));
    }

    #[test]
    fn creates_only_what_may_be_created() {
        let d = tmp("create");
        std::fs::create_dir_all(d.join("c/pf")).unwrap();
        std::fs::write(d.join("new"), "uiaccess").unwrap();
        let mut f = InstallFile::new("pf/uiaccess.exe", d.join("new"), ReadAt::EveryLogon);
        // Not marked: like any other file the install lacks.
        assert_eq!(change(&f, &d.join("c")).unwrap(), Change::Missing);
        f.if_missing = IfMissing::CreateLike("pf/launcher.exe".into());
        // Nothing to take the descriptor from: an install too old, or not ours.
        assert_eq!(change(&f, &d.join("c")).unwrap(), Change::Missing);
        std::fs::write(d.join("c/pf/launcher.exe"), "launcher").unwrap();
        assert_eq!(change(&f, &d.join("c")).unwrap(), Change::Create);

        // Off NTFS there's no descriptor to copy, which must leave no file
        // behind rather than one with whatever the filesystem gives it.
        let dest = d.join("c/pf/uiaccess.exe");
        assert!(create_like(&d.join("new"), &dest, &d.join("c/pf/launcher.exe")).is_err());
        assert!(!dest.exists());
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
