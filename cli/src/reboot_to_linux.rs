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
use crate::render::{IfMissing, InstallFile, Payload, ReadAt, SETUP_DIR};
use crate::tasks::{self, AsUser, Logon, RunLevel, Task, Trigger};
use crate::unattend::FirstLogon;

pub const TASK: &str = "defenestrate-reboot-to-linux";
pub const LINUX_FIRST_TASK: &str = "defenestrate-linux-first";

pub fn render(config: &Config, dir: &Path) -> Result<Payload> {
    let description = match &config.linux_boot_entry {
        LinuxBootEntry::None => return Ok(Payload::default()),
        LinuxBootEntry::Auto => None,
        LinuxBootEntry::Description(d) => Some(d.as_str()),
    };
    let out = dir.join("reboot");
    std::fs::create_dir_all(&out)?;
    let on_c = SETUP_DIR.join("reboot");
    let mut files = Vec::new();
    let mut write = |name: &str, data: &[u8], read: ReadAt| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        files.push(InstallFile::new(on_c.join(name), out.join(name), read));
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
            trigger: Trigger::OnDemand(AsUser { user, logon: Logon::Session0, level: RunLevel::Elevated }),
            script: &on_c.join("reboot-to-linux.ps1"),
            script_args: "",
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
            script: &on_c.join("linux-first.ps1"),
            script_args: "",
        })?,
        ReadAt::Setup,
    )?;
    // `update` rewrites reboot-to-linux.ps1 on an install from before it
    // dot-sourced linux-entry.ps1, so it must be able to add that too; it
    // needs nothing but to exist.
    let entry = on_c.join("linux-entry.ps1");
    let entry = files.iter_mut().find(|f| f.dest == entry).expect("written above");
    entry.if_missing = IfMissing::CreateLike(on_c.join("reboot-to-linux.ps1"));
    let first_logon = vec![
        FirstLogon::powershell(&on_c.join("register.ps1"), &format!("-Task {TASK}"), "Register Reboot to Linux"),
        FirstLogon::register_task(LINUX_FIRST_TASK, &on_c.join("linux-first.xml"), "Keep Linux first in the boot order"),
    ];
    Ok(Payload { files, first_logon })
}
