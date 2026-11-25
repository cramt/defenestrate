//! The installed Windows partition from Linux, through ntfs-3g: what
//! `update` writes to and `logs` reads from while Windows is shut down.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::disk;
use crate::tools::{self, FINDMNT, LSBLK, NTFS3G, NTFS3G_PROBE};

/// Which Windows volume a command works on.
#[derive(clap::Args)]
pub struct VolumeArgs {
    /// Whole disk Windows is installed on, by /dev/disk/by-id/ [default: the config's installTarget]
    target: Option<String>,
    /// Use the NTFS volume in this file instead of a disk's Windows
    /// partition, skipping every disk check. For tests.
    #[arg(long, hide = true, value_name = "FILE", conflicts_with = "target")]
    volume_image: Option<PathBuf>,
}

impl VolumeArgs {
    /// The volume to mount. A disk's partition is checked as the caller
    /// first; then `before_sudo` runs, returning arguments to add, and the
    /// command runs again as root, which checks it all again and returns.
    pub fn volume(&self, config: &Config, usage: &str, before_sudo: impl FnOnce() -> Result<Vec<OsString>>) -> Result<PathBuf> {
        if let Some(file) = &self.volume_image {
            return Ok(file.clone());
        }
        tools::require(&[LSBLK, FINDMNT])?;
        let disk = disk::unused(disk::target(self.target.as_deref(), config.install_target.clone(), usage)?)?;
        let part = disk::windows_partition(&disk)?;
        let fs_type = tools::output_string(LSBLK.cmd().args(["-no", "FSTYPE"]).arg(&part))?;
        if fs_type.trim() != "ntfs" {
            bail!("{} is {:?}, not NTFS; is Windows on this disk?", part.display(), fs_type.trim());
        }
        if !disk::is_root() {
            let mut extra = before_sudo()?;
            if self.target.is_none() {
                extra.insert(0, disk.target.as_str().into());
            }
            disk::rerun_as_root(extra)?;
        }
        Ok(part)
    }
}

/// ntfs-3g mounts a volume it won't write read-only without failing, so
/// whether it would write is asked first, for a message that says why not.
pub fn probe(volume: &Path, write: bool) -> Result<()> {
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
pub struct Mount {
    pub dir: PathBuf,
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
    pub fn new(volume: &Path, dir: PathBuf, write: bool) -> Result<Self> {
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

    pub fn unmount(mut self) -> Result<()> {
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

/// ntfs-3g's view of a file's whole NTFS security descriptor.
const NTFS_ACL: &str = "system.ntfs_acl";

/// A file's NTFS security descriptor, on a volume ntfs-3g mounted.
pub fn descriptor(path: &Path) -> Result<Vec<u8>> {
    let (p, n) = (std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?, std::ffi::CString::new(NTFS_ACL)?);
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

pub fn set_descriptor(path: &Path, value: &[u8]) -> Result<()> {
    let (p, n) = (std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?, std::ffi::CString::new(NTFS_ACL)?);
    // SAFETY: p and n are valid NUL-terminated strings and value is read for
    // value.len() bytes.
    if unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
