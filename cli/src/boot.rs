//! The boot queue: things Windows does once, at its next boot, that Linux
//! asked for. `build iso` puts the first actions into the image and `update`
//! adds to it on an installed system, so a fresh install's first boot and an
//! update's next boot run the same actions through the same runner.
//!
//! Every action belongs to one phase, fixed by its type:
//! - [`MachineAction`]s run as SYSTEM at startup, before anyone logs on
//!   (task `defenestrate-boot`), e.g. HKLM imports;
//! - [`UserAction`]s run at the user's logon, as the user, elevated, in
//!   their own session (task `defenestrate-boot-user`), e.g. HKCU imports and
//!   the SystemParametersInfo calls that put them into effect in that session.
//!
//! An HKCU import has no machine form, so it can't be queued to run as
//! SYSTEM; the runner (assets/boot.ps1) also refuses a .reg whose keys are
//! outside its phase's root.
//!
//! On C:, under C:\Windows\Setup\defenestrate\boot:
//!   boot.ps1                 the runner; `-Register` registers both tasks
//!   machine.xml, user.xml    those tasks
//!   machine/, user/          the queue: NNNNNN-<name>.json, the action;
//!                            NNNNNN-<name>.reg, its payload; and once it
//!                            ran, NNNNNN-<name>.done
//!
//! Under C:\Windows rather than %ProgramData%: ProgramData lets any user
//! create files in its subdirectories, which a SYSTEM runner must not take
//! actions from; C:\Windows hands down write access to SYSTEM and
//! Administrators only, and `update` creates the queue's files through
//! ntfs-3g with that inheritance (ntfs::Mount::inheriting).
//!
//! The runner takes a phase's pending actions in sequence order, writes the
//! .done marker after each one succeeds, and stops at the first failure,
//! leaving it and everything after it for the next boot. So a crash midway
//! redoes at most the action it crashed in, and a registry import is safe to
//! redo.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::config::{Config, Hive};
use crate::registry::{self, Root};
use crate::render::{CPath, IfMissing, InstallFile, Payload, ReadAt};
use crate::tasks::{self, AsUser, Logon, RunLevel, Task, Trigger};
use crate::unattend::FirstLogon;

pub const DIR: CPath = CPath::new("Windows/Setup/defenestrate/boot");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// SYSTEM, at startup.
    Machine,
    /// The user, at logon.
    User,
}

impl Phase {
    pub const ALL: [Phase; 2] = [Phase::Machine, Phase::User];

    /// Its queue's directory under [`DIR`], which is also how the runner is
    /// told which phase it runs (`-Phase Machine`, `-Phase User`).
    pub fn dir(self) -> &'static str {
        match self {
            Phase::Machine => "machine",
            Phase::User => "user",
        }
    }
}

/// Which HKLM import: the build's software.reg, system.reg and shell.reg.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum MachineReg {
    Software,
    System,
    /// The Shell value, which the build applies at first logon rather than
    /// with software.reg (render::REG_FILES).
    Shell,
}

impl MachineReg {
    fn root(self) -> Root {
        match self {
            MachineReg::Software | MachineReg::Shell => Root::Software,
            MachineReg::System => Root::System,
        }
    }

    fn name(self) -> &'static str {
        match self {
            MachineReg::Software => "software",
            MachineReg::System => "system",
            MachineReg::Shell => "shell",
        }
    }
}

/// A queued machine action as the runner reads it (machine/*.json).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum MachineAction {
    /// `reg import` of the .reg next to it, all under HKEY_LOCAL_MACHINE.
    #[serde(rename_all = "camelCase")]
    RegImport { target: MachineReg, sha256: String },
}

/// A queued user action as the runner reads it (user/*.json).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum UserAction {
    /// `reg import` of the .reg next to it, all under HKEY_CURRENT_USER,
    /// then the SystemParametersInfo calls that put what it changed into
    /// effect in the running session.
    #[serde(rename_all = "camelCase")]
    RegImport {
        sha256: String,
        refresh: BTreeSet<Refresh>,
        /// Values it sets that win32k only reads at logon and nothing
        /// refreshes, which the runner logs as taking another logon.
        at_next_logon: Vec<String>,
    },
}

/// A SystemParametersInfo call the user phase makes after an import, with
/// the values the import left in HKCU, and SPIF_UPDATEINIFILE |
/// SPIF_SENDCHANGE (https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-systemparametersinfow).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Refresh {
    /// SPI_SETMOUSE: MouseThreshold1, MouseThreshold2, MouseSpeed (the
    /// "Enhance pointer precision" switch).
    Mouse,
    /// SPI_SETMOUSESPEED: MouseSensitivity, the slider.
    MouseSpeed,
    /// SPI_SETKEYBOARDDELAY: KeyboardDelay.
    KeyboardDelay,
    /// SPI_SETKEYBOARDSPEED: KeyboardSpeed.
    KeyboardSpeed,
}

const MOUSE: &str = "control panel\\mouse";
const KEYBOARD: &str = "control panel\\keyboard";
const DESKTOP: &str = "control panel\\desktop";

/// What makes an import's values take effect without a logon: the
/// refreshes for the values that have one, and the win32k-loaded values
/// (under Control Panel\Mouse, Keyboard and Desktop) that have none. Values
/// elsewhere are read by whoever reads them, whenever they do.
///
/// SmoothMouseXCurve/YCurve only load at logon: setting the speed and
/// acceleration again doesn't reload them (saturn 2026-11-28, where doing so
/// switched Enhance pointer precision on over Windows' stock accelerating
/// curve for the rest of the session). So an import with a curve leaves all
/// of Control Panel\Mouse to the next logon, where they load together.
fn refreshes(keys: &Hive) -> (BTreeSet<Refresh>, Vec<String>) {
    let (mut refresh, mut at_logon) = (BTreeSet::new(), Vec::new());
    let curve = keys.iter().any(|(key, values)| {
        key.eq_ignore_ascii_case(MOUSE)
            && values.keys().any(|n| n.eq_ignore_ascii_case("smoothmousexcurve") || n.eq_ignore_ascii_case("smoothmouseycurve"))
    });
    for (key, values) in keys {
        let key_lower = key.to_ascii_lowercase();
        for name in values.keys() {
            let r = match (key_lower.as_str(), name.to_ascii_lowercase().as_str()) {
                (MOUSE, "mousespeed" | "mousethreshold1" | "mousethreshold2") if !curve => Some(Refresh::Mouse),
                (MOUSE, "mousesensitivity") if !curve => Some(Refresh::MouseSpeed),
                (KEYBOARD, "keyboarddelay") => Some(Refresh::KeyboardDelay),
                (KEYBOARD, "keyboardspeed") => Some(Refresh::KeyboardSpeed),
                (MOUSE | KEYBOARD | DESKTOP, _) => {
                    at_logon.push(format!("{key}\\{name}"));
                    None
                }
                _ => None,
            };
            refresh.extend(r);
        }
    }
    (refresh, at_logon)
}

/// What a queued action replaces: a newer one for the same target makes an
/// older pending one moot, and the newest one's hash is what was last asked
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Machine(MachineReg),
    CurrentUser,
}

impl Target {
    pub fn phase(self) -> Phase {
        match self {
            Target::Machine(_) => Phase::Machine,
            Target::CurrentUser => Phase::User,
        }
    }

    /// For file names and messages.
    pub fn name(self) -> &'static str {
        match self {
            Target::Machine(m) => m.name(),
            Target::CurrentUser => "user",
        }
    }
}

/// An action with its payload, ready to queue. Built only through
/// [`Action::machine_reg`] and [`Action::user_reg`], which render the .reg
/// under their phase's root themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Machine { action: MachineAction, reg: String },
    User { action: UserAction, reg: String },
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl Action {
    pub fn machine_reg(target: MachineReg, keys: &Hive) -> Self {
        let reg = registry::render(target.root(), keys);
        Action::Machine { action: MachineAction::RegImport { target, sha256: sha256(&reg) }, reg }
    }

    /// The Default profile's keys, imported into the user's own hive.
    pub fn user_reg(keys: &Hive) -> Self {
        let reg = registry::render_under("HKEY_CURRENT_USER", keys);
        let (refresh, at_next_logon) = refreshes(keys);
        Action::User { action: UserAction::RegImport { sha256: sha256(&reg), refresh, at_next_logon }, reg }
    }

    pub fn target(&self) -> Target {
        match self {
            Action::Machine { action, .. } => action.target(),
            Action::User { .. } => Target::CurrentUser,
        }
    }

    pub fn phase(&self) -> Phase {
        self.target().phase()
    }

    fn sha256(&self) -> &str {
        match self {
            Action::Machine { action, .. } => action.sha256(),
            Action::User { action: UserAction::RegImport { sha256, .. }, .. } => sha256,
        }
    }

    fn json(&self) -> String {
        match self {
            Action::Machine { action, .. } => serde_json::to_string_pretty(action),
            Action::User { action, .. } => serde_json::to_string_pretty(action),
        }
        .expect("plain data")
    }

    fn reg(&self) -> &str {
        match self {
            Action::Machine { reg, .. } | Action::User { reg, .. } => reg,
        }
    }
}

/// An action as read back off a queue directory.
trait Record: DeserializeOwned {
    fn target(&self) -> Target;
    fn sha256(&self) -> &str;
}

impl Record for MachineAction {
    fn target(&self) -> Target {
        match self {
            MachineAction::RegImport { target, .. } => Target::Machine(*target),
        }
    }
    fn sha256(&self) -> &str {
        match self {
            MachineAction::RegImport { sha256, .. } => sha256,
        }
    }
}

/// A user record as `read` needs it: what it imports, not how the runner
/// refreshes after, which older dfnstrates wrote with names this one no
/// longer has (`mouseCurve`, before 5d1cd0f; saturn's queue still had one).
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum QueuedUser {
    #[serde(rename_all = "camelCase")]
    RegImport {
        sha256: String,
        #[allow(dead_code)]
        refresh: serde::de::IgnoredAny,
        #[allow(dead_code)]
        at_next_logon: serde::de::IgnoredAny,
    },
}

impl Record for QueuedUser {
    fn target(&self) -> Target {
        match self {
            QueuedUser::RegImport { .. } => Target::CurrentUser,
        }
    }
    fn sha256(&self) -> &str {
        match self {
            QueuedUser::RegImport { sha256, .. } => sha256,
        }
    }
}

/// One action on the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: u32,
    /// The file names' stem: `{seq:06}-{target name}`.
    pub stem: String,
    pub target: Target,
    pub sha256: String,
    /// Its .done marker is there: the runner finished it.
    pub done: bool,
}

impl Entry {
    fn phase(&self) -> Phase {
        self.target.phase()
    }
}

/// `NNNNNN-name` -> NNNNNN.
fn seq_of(stem: &str) -> Option<u32> {
    let (n, rest) = stem.split_once('-')?;
    (n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty()).then(|| n.parse().ok()).flatten()
}

/// What's on the queue under `boot` (C:\Windows\Setup\defenestrate\boot on
/// a mounted install), oldest first. An action is its .json: a .reg or
/// .done without one is a half-finished write or removal, and ignored. An
/// install from before the queue has none of it, which is an empty queue.
pub fn read(boot: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for phase in Phase::ALL {
        let dir = boot.join(phase.dir());
        let list = match std::fs::read_dir(&dir) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| dir.display().to_string()),
        };
        for f in list {
            let path = f?.path();
            let Some(stem) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".json")) else { continue };
            let seq = seq_of(stem).with_context(|| format!("{} isn't a queued action's name", path.display()))?;
            let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
            let (target, sha256) = match phase {
                Phase::Machine => parsed::<MachineAction>(&text),
                Phase::User => parsed::<QueuedUser>(&text),
            }
            .with_context(|| path.display().to_string())?;
            let done = dir.join(format!("{stem}.done")).exists();
            entries.push(Entry { seq, stem: stem.to_owned(), target, sha256, done });
        }
    }
    entries.sort_by_key(|e| e.seq);
    Ok(entries)
}

fn parsed<R: Record>(text: &str) -> Result<(Target, String)> {
    let r: R = serde_json::from_str(text)?;
    Ok((r.target(), r.sha256().to_owned()))
}

/// What to do to a queue to get `wanted` applied.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan<'a> {
    /// New entries, in the order they're numbered.
    pub add: Vec<(String, &'a Action)>,
    /// Pending ones a newer action for the same target replaces, and done
    /// ones older than the newest done one for their target (history nobody
    /// reads), as (phase, stem).
    pub remove: Vec<(Phase, String)>,
    /// Targets whose newest action, pending or done, is already this.
    pub unchanged: Vec<Target>,
}

/// Queues each wanted action unless its target's newest action, pending or
/// done, has the same hash: what was last asked for is the baseline, whether
/// the runner has got to it yet or not. A wanted action replaces its
/// target's pending ones, since importing the older first would end in the
/// same state (removing a value takes "delete", not leaving it out).
pub fn plan<'a>(queue: &[Entry], wanted: &'a [Action]) -> Plan<'a> {
    let mut p = Plan::default();
    let mut next = queue.iter().map(|e| e.seq).max().unwrap_or(0) + 1;
    let mut by_target: BTreeMap<Target, Vec<&Entry>> = BTreeMap::new();
    for e in queue {
        by_target.entry(e.target).or_default().push(e);
    }
    let mut ordered: Vec<&Action> = wanted.iter().collect();
    // Machine first: within a boot its phase runs first, and the numbers say so.
    ordered.sort_by_key(|a| a.phase());
    for a in ordered {
        let entries = by_target.get(&a.target()).map(Vec::as_slice).unwrap_or_default();
        if entries.last().is_some_and(|e| e.sha256 == a.sha256()) {
            p.unchanged.push(a.target());
            continue;
        }
        p.remove.extend(entries.iter().filter(|e| !e.done).map(|e| (e.phase(), e.stem.clone())));
        p.add.push((format!("{next:06}-{}", a.target().name()), a));
        next += 1;
    }
    for entries in by_target.values() {
        let done: Vec<&&Entry> = entries.iter().filter(|e| e.done).collect();
        if let Some((_newest, older)) = done.split_last() {
            p.remove.extend(older.iter().map(|e| (e.phase(), e.stem.clone())));
        }
    }
    p
}

/// Carries out a plan on the queue under `boot`. The .json goes last when
/// writing and first when removing, so whatever an interruption leaves is
/// either a whole action or something [`read`] and the runner ignore.
pub fn apply(boot: &Path, plan: &Plan) -> Result<()> {
    for (phase, stem) in &plan.remove {
        let dir = boot.join(phase.dir());
        for ext in ["json", "reg", "done"] {
            match std::fs::remove_file(dir.join(format!("{stem}.{ext}"))) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {stem}.{ext}")),
                _ => {}
            }
        }
    }
    for (stem, action) in &plan.add {
        let dir = boot.join(action.phase().dir());
        std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
        // A marker left by an interrupted removal must not count for the
        // new action that takes its number.
        match std::fs::remove_file(dir.join(format!("{stem}.done"))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {stem}.done")),
            _ => {}
        }
        // reg.exe wants UTF-16; the hash is of the text, which the runner
        // checks the file against.
        write_synced(&dir.join(format!("{stem}.reg")), &registry::utf16(action.reg()))?;
        write_synced(&dir.join(format!("{stem}.json")), action.json().as_bytes())?;
    }
    Ok(())
}

fn write_synced(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path).with_context(|| path.display().to_string())?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

/// The runner, its tasks, and first logon's registration of them. The queue
/// itself comes from [`plan`] and [`apply`], by `build iso` and `update`.
pub fn render(config: &Config, dir: &Path) -> Result<Payload> {
    let out = dir.join("boot");
    std::fs::create_dir_all(&out)?;
    let script = DIR.join("boot.ps1");
    let mut files = Vec::new();
    let mut write = |name: &str, data: &[u8]| -> Result<()> {
        std::fs::write(out.join(name), data)?;
        let mut f = InstallFile::new(DIR.join(name), out.join(name), ReadAt::EveryLogon);
        // An install from before the queue gets all three from `update`,
        // with what C:\Windows\Setup\defenestrate hands down (admins and
        // SYSTEM write, users read), and linux-first.ps1 registers the tasks
        // from them at the next startup.
        f.if_missing = IfMissing::Inherit;
        files.push(f);
        Ok(())
    };
    write("boot.ps1", include_bytes!("../assets/boot.ps1"))?;
    let user = config.user.name.as_str();
    // Read by `boot.ps1 -Register` whenever the tasks are missing, so they
    // count as read every logon, though a registered task isn't
    // re-registered when they change.
    write(
        "machine.xml",
        &tasks::xml(&Task {
            description: "Runs what dfnstrate queued for the machine (HKLM imports), once each. Log: %ProgramData%\\defenestrate\\boot-machine.log",
            trigger: Trigger::Startup,
            script: &script,
            script_args: "-Phase Machine",
        })?,
    )?;
    write(
        "user.xml",
        &tasks::xml(&Task {
            description: "Runs what dfnstrate queued for the user (HKCU imports, settings refreshes), once each. Log: %ProgramData%\\defenestrate\\boot-user.log",
            // The user's own session, for HKCU and for SystemParametersInfo
            // to reach their desktop; elevated, because the queue is only
            // writable by admins.
            trigger: Trigger::Logon(AsUser { user, logon: Logon::Interactive, level: RunLevel::Elevated }),
            script: &script,
            script_args: "-Phase User",
        })?,
    )?;
    let first_logon = vec![FirstLogon::powershell(&script, "-Register", "Register the boot queue's tasks")];
    Ok(Payload { files, first_logon })
}

/// The registry config as queued actions: each of the build's .reg files,
/// with the Default profile's going into the user's own hive instead.
pub fn registry_actions(software: &Hive, system: &Hive, default_user: &Hive, shell: &Hive) -> Vec<Action> {
    vec![
        Action::machine_reg(MachineReg::Software, software),
        Action::machine_reg(MachineReg::System, system),
        Action::machine_reg(MachineReg::Shell, shell),
        Action::user_reg(default_user),
    ]
}

/// A fresh queue holding `actions`, written to `dir` for the image.
pub fn initial(dir: &Path, actions: &[Action]) -> Result<()> {
    apply(dir, &plan(&[], actions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RegValue;

    fn hive(entries: &[(&str, &str, RegValue)]) -> Hive {
        let mut h = Hive::new();
        for (k, n, v) in entries {
            h.entry((*k).to_owned()).or_default().insert((*n).to_owned(), v.clone());
        }
        h
    }

    fn sz(s: &str) -> RegValue {
        RegValue::Sz(s.into())
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dfnstrate-test-boot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn phase_comes_from_the_type() {
        let keys = hive(&[("K", "V", RegValue::Dword(1))]);
        for (m, root) in [
            (MachineReg::Software, "HKEY_LOCAL_MACHINE\\SOFTWARE\\"),
            (MachineReg::System, "HKEY_LOCAL_MACHINE\\SYSTEM\\"),
            (MachineReg::Shell, "HKEY_LOCAL_MACHINE\\SOFTWARE\\"),
        ] {
            let a = Action::machine_reg(m, &keys);
            assert_eq!(a.phase(), Phase::Machine);
            assert!(a.reg().contains(&format!("[{root}K]")), "{}", a.reg());
        }
        let u = Action::user_reg(&keys);
        assert_eq!(u.phase(), Phase::User);
        assert!(u.reg().contains("[HKEY_CURRENT_USER\\K]"), "{}", u.reg());
        assert!(!u.reg().contains("HKEY_LOCAL_MACHINE") && !u.reg().contains("HKEY_USERS"), "{}", u.reg());
    }

    #[test]
    fn records_are_what_the_runner_reads() {
        let keys = hive(&[("Control Panel\\Mouse", "MouseSpeed", sz("1"))]);
        let Action::User { action, .. } = Action::user_reg(&keys) else { panic!() };
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v["action"], "regImport");
        assert_eq!(v["refresh"], serde_json::json!(["mouse"]));
        assert_eq!(v["atNextLogon"], serde_json::json!([]));
        assert_eq!(v["sha256"].as_str().unwrap().len(), 64);
        let Action::Machine { action, .. } = Action::machine_reg(MachineReg::Shell, &keys) else { panic!() };
        let v = serde_json::to_value(&action).unwrap();
        assert_eq!(v, serde_json::json!({ "action": "regImport", "target": "shell", "sha256": v["sha256"] }));
        // Neither phase's records parse as the other's.
        assert!(serde_json::from_str::<MachineAction>(r#"{"action":"regImport","target":"software","sha256":"x","refresh":[]}"#).is_err());
        assert!(serde_json::from_str::<UserAction>(r#"{"action":"regImport","target":"software","sha256":"x","refresh":[],"atNextLogon":[]}"#).is_err());
        assert!(serde_json::from_str::<MachineAction>(r#"{"action":"installApps","target":"software","sha256":"x"}"#).is_err());
        // The script knows the actions and refreshes by these names.
        let script = include_str!("../assets/boot.ps1");
        for name in ["'regImport'", "'mouse'", "'mouseSpeed'", "'keyboardDelay'", "'keyboardSpeed'"] {
            assert!(script.contains(name), "boot.ps1 doesn't handle {name}");
        }
        // Task names: the runner registers them, linux-first.ps1 looks one up.
        assert!(script.contains("'defenestrate-boot' = 'machine.xml'") && script.contains("'defenestrate-boot-user' = 'user.xml'"));
        assert!(include_str!("../assets/linux-first.ps1").contains("-TaskName 'defenestrate-boot'"));
    }

    #[test]
    fn which_values_refresh() {
        let mut values = crate::input::pointer(
            &toml::from_str("state = \"Enabled\"\nacceleration = { profile = \"Flat\", speed = 0.6 }").unwrap(),
            crate::input::DisplayScale::try_from(1.0).unwrap(),
        )
        .unwrap();
        values.extend(crate::input::keyboard(&toml::from_str("repeat_delay = 600\nrepeat_rate = 25").unwrap()));
        values.extend(crate::input::DisplayScale::try_from(1.25).unwrap().registry());
        values.push(("Console", "FaceName", sz("x")));
        values.push(("Control Panel\\Mouse", "MouseTrails", sz("0")));
        let keys = hive(&values);
        let (refresh, at_logon) = refreshes(&keys);
        // The flat profile writes a curve, so the pointer waits for the logon
        // that loads it, all of it together.
        assert_eq!(refresh, BTreeSet::from([Refresh::KeyboardDelay, Refresh::KeyboardSpeed]));
        let mouse: Vec<&str> = at_logon.iter().filter_map(|v| v.strip_prefix("Control Panel\\Mouse\\")).collect();
        for name in ["MouseSpeed", "MouseSensitivity", "SmoothMouseXCurve", "SmoothMouseYCurve", "MouseTrails"] {
            assert!(mouse.contains(&name), "{name} not left to the next logon: {at_logon:?}");
        }
        assert!(at_logon.iter().any(|v| v == "Control Panel\\Desktop\\LogPixels"));
        // Without a curve, the pointer's SPIs put it in effect now.
        let no_curve = hive(&[("Control Panel\\Mouse", "MouseSpeed", sz("0")), ("Control Panel\\Mouse", "MouseSensitivity", sz("12"))]);
        assert_eq!(refreshes(&no_curve), (BTreeSet::from([Refresh::Mouse, Refresh::MouseSpeed]), vec![]));
        // Case doesn't matter to the registry, so not here either.
        assert_eq!(refreshes(&hive(&[("control panel\\KEYBOARD", "keyboarddelay", sz("1"))])).0, BTreeSet::from([Refresh::KeyboardDelay]));
        assert_eq!(refreshes(&Hive::new()), (BTreeSet::new(), vec![]));
    }

    fn entry(seq: u32, target: Target, sha: &str, done: bool) -> Entry {
        Entry { seq, stem: format!("{seq:06}-{}", target.name()), target, sha256: sha.into(), done }
    }

    #[test]
    fn hashes_follow_content() {
        let a = hive(&[("K", "V", RegValue::Dword(1))]);
        let b = hive(&[("K", "V", RegValue::Dword(2))]);
        assert_eq!(Action::machine_reg(MachineReg::Software, &a).sha256(), Action::machine_reg(MachineReg::Software, &a).sha256());
        assert_ne!(Action::machine_reg(MachineReg::Software, &a).sha256(), Action::machine_reg(MachineReg::Software, &b).sha256());
        // The same keys under another root are another import.
        assert_ne!(Action::machine_reg(MachineReg::Software, &a).sha256(), Action::machine_reg(MachineReg::System, &a).sha256());
    }

    #[test]
    fn plans_against_what_was_last_asked_for() {
        let k1 = hive(&[("K", "V", RegValue::Dword(1))]);
        let k2 = hive(&[("K", "V", RegValue::Dword(2))]);
        let user = Action::user_reg(&k1);
        let software = Action::machine_reg(MachineReg::Software, &k1);
        let soft2 = Action::machine_reg(MachineReg::Software, &k2);
        let sw = Target::Machine(MachineReg::Software);

        // Empty queue: everything, machine first whatever order it came in.
        let wanted = [user.clone(), software.clone()];
        let p = plan(&[], &wanted);
        assert_eq!(p.add.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["000001-software", "000002-user"]);
        assert!(p.remove.is_empty() && p.unchanged.is_empty());

        // Applied (done) and unchanged: nothing to do.
        let q = [entry(1, sw, software.sha256(), true), entry(2, Target::CurrentUser, user.sha256(), true)];
        assert_eq!(plan(&q, &wanted), Plan { add: vec![], remove: vec![], unchanged: vec![sw, Target::CurrentUser] });

        // Still pending and unchanged: not queued twice.
        let q = [entry(1, sw, software.sha256(), false)];
        assert_eq!(plan(&q, std::slice::from_ref(&software)).add, vec![]);

        // Changed: numbered after everything there, replacing the pending one.
        let q = [entry(3, sw, software.sha256(), true), entry(4, Target::CurrentUser, "old", false), entry(7, sw, "older", false)];
        let wanted = [soft2.clone(), user.clone()];
        let p = plan(&q, &wanted);
        assert_eq!(p.add.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["000008-software", "000009-user"]);
        assert_eq!(p.remove, [(Phase::Machine, "000007-software".to_owned()), (Phase::User, "000004-user".to_owned())]);

        // Changed back to what was applied before, with a newer one pending:
        // the newest decides, so it's queued again.
        let q = [entry(1, sw, software.sha256(), true), entry(2, sw, soft2.sha256(), false)];
        let p = plan(&q, std::slice::from_ref(&software));
        assert_eq!(p.add.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["000003-software"]);
        assert_eq!(p.remove, [(Phase::Machine, "000002-software".to_owned())]);

        // Done history: only the newest done per target stays.
        let q = [entry(1, sw, "a", true), entry(2, sw, "b", true), entry(5, sw, software.sha256(), true)];
        let p = plan(&q, std::slice::from_ref(&software));
        assert!(p.add.is_empty());
        assert_eq!(p.remove, [(Phase::Machine, "000001-software".to_owned()), (Phase::Machine, "000002-software".to_owned())]);
    }

    #[test]
    fn queue_on_disk() {
        let d = tmp("disk");
        let k1 = hive(&[("Control Panel\\Keyboard", "KeyboardDelay", sz("1"))]);
        let wanted = registry_actions(&k1, &Hive::new(), &k1, &Hive::new());
        initial(&d, &wanted).unwrap();
        let q = read(&d).unwrap();
        assert_eq!(
            q.iter().map(|e| (e.stem.as_str(), e.done)).collect::<Vec<_>>(),
            [("000001-software", false), ("000002-system", false), ("000003-shell", false), ("000004-user", false)]
        );
        assert!(d.join("user/000004-user.reg").is_file() && d.join("machine/000001-software.reg").is_file());
        let reg = std::fs::read(d.join("user/000004-user.reg")).unwrap();
        assert_eq!(&reg[..2], &[0xff, 0xfe], "not UTF-16 for reg.exe");
        // The runner got through the first two, then the update comes.
        for stem in ["000001-software", "000002-system"] {
            std::fs::write(d.join(format!("machine/{stem}.done")), "").unwrap();
        }
        // Half-written leftovers are no action.
        std::fs::write(d.join("machine/000009-software.reg"), "x").unwrap();
        std::fs::write(d.join("user/000010-user.done"), "").unwrap();
        let q = read(&d).unwrap();
        assert_eq!(q.iter().map(|e| e.done).collect::<Vec<_>>(), [true, true, false, false]);
        assert_eq!(plan(&q, &wanted).add, vec![], "unchanged config queued again");

        let k2 = hive(&[("Control Panel\\Keyboard", "KeyboardDelay", sz("2"))]);
        let wanted = registry_actions(&k2, &Hive::new(), &k2, &Hive::new());
        let p = plan(&q, &wanted);
        assert_eq!(p.add.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["000005-software", "000006-user"]);
        apply(&d, &p).unwrap();
        let q = read(&d).unwrap();
        assert_eq!(
            q.iter().map(|e| (e.stem.as_str(), e.done)).collect::<Vec<_>>(),
            [("000001-software", true), ("000002-system", true), ("000003-shell", false), ("000005-software", false), ("000006-user", false)]
        );
        assert!(!d.join("user/000004-user.reg").exists(), "the replaced action's payload stayed");

        // A leftover marker doesn't make a new action with its number done.
        std::fs::write(d.join("user/000007-user.done"), "").unwrap();
        let k3 = hive(&[("Control Panel\\Keyboard", "KeyboardDelay", sz("3"))]);
        let wanted = [Action::user_reg(&k3)];
        let p = plan(&read(&d).unwrap(), &wanted);
        assert_eq!(p.add[0].0, "000007-user");
        apply(&d, &p).unwrap();
        assert!(!read(&d).unwrap().iter().find(|e| e.stem == "000007-user").unwrap().done);

        // One an older dfnstrate wrote, with a refresh this one dropped, is
        // still read: its hash is what counts (saturn's 000004-user).
        let old = std::fs::read_to_string(d.join("user/000007-user.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&old).unwrap();
        let sha = v["sha256"].as_str().unwrap().to_owned();
        std::fs::write(
            d.join("user/000007-user.json"),
            serde_json::json!({ "action": "regImport", "sha256": sha, "refresh": ["mouse", "mouseCurve"], "atNextLogon": [] }).to_string(),
        )
        .unwrap();
        assert_eq!(read(&d).unwrap().iter().find(|e| e.stem == "000007-user").unwrap().sha256, sha);

        // A user action in the machine queue is refused, not run as SYSTEM,
        // and a machine one in the user queue too.
        std::fs::copy(d.join("user/000007-user.json"), d.join("machine/000011-user.json")).unwrap();
        assert!(read(&d).is_err());
        std::fs::remove_file(d.join("machine/000011-user.json")).unwrap();
        std::fs::copy(d.join("machine/000005-software.json"), d.join("user/000012-software.json")).unwrap();
        assert!(read(&d).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn names() {
        assert_eq!(seq_of("000012-user"), Some(12));
        assert_eq!(seq_of("12-user"), None);
        assert_eq!(seq_of("000012-"), None);
        assert_eq!(seq_of("00001x-user"), None);
    }
}
