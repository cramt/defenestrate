//! What `flash` and `update` share: finding the disk Windows is on and
//! refusing one that's in use, and getting root only once that's settled.

use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::ByIdPath;
use crate::tools::{self, FINDMNT, LSBLK, SUDO};

/// Windows' partition on a `build image` disk: ESP, MSR, then Windows, last
/// so `flash` can grow it.
pub const WINDOWS_PART: u32 = 3;

/// A whole disk nothing on this machine is using.
pub struct Disk {
    /// The by-id path it was named by.
    pub target: ByIdPath,
    /// Its /dev node.
    pub dev: PathBuf,
    /// Its /sys/block directory.
    pub sys: PathBuf,
}

impl Disk {
    /// One of its partitions, by the by-id name udev gives it.
    pub fn partition(&self, n: u32) -> String {
        format!("{}-part{n}", self.target.as_str())
    }
}

/// The target given on the command line, else the config's installTarget.
pub fn target(given: Option<&str>, install_target: Option<ByIdPath>, usage: &str) -> Result<ByIdPath> {
    match given {
        Some(t) => ByIdPath::try_from(t.to_owned()).map_err(anyhow::Error::msg),
        None => install_target.with_context(|| format!("no target given and the config has no installTarget\nusage: {usage}")),
    }
}

/// Refuses a partition, anything that isn't a whole disk, and anything in
/// use.
pub fn unused(target: ByIdPath) -> Result<Disk> {
    let t = target.as_str();
    if t.contains("-part") {
        bail!("{t} is a partition; name the whole disk. Windows has all of it, its own ESP included.");
    }
    let meta = std::fs::metadata(t).with_context(|| t.to_owned())?;
    if !meta.file_type().is_block_device() {
        bail!("{t} is not a block device");
    }
    let dev = std::fs::canonicalize(t)?;
    let lsblk = |args: &[&str]| tools::output_string(LSBLK.cmd().args(args).arg(&dev));
    if lsblk(&["-dno", "TYPE"])?.trim() != "disk" {
        bail!("{} is not a whole disk", dev.display());
    }

    // Mountpoints alone miss a multi-device btrfs, which shows its mount on
    // one member only, so filesystem UUIDs are matched against everything
    // mounted too. Holders catch md, LVM and dm.
    let mounts: Vec<String> = lsblk(&["-rno", "MOUNTPOINTS"])?.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
    if !mounts.is_empty() {
        bail!("{} has mounted filesystems: {}", dev.display(), mounts.join(" "));
    }
    let mounted = tools::output_string(FINDMNT.cmd().arg("-rno").arg("UUID"))?;
    let mounted: Vec<&str> = mounted.lines().filter(|l| !l.is_empty()).collect();
    for uuid in lsblk(&["-rno", "UUID"])?.lines().filter(|l| !l.is_empty()) {
        if mounted.contains(&uuid) {
            bail!("{} holds part of the mounted filesystem {uuid}", dev.display());
        }
    }
    let name = dev.file_name().context("device has no name")?.to_string_lossy().into_owned();
    let sys = Path::new("/sys/block").join(&name);
    let mut holder_dirs = vec![sys.join("holders")];
    for e in std::fs::read_dir(&sys)? {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with(&name) {
            holder_dirs.push(e.path().join("holders"));
        }
    }
    for dir in holder_dirs {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let holders: Vec<String> = entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            if !holders.is_empty() {
                bail!("{} is in use by {}", dev.display(), holders.join(" "));
            }
        }
    }
    Ok(Disk { target, dev, sys })
}

pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Runs this same command again under sudo, with `extra` arguments after
/// the caller's. Everything up to here is checked as the caller, so a
/// mistake fails before the password prompt; then it's all checked again as
/// root.
pub fn rerun_as_root(extra: impl IntoIterator<Item = OsString>) -> Result<std::convert::Infallible> {
    tools::require(&[SUDO])?;
    let mut sudo = SUDO.cmd();
    sudo.arg("--").arg(std::env::current_exe()?).args(std::env::args_os().skip(1)).args(extra);
    Err(sudo.exec()).context("re-running under sudo")
}
