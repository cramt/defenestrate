//! WSL on the Windows side, and the small distro that reaches the machine's
//! Linux install. What goes into the image, under
//! C:\Windows\Setup\defenestrate\wsl:
//!   wsl.msi          installed at first logon (unattend.rs)
//!   wsl-install.cmd  which installs it, and registers the task below
//! and with wsl.linux set:
//!   rootfs.tar       the defenestrate distro: busybox, dfn, the mount list;
//!                    re-imported at the next logon whenever it changes
//!   wsl-logon.ps1    run at every logon by the defenestrate-wsl task
//!   wsl.json         the disks it attaches
//!   task.xml         that task

use std::path::Path;

use anyhow::Result;

use crate::cache::Cache;
use crate::config::{Config, FileSource, Linux};
use crate::render::{InstallFile, ReadAt};
use crate::tasks::{self, LogonTask};

/// Relative to C:.
pub const DIR: &str = "Windows/Setup/defenestrate/wsl";
pub const TASK: &str = "defenestrate-wsl";

/// Writes the payload to `dir`/wsl, except the MSI, which stays in the
/// cache (it's 370 MB).
pub fn render(config: &Config, cache: &Cache, work: &Path, dir: &Path) -> Result<Vec<InstallFile>> {
    let wsl = &config.wsl;
    let out = dir.join("wsl");
    std::fs::create_dir_all(&out)?;
    let mut files = vec![InstallFile::new(format!("{DIR}/wsl.msi"), cache.materialize(&FileSource::Fetched(wsl.package.clone()), work)?, ReadAt::Setup)];
    let mut write = |name: &str, data: &[u8], read: ReadAt| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        files.push(InstallFile::new(format!("{DIR}/{name}"), out.join(name), read));
        Ok(())
    };
    write("wsl-install.cmd", crate::render::crlf(include_str!("../assets/wsl-install.cmd")).as_bytes(), ReadAt::Setup)?;
    let Some(linux) = &wsl.linux else { return Ok(files) };

    let busybox = std::fs::read(cache.materialize(&FileSource::Fetched(wsl.busybox.clone()), work)?)?;
    write("rootfs.tar", &rootfs(&busybox, linux)?, ReadAt::EveryLogon)?;
    write("wsl-logon.ps1", include_bytes!("../assets/wsl-logon.ps1"), ReadAt::EveryLogon)?;
    let disks: Vec<&str> = linux.disks.iter().map(|g| g.as_str()).collect();
    write("wsl.json", serde_json::to_string(&serde_json::json!({ "disks": disks }))?.as_bytes(), ReadAt::EveryLogon)?;

    let user = config.user.name.as_str();
    // Registered once, at first logon.
    write(
        "task.xml",
        &tasks::xml(&LogonTask {
            description: "Attaches and mounts the Linux install in WSL, and lists its apps in the Start Menu. Log: %LOCALAPPDATA%\\defenestrate\\wsl.log",
            user,
            // Elevated, for `wsl --mount`, and S4U: in session 0, so no window
            // of it (elevated windows starve the shell's keyboard hook) and no
            // stored password.
            // Known cost: WSL started from session 0 puts WSLg's viewer there
            // too, so GUI apps launched while that VM lives are invisible
            // (github.com/microsoft/wslg/issues/1514). An InteractiveToken
            // task via `conhost --headless` avoids that and works when run by
            // hand, but failed at logon with 0x80070003. GUI apps are blocked
            // anyway under this shell (README), so this stays until both are
            // solved.
            principal: format!("<UserId>{user}</UserId><LogonType>S4U</LogonType><RunLevel>HighestAvailable</RunLevel>"),
            command: "powershell.exe",
            arguments: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\Windows\\Setup\\defenestrate\\wsl\\wsl-logon.ps1".into(),
        })?,
        ReadAt::Setup,
    )?;
    drop(write);
    Ok(files)
}

const APPLETS: &[&str] = &[
    "sh", "mount", "umount", "ls", "cat", "cp", "mv", "rm", "mkdir", "ln", "chroot", "ps", "dmesg", "grep", "sed", "awk", "head", "tail", "sleep", "env", "id",
    "blkid", "findfs", "modprobe", "lsmod", "df", "du", "find", "vi", "less",
];

/// The defenestrate distro's root filesystem, for `wsl --import`.
fn rootfs(busybox: &[u8], linux: &Linux) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |path: &str, mode: u32, data: &[u8]| -> std::io::Result<()> {
        let mut h = tar::Header::new_gnu();
        h.set_mode(mode);
        h.set_size(data.len() as u64);
        h.set_mtime(0);
        h.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut h, path, data)
    };
    add("bin/busybox", 0o755, busybox)?;
    add("defenestrate/dfn", 0o755, include_bytes!("../assets/dfn.sh"))?;
    add("defenestrate/linux.conf", 0o644, linux_conf(linux).as_bytes())?;
    add("etc/passwd", 0o644, b"root:x:0:0:root:/root:/bin/sh\n")?;
    add("etc/group", 0o644, b"root:x:0:\n")?;
    // The distro only ever runs dfn: no Windows PATH, no C: drive, no fstab.
    add("etc/wsl.conf", 0o644, b"[automount]\nenabled = false\nmountFsTab = false\n\n[interop]\nappendWindowsPath = false\n\n[user]\ndefault = root\n")?;
    drop(add);
    for dir in ["dev", "proc", "sys", "tmp", "run", "mnt", "root", "linux", "lib", "usr"] {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_mode(if dir == "tmp" { 0o1777 } else { 0o755 });
        h.set_size(0);
        h.set_mtime(0);
        tar.append_data(&mut h, format!("{dir}/"), std::io::empty())?;
    }
    // WSL runs commands through /bin/sh and calls mount and friends itself;
    // the rest is for poking around (`wsl -d defenestrate`).
    for applet in APPLETS {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_mode(0o777);
        h.set_size(0);
        h.set_mtime(0);
        tar.append_link(&mut h, format!("bin/{applet}"), "busybox")?;
    }
    Ok(tar.into_inner()?)
}

/// Sourced by dfn. Every value is checked to be quote-free when the config
/// loads, so single quotes are enough.
fn linux_conf(linux: &Linux) -> String {
    let mut s = format!("USER_NAME='{}'\nmounts() {{\n", linux.user);
    for m in &linux.mounts {
        let options: Vec<&str> = m.options.iter().map(|o| o.as_str()).collect();
        s += &format!("\tm '{}' '{}' '{}' '{}'\n", m.device, m.fs_type, options.join(","), m.target);
    }
    s + "}\n"
}
