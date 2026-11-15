//! Windows' side of booting Linux, both driven by `linuxBootEntry`:
//!
//! - Linux stays the firmware's default: a task at every startup puts the
//!   Linux entry first in UEFI BootOrder ({fwbootmgr}'s displayorder).
//!   Windows' first boot puts a Windows Boot Manager entry of its own first,
//!   and boot manager updates can again later.
//! - "Reboot to Linux": a Start Menu shortcut that boots the Linux entry
//!   once (UEFI BootNext, {fwbootmgr}'s bootsequence) and reboots, so it
//!   lands in Linux even when the startup task left BootOrder alone (it
//!   refuses to choose between two candidates). The mirror of
//!   `dfnstrate reboot` on the Linux side.
//!
//! Setting BootNext takes admin, and the shell isn't elevated. So first
//! logon registers an elevated task with no trigger, which its own user may
//! start unelevated without a UAC prompt, and the shortcut starts it. Under
//! C:\Windows\Setup\defenestrate\reboot:
//!   linux-entry.ps1      finds the Linux entry; dot-sourced by both scripts
//!   reboot.json          which entry (`linuxBootEntry`)
//!   linux-first.ps1      run at startup: Linux first in BootOrder
//!   linux-first.xml      its task, registered at first logon
//!   reboot-to-linux.ps1  what the shortcut's task runs: sets BootNext, reboots
//!   task.xml             that task
//!   register.ps1         run at first logon: registers it, makes the shortcut

use std::path::Path;

use anyhow::Result;

use crate::config::{Config, LinuxBootEntry};
use crate::render::{IfMissing, InstallFile, ReadAt};
use crate::tasks::{self, AsUser, Logon, Task, Trigger};

pub const DIR: &str = "Windows/Setup/defenestrate/reboot";
pub const TASK: &str = "defenestrate-reboot-to-linux";
pub const LINUX_FIRST_TASK: &str = "defenestrate-linux-first";

/// Whether there's anything to register at first logon: both tasks or
/// neither.
pub fn enabled(config: &Config) -> bool {
    config.linux_boot_entry != LinuxBootEntry::None
}

pub fn render(config: &Config, dir: &Path) -> Result<Vec<InstallFile>> {
    let description = match &config.linux_boot_entry {
        LinuxBootEntry::None => return Ok(Vec::new()),
        LinuxBootEntry::Auto => None,
        LinuxBootEntry::Description(d) => Some(d.as_str()),
    };
    let out = dir.join("reboot");
    std::fs::create_dir_all(&out)?;
    let mut files = Vec::new();
    let mut write = |name: &str, data: &[u8], read: ReadAt| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        files.push(InstallFile::new(format!("{DIR}/{name}"), out.join(name), read));
        Ok(())
    };
    // Read at each run, so `update` reaches them like the per-logon files.
    write("reboot.json", serde_json::to_string(&serde_json::json!({ "description": description }))?.as_bytes(), ReadAt::EveryLogon)?;
    write("reboot-to-linux.ps1", include_bytes!("../assets/reboot-to-linux.ps1"), ReadAt::EveryLogon)?;
    write("linux-entry.ps1", include_bytes!("../assets/linux-entry.ps1"), ReadAt::EveryLogon)?;
    write("linux-first.ps1", include_bytes!("../assets/linux-first.ps1"), ReadAt::EveryLogon)?;
    write("register.ps1", include_bytes!("../assets/reboot-register.ps1"), ReadAt::Setup)?;
    let user = config.user.name.as_str();
    write(
        "task.xml",
        &tasks::xml(&Task {
            description: "Boots the firmware's Linux entry once and reboots; started by the Start Menu's \"Reboot to Linux\". Log: %LOCALAPPDATA%\\defenestrate\\reboot.log",
            // Session 0: nothing to see, and no console window flashing up
            // over the game the user is quitting.
            trigger: Trigger::OnDemand(AsUser { user, logon: Logon::Session0 }),
            command: "powershell.exe",
            arguments: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\Windows\\Setup\\defenestrate\\reboot\\reboot-to-linux.ps1".into(),
        })?,
        ReadAt::Setup,
    )?;
    write(
        "linux-first.xml",
        &tasks::xml(&Task {
            description: "Puts the firmware's Linux entry first in the boot order, so Linux stays the default. Log: %ProgramData%\\defenestrate\\linux-first.log",
            // Startup rather than logon: the order matters at the next boot
            // whether or not anyone logs on in this one. Startup tasks don't
            // run when Fast Startup resumes instead of booting; default.toml
            // turns it off.
            trigger: Trigger::Startup,
            command: "powershell.exe",
            arguments: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\Windows\\Setup\\defenestrate\\reboot\\linux-first.ps1".into(),
        })?,
        ReadAt::Setup,
    )?;
    // `update` rewrites reboot-to-linux.ps1 on an install from before it
    // dot-sourced linux-entry.ps1, so it must be able to add that too; it
    // needs nothing but to exist.
    let entry = files.iter_mut().find(|f| f.dest.ends_with("/linux-entry.ps1")).expect("written above");
    entry.if_missing = IfMissing::CreateLike(format!("{DIR}/reboot-to-linux.ps1"));
    Ok(files)
}
