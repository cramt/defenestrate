//! Windows apps and GPU drivers: winget package IDs and a Windows Update
//! pass, installed at the machine's first real boot by a logon task that
//! first logon registers. Under C:\Windows\Setup\defenestrate\apps:
//!   apps.json          what to install (see [`Plan`])
//!   install-apps.ps1   what the task runs
//!   task.xml           the task
//!   limited-task.xml   the same script unelevated, on demand, which the
//!                      task registers and runs for an installer that
//!                      failed elevated

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::{AppId, Config};
use crate::render::{IfMissing, InstallFile, Payload, ReadAt, SETUP_DIR};
use crate::tasks::{self, AsUser, Logon, RunLevel, Task, Trigger};
use crate::unattend::FirstLogon;

pub const TASK: &str = "defenestrate-apps";

/// What install-apps.ps1 reads.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Plan<'a> {
    apps: &'a [AppId],
    windows_update_display_drivers: bool,
}

pub fn needed(config: &Config) -> bool {
    !config.apps.is_empty() || config.gpu.windows_update
}

fn plan(config: &Config) -> Plan<'_> {
    Plan { apps: &config.apps, windows_update_display_drivers: config.gpu.windows_update }
}

pub fn render(config: &Config, dir: &Path) -> Result<Payload> {
    if !needed(config) {
        return Ok(Payload::default());
    }
    let out = dir.join("apps");
    std::fs::create_dir_all(&out)?;
    let on_c = SETUP_DIR.join("apps");
    let script = on_c.join("install-apps.ps1");
    let mut files = Vec::new();
    let mut write = |name: &str, data: &[u8], read: ReadAt| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        files.push(InstallFile::new(on_c.join(name), out.join(name), read));
        Ok(())
    };
    write("apps.json", serde_json::to_string_pretty(&plan(config))?.as_bytes(), ReadAt::EveryLogon)?;
    write("install-apps.ps1", include_bytes!("../assets/install-apps.ps1"), ReadAt::EveryLogon)?;
    let user = config.user.name.as_str();
    write(
        "task.xml",
        &tasks::xml(&Task {
            description: "Installs the configured winget apps and GPU drivers, then deletes itself. Log: %LOCALAPPDATA%\\defenestrate\\apps.log",
            // The user's own session, elevated: machine-wide installers then
            // run without a UAC prompt each, and per-user ones still land in
            // the user's profile, which they wouldn't as SYSTEM (winget
            // doesn't run as SYSTEM anyway). The cost: while an installer
            // window is focused, the shell's keyboard hook gets no keys.
            trigger: Trigger::Logon(AsUser { user, logon: Logon::Interactive, level: RunLevel::Elevated }),
            script: &script,
            script_args: "",
        })?,
        ReadAt::Setup,
    )?;
    // Registered by install-apps.ps1 itself (schtasks /create /f), not at
    // first logon, so an install `dfnstrate update` brings it to gets it too.
    write(
        "limited-task.xml",
        &tasks::xml(&Task {
            description: "Installs one winget app unelevated, for defenestrate-apps: some per-user installers crash elevated (Discord's).",
            trigger: Trigger::OnDemand(AsUser { user, logon: Logon::Interactive, level: RunLevel::Limited }),
            script: &script,
            script_args: "-Limited",
        })?,
        ReadAt::EveryLogon,
    )?;
    // Since the script registers it, it needs nothing but to exist, so
    // `update` can create it on an install from before it was added.
    files.last_mut().expect("just written").if_missing = IfMissing::CreateLike(script);
    let first_logon = vec![FirstLogon::register_task(TASK, &on_c.join("task.xml"), "Install the apps at the next logon")];
    Ok(Payload { files, first_logon })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_knows_the_tasks_by_their_names() {
        let script = include_str!("../assets/install-apps.ps1");
        // The file render() writes, which the script registers itself.
        assert!(script.contains("(Join-Path $here 'limited-task.xml')"));
        assert!(script.contains(&format!("$taskName = '{TASK}'")));
    }

    #[test]
    fn plan_carries_apps_and_the_driver_pass() {
        let mut t: toml::Table = toml::from_str(crate::config::DEFAULTS).unwrap();
        let machine = "apps = [\"Valve.Steam\"]\n[user]\nname = \"u\"";
        for (k, v) in toml::from_str::<toml::Table>(machine).unwrap() {
            match (t.get_mut(&k), v) {
                (Some(toml::Value::Table(into)), toml::Value::Table(from)) => into.extend(from),
                (_, v) => drop(t.insert(k, v)),
            }
        }
        let config: Config = toml::Value::Table(t).try_into().unwrap();
        assert!(needed(&config));
        let json = serde_json::to_value(plan(&config)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "apps": ["Valve.Steam"],
                "windowsUpdateDisplayDrivers": true,
            })
        );
    }
}
