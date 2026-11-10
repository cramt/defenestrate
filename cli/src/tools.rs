//! The programs dfnstrate drives. Each command checks for all of its tools
//! before doing anything, so a missing one fails in a second instead of
//! twenty minutes into a build. The Nix package puts them all on PATH.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy)]
pub struct Tool {
    /// Executable names to try, in order.
    pub names: &'static [&'static str],
    /// Package providing it (Arch / most distros).
    pub package: &'static str,
}

pub const SEVENZIP: Tool = Tool { names: &["7zz", "7z"], package: "7zip" };
pub const WIMLIB: Tool = Tool { names: &["wimlib-imagex"], package: "wimlib" };
pub const HIVEXREGEDIT: Tool = Tool { names: &["hivexregedit"], package: "hivex" };
pub const XORRISO: Tool = Tool { names: &["xorriso"], package: "libisoburn / xorriso" };
pub const QEMU: Tool = Tool { names: &["qemu-system-x86_64"], package: "qemu-system-x86 / qemu-full" };
pub const QEMU_IMG: Tool = Tool { names: &["qemu-img"], package: "qemu-img / qemu-full" };
pub const SWTPM: Tool = Tool { names: &["swtpm"], package: "swtpm" };
pub const SFDISK: Tool = Tool { names: &["sfdisk"], package: "util-linux" };
pub const MDIR: Tool = Tool { names: &["mdir"], package: "mtools" };
pub const IFIND: Tool = Tool { names: &["ifind"], package: "sleuthkit" };
pub const ICAT: Tool = Tool { names: &["icat"], package: "sleuthkit" };
pub const LSBLK: Tool = Tool { names: &["lsblk"], package: "util-linux" };
pub const FINDMNT: Tool = Tool { names: &["findmnt"], package: "util-linux" };
pub const BLKDISCARD: Tool = Tool { names: &["blkdiscard"], package: "util-linux" };
pub const PARTX: Tool = Tool { names: &["partx"], package: "util-linux" };
pub const BLOCKDEV: Tool = Tool { names: &["blockdev"], package: "util-linux" };
pub const UDEVADM: Tool = Tool { names: &["udevadm"], package: "systemd" };
pub const NTFSRESIZE: Tool = Tool { names: &["ntfsresize"], package: "ntfs-3g" };
pub const NTFS3G: Tool = Tool { names: &["ntfs-3g"], package: "ntfs-3g" };
pub const NTFS3G_PROBE: Tool = Tool { names: &["ntfs-3g.probe"], package: "ntfs-3g" };
pub const EFIBOOTMGR: Tool = Tool { names: &["efibootmgr"], package: "efibootmgr" };
pub const SUDO: Tool = Tool { names: &["sudo"], package: "sudo" };

impl Tool {
    pub fn path(&self) -> Option<PathBuf> {
        self.names.iter().find_map(which)
    }

    pub fn cmd(&self) -> Command {
        Command::new(self.path().unwrap_or_else(|| PathBuf::from(self.names[0])))
    }
}

pub fn which(name: impl AsRef<OsStr>) -> Option<PathBuf> {
    let name = name.as_ref();
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Fails listing every missing tool and what provides it.
pub fn require(tools: &[Tool]) -> Result<()> {
    let missing: Vec<String> = tools.iter().filter(|t| t.path().is_none()).map(|t| format!("  {} (package: {})", t.names.join(" or "), t.package)).collect();
    if !missing.is_empty() {
        bail!("missing tools, install them and retry:\n{}", missing.join("\n"));
    }
    Ok(())
}

fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
}

/// Runs to completion with stdout discarded; stderr shows through.
pub fn run(cmd: &mut Command) -> Result<()> {
    let what = describe(cmd);
    let status = cmd.stdout(Stdio::null()).status().with_context(|| format!("running {what}"))?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// Runs with `input` on stdin, stdout discarded.
pub fn run_with_input(cmd: &mut Command, input: &str) -> Result<()> {
    use std::io::Write;
    let what = describe(cmd);
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).spawn().with_context(|| format!("running {what}"))?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// Runs and returns stdout.
pub fn output(cmd: &mut Command) -> Result<Vec<u8>> {
    let what = describe(cmd);
    let out = cmd.stderr(Stdio::inherit()).output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        bail!("{what} failed ({})", out.status);
    }
    Ok(out.stdout)
}

pub fn output_string(cmd: &mut Command) -> Result<String> {
    String::from_utf8(output(cmd)?).context("non-UTF-8 output")
}

/// Marks a file or directory No_COW (chattr +C), so a disk image being
/// rewritten in place doesn't fragment badly on btrfs. Only takes on empty
/// files and on what's created in a directory afterwards; a no-op
/// elsewhere, so errors are ignored.
pub fn no_cow(path: &std::path::Path) {
    use std::os::fd::AsRawFd;
    const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
    const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;
    // The kernel reads and writes these flags as an int, whatever the ioctl
    // number claims.
    const FS_NOCOW_FL: libc::c_int = 0x0080_0000;
    let Ok(f) = std::fs::File::open(path) else { return };
    let mut flags: libc::c_int = 0;
    // SAFETY: both ioctls read/write one int through the pointer given.
    unsafe {
        if libc::ioctl(f.as_raw_fd(), FS_IOC_GETFLAGS, &mut flags) == 0 {
            flags |= FS_NOCOW_FL;
            libc::ioctl(f.as_raw_fd(), FS_IOC_SETFLAGS, &flags);
        }
    }
}
