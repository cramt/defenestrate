//! Windows apps and GPU drivers: winget package IDs and a Windows Update
//! pass, installed at the machine's first real boot by a logon task that
//! first logon registers. Under C:\Windows\Setup\defenestrate\apps:
//!   apps.json          what to install (see [`Plan`])
//!   install-apps.ps1   what the task runs
//!   task.xml           the task

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::{AppId, Config};
use crate::render::{InstallFile, ReadAt};
use crate::tasks::{self, LogonTask};

pub const DIR: &str = "Windows/Setup/defenestrate/apps";
pub const TASK: &str = "defenestrate-apps";

/// What install-apps.ps1 reads. The GPU half is decided there, since only
/// the real machine knows its GPUs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Plan<'a> {
    apps: &'a [AppId],
    windows_update_display_drivers: bool,
    gpu_vendors: Vec<VendorApps<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VendorApps<'a> {
    /// Matched against `PCI\VEN_xxxx` in device IDs.
    pci_vendor: &'static str,
    apps: &'a [AppId],
}

pub fn needed(config: &Config) -> bool {
    !config.apps.is_empty() || config.gpu.installs_anything()
}

fn plan(config: &Config) -> Plan<'_> {
    Plan {
        apps: &config.apps,
        windows_update_display_drivers: config.gpu.windows_update,
        gpu_vendors: config
            .gpu
            .vendor_apps()
            .into_iter()
            .filter(|(_, apps)| !apps.is_empty())
            .map(|(vendor, apps)| VendorApps { pci_vendor: vendor.pci_id(), apps })
            .collect(),
    }
}

pub fn render(config: &Config, dir: &Path) -> Result<Vec<InstallFile>> {
    if !needed(config) {
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
    write("apps.json", serde_json::to_string_pretty(&plan(config))?.as_bytes(), ReadAt::EveryLogon)?;
    write("install-apps.ps1", include_bytes!("../assets/install-apps.ps1"), ReadAt::EveryLogon)?;
    let user = config.user.name.as_str();
    write(
        "task.xml",
        &tasks::xml(&LogonTask {
            description: "Installs the configured winget apps and GPU drivers, then deletes itself. Log: %LOCALAPPDATA%\\defenestrate\\apps.log",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_keeps_only_vendors_with_apps() {
        let mut t: toml::Table = toml::from_str(crate::config::DEFAULTS).unwrap();
        let machine = "apps = [\"Valve.Steam\"]\n[user]\nname = \"u\"\n[gpu]\nnvidia = [\"A.B\"]\nintel = [\"C.D\"]";
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
                "gpuVendors": [{ "pciVendor": "10DE", "apps": ["A.B"] }, { "pciVendor": "8086", "apps": ["C.D"] }],
            })
        );
    }
}
