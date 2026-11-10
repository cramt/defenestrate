//! Windows apps: winget package IDs, installed at the machine's first real
//! boot by a logon task that first logon registers. Under
//! C:\Windows\Setup\defenestrate\apps:
//!   apps.json          the IDs
//!   install-apps.ps1   what the task runs
//!   task.xml           the task

use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::render::{InstallFile, ReadAt};
use crate::tasks::{self, LogonTask};

pub const DIR: &str = "Windows/Setup/defenestrate/apps";
pub const TASK: &str = "defenestrate-apps";

pub fn render(config: &Config, dir: &Path) -> Result<Vec<InstallFile>> {
    if config.apps.is_empty() {
        return Ok(Vec::new());
    }
    let out = dir.join("apps");
    std::fs::create_dir_all(&out)?;
    let mut files = Vec::new();
    let mut write = |name: &str, data: &[u8], read: ReadAt| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        files.push(InstallFile::new(format!("{DIR}/{name}"), out.join(name), read));
        Ok(())
    };
    let ids: Vec<&str> = config.apps.iter().map(|a| a.as_str()).collect();
    write("apps.json", serde_json::to_string(&ids)?.as_bytes(), ReadAt::EveryLogon)?;
    write("install-apps.ps1", include_bytes!("../assets/install-apps.ps1"), ReadAt::EveryLogon)?;
    let user = config.user.name.as_str();
    write(
        "task.xml",
        &tasks::xml(&LogonTask {
            description: "Installs the configured winget apps, then deletes itself. Log: %LOCALAPPDATA%\\defenestrate\\apps.log",
            user,
            // The user's own session, elevated: machine-wide installers then
            // run without a UAC prompt each, and per-user ones still land in
            // the user's profile, which they wouldn't as SYSTEM (winget
            // doesn't run as SYSTEM anyway). The cost: while an installer
            // window is focused, the shell's keyboard hook gets no keys.
            principal: format!("<UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel>"),
            command: "powershell.exe",
            arguments: "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\Windows\\Setup\\defenestrate\\apps\\install-apps.ps1".into(),
        })?,
        ReadAt::Setup,
    )?;
    Ok(files)
}
