//! "Reboot to Linux": a Start Menu shortcut that boots the firmware's Linux
//! entry once (UEFI BootNext, which Windows calls {fwbootmgr}'s
//! bootsequence) and reboots. A plain reboot usually lands in Linux anyway,
//! but Windows, major updates especially, puts itself first in BootOrder
//! now and then. The mirror of `dfnstrate reboot` on the Linux side.
//!
//! Setting BootNext takes admin, and the shell isn't elevated. So first
//! logon registers an elevated task with no trigger, which its own user may
//! start unelevated without a UAC prompt, and the shortcut starts it. Under
//! C:\Windows\Setup\defenestrate\reboot:
//!   reboot-to-linux.ps1  what the task runs: picks the entry, sets it, reboots
//!   reboot.json          which entry (`linuxBootEntry`)
//!   task.xml             the task
//!   register.ps1         run at first logon: registers it, makes the shortcut

use std::path::Path;

use anyhow::Result;

use crate::config::{Config, LinuxBootEntry};
use crate::render::{InstallFile, ReadAt};
use crate::tasks::{self, Logon, Task, Trigger};

pub const DIR: &str = "Windows/Setup/defenestrate/reboot";
pub const TASK: &str = "defenestrate-reboot-to-linux";

/// Whether there's anything to register at first logon.
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
    write("register.ps1", include_bytes!("../assets/reboot-register.ps1"), ReadAt::Setup)?;
    let user = config.user.name.as_str();
    write(
        "task.xml",
        &tasks::xml(&Task {
            description: "Boots the firmware's Linux entry once and reboots; started by the Start Menu's \"Reboot to Linux\". Log: %LOCALAPPDATA%\\defenestrate\\reboot.log",
            user,
            trigger: Trigger::OnDemand,
            // Session 0: nothing to see, and no console window flashing up
            // over the game the user is quitting.
            logon: Logon::Session0,
            command: "powershell.exe",
            arguments: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\Windows\\Setup\\defenestrate\\reboot\\reboot-to-linux.ps1".into(),
        })?,
        ReadAt::Setup,
    )?;
    Ok(files)
}
