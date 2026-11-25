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

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cache::Cache;
use crate::cert::{self, ShellCert};
use crate::config::Config;
use crate::ntfs::{self, Mount, VolumeArgs};
use crate::registry::{self, Root};
use crate::render::{self, IfMissing, InstallFile, ReadAt, Rendered, SETUP_DIR};
use crate::tools::{self, HIVEXREGEDIT, NTFS3G, NTFS3G_PROBE};
use crate::unattend::Variant;

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    volume: VolumeArgs,
    /// List what would change, writing nothing.
    #[arg(long)]
    dry_run: bool,
}

const USAGE: &str = "dfnstrate update -c CONFIG... /dev/disk/by-id/<the disk Windows is on>";

pub fn run(config: &Config, cache: &Cache, args: &Args) -> Result<()> {
    tools::require(&[NTFS3G, NTFS3G_PROBE, HIVEXREGEDIT])?;
    let volume = args.volume.volume(config, USAGE, || {
        // Rendered as the caller first: that fails early on a bad config, and
        // downloads into the caller's cache, which root then reads offline
        // rather than filling its own.
        Scratch::new("render")?.render(config, cache)?;
        Ok(vec!["--cache".into(), std::path::absolute(&cache.dir)?.into(), "--offline".into()])
    })?;

    let scratch = Scratch::new("render")?;
    let Rendered { files, shell_cert, .. } = scratch.render(config, cache)?;
    ntfs::probe(&volume, !args.dry_run)?;
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

/// Creates `dest` with `like`'s security descriptor, then writes `source`
/// into it. If the descriptor can't be set, `dest` is removed again rather
/// than left with ntfs-3g's own (Everyone: full control).
fn create_like(source: &Path, dest: &Path, like: &Path) -> Result<()> {
    let descriptor = ntfs::descriptor(like).with_context(|| format!("reading {}'s security descriptor", like.display()))?;
    File::options().write(true).create_new(true).open(dest)?;
    if let Err(e) = ntfs::set_descriptor(dest, &descriptor) {
        std::fs::remove_file(dest).ok();
        return Err(e).context("setting its security descriptor");
    }
    write_in_place(source, dest)
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
