//! Everything the first real boot needs to install the config's apps: the
//! installers from the lock, the plan, install-apps.ps1, and the scheduled
//! tasks that run it. Goes into the image under
//! C:\Windows\Setup\defenestrate\apps.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cache::Cache;
use crate::config::{AppId, Config, Fetched, FileSource, Sri};
use crate::lockfile::{Lock, LockedApp};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Baked,
    Winget,
    Msstore,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Baked => "baked",
            Self::Winget => "winget",
            Self::Msstore => "msstore",
        }
    }

    fn relock(self, id: &AppId) -> String {
        match self {
            Self::Baked => format!("dfnstrate lock {id}"),
            Self::Winget => format!("dfnstrate lock --winget {id}"),
            Self::Msstore => format!("dfnstrate lock --msstore {id}"),
        }
    }
}

pub struct Planned<'a> {
    id: &'a AppId,
    app: &'a LockedApp,
}

pub fn read_lock(config: &Config) -> Result<Option<Lock>> {
    let Some(path) = &config.apps_lock else { return Ok(None) };
    if !config.has_apps() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading appsLock {}", path.display()))?;
    Ok(Some(serde_json::from_str(&text).with_context(|| format!("{}", path.display()))?))
}

/// Each configured ID with its lock entry, which must be of the same kind.
/// Baked first, in config order; install-apps.ps1 keeps that order.
pub fn plan<'a>(config: &'a Config, lock: &'a Lock) -> Result<Vec<Planned<'a>>> {
    let lock_name = config.apps_lock.as_deref().map(Path::display);
    let lock_name = lock_name.map(|d| d.to_string()).unwrap_or_default();
    let mut out = Vec::new();
    for (kind, ids) in [(Kind::Baked, &config.apps), (Kind::Winget, &config.winget_apps), (Kind::Msstore, &config.store_apps)] {
        for id in ids {
            let app =
                lock.apps.get(id.as_str()).with_context(|| format!("{id} isn't in {lock_name}. Lock it with:\n  {} --lock {lock_name}", kind.relock(id)))?;
            let locked_as = match app {
                LockedApp::Baked(_) => Kind::Baked,
                LockedApp::Winget { .. } => Kind::Winget,
                LockedApp::Msstore {} => Kind::Msstore,
            };
            if locked_as != kind {
                bail!("{id} is configured as {} but locked as {}. Relock it with:\n  {} --lock {lock_name}", kind.name(), locked_as.name(), kind.relock(id));
            }
            out.push(Planned { id, app });
        }
    }
    Ok(out)
}

/// Writes the payload directory.
pub fn write_payload(config: &Config, plan: &[Planned], cache: &Cache, work: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out.join("installers"))?;
    let mut entries = Vec::new();
    for p in plan {
        let app = serde_json::to_value(p.app)?;
        let mut entry = app.as_object().cloned().context("lock entry isn't an object")?;
        entry.insert("id".into(), json!(p.id.as_str()));
        entries.push(serde_json::Value::Object(entry));

        if let LockedApp::Baked(b) = p.app {
            if b.file.contains(['/', '\\']) || b.file.starts_with('.') {
                bail!("{}: locked file name {:?} isn't a plain file name", p.id, b.file);
            }
            let hash = Sri::try_from(b.hash.clone()).map_err(anyhow::Error::msg)?;
            let src = cache.materialize(&FileSource::Fetched(Fetched { url: b.url.clone(), hash, member: None }), work)?;
            std::fs::copy(&src, out.join("installers").join(&b.file)).with_context(|| format!("copying {}", b.file))?;
        }
    }
    // serde_json's maps are sorted, like Nix's toJSON was.
    std::fs::write(out.join("apps.json"), serde_json::to_string(&entries)?)?;
    std::fs::write(out.join("install-apps.ps1"), include_str!("../assets/install-apps.ps1"))?;

    // Logon tasks rather than RunOnce entries: explorer is what runs those,
    // and explorer isn't the shell. Both run alongside the shell, without a
    // UAC prompt:
    // - SYSTEM, in session 0, for machine-wide installers. Not elevated in
    //   the user's session: a non-elevated keyboard hook gets no keys while
    //   an elevated window has focus, and the task's hidden console and
    //   installers held it, so the shell's keys were dead until they finished.
    // - The plain user (install-apps.ps1 -User) for per-user installers, which
    //   break when elevated (Discord's Squirrel Update.exe crashes with
    //   0xC0000005) and belong in the user's profile, and for winget, which
    //   doesn't run as SYSTEM.
    let user = config.user.name.as_str();
    // S-1-5-18 is LocalSystem.
    let system = task_xml(user, "<UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel>", "", "C:\\Windows\\Setup\\defenestrate\\apps.log");
    let per_user = task_xml(
        user,
        &format!("<UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>"),
        " -User",
        "%LOCALAPPDATA%\\defenestrate\\apps.log",
    );
    for (name, xml) in [("task.xml", system), ("task-user.xml", per_user)] {
        roxmltree::Document::parse(&xml.replace("encoding=\"UTF-16\"", "encoding=\"UTF-8\"")).with_context(|| format!("generated {name} doesn't parse"))?;
        // schtasks only takes task XML as UTF-16.
        std::fs::write(out.join(name), crate::registry::utf16(&xml))?;
    }
    Ok(())
}

/// schtasks' defaults would skip a task on battery and stop it after 72
/// hours, hence the settings.
fn task_xml(user: &str, principal: &str, args: &str, log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Installs defenestrate's apps, then deletes itself. Log: {log}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      {principal}
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT4H</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>powershell.exe</Command>
      <Arguments>-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\Windows\Setup\defenestrate\apps\install-apps.ps1{args}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
    .replace('\n', "\r\n")
}
