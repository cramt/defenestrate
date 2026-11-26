//! Carries the Windows user's Firefox across a reflash (`persist.firefox`),
//! and gives the first install a profile made from a Linux browser's logins
//! (`persist.firefox.seed`).
//!
//! `flash` copies %APPDATA%\Mozilla\Firefox off the install it's about to
//! erase into a store on Linux, and writes it onto the new install before
//! its first boot, offline, through ntfs-3g. Not through the logon copy
//! (`wsl.linux.files`): that replaces its files at every logon, which would
//! put the old profile back over the live one each time; it needs
//! `wsl.linux`; and it runs alongside the first logon's apps task, where a
//! Firefox started first would make a profile of its own. Written before the
//! first boot, the profile is there before Firefox is even installed.
//!
//! The whole directory goes, profiles.ini and installs.ini included, so the
//! new Firefox opens the same profile: installs.ini picks a profile per
//! install directory, by a hash of its path, and winget puts Firefox in the
//! same C:\Program Files\Mozilla Firefox each time.
//!
//! Firefox refuses a profile a newer Firefox last ran (compatibility.ini's
//! LastVersion), offering only a new profile. The old install's Firefox
//! updated itself, and the new one is whatever winget has at first boot,
//! which can trail Firefox's own updater by a release. Offline there's no
//! knowing which Firefox will open it, so compatibility.ini isn't put back:
//! without it Firefox takes the start as an upgrade (it rebuilds its startup
//! cache and rescans extensions) and keeps the profile. Within a major
//! version that's exactly right; across one (a reflash on release day,
//! before winget has it), the profile's databases are a version newer than
//! the Firefox opening them, which Firefox mostly copes with.

use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{Config, FirefoxPersist, FirefoxSeed, WindowsName};
use crate::logs::{chown_tree, sudo_caller};
use crate::ntfs::{self, Mount, VolumeArgs};
use crate::tools::{self, NTFS3G, NTFS3G_PROBE, SQLITE3};

/// Firefox's per-user directory: profiles.ini (the profiles there are),
/// installs.ini (which one each install opens) and the profiles.
const FIREFOX_DIR: &str = "AppData/Roaming/Mozilla/Firefox";

/// C:\Users\<user>\AppData\Roaming\Mozilla\Firefox, relative to C:\.
pub fn firefox_dir(user: &WindowsName) -> PathBuf {
    Path::new("Users").join(user.as_str()).join(FIREFOX_DIR)
}

/// What a new install's Firefox starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The store's copy of an earlier install's.
    Kept,
    /// A new profile with the seed's logins, for a first install.
    Seed,
    /// Nothing: Firefox makes its own profile at its first start.
    Fresh,
}

impl Source {
    /// A kept profile always wins: the seed is only ever for the first
    /// install, after which Windows' Firefox is its own.
    pub fn pick(kept: bool, seed: bool) -> Self {
        match (kept, seed) {
            (true, _) => Self::Kept,
            (false, true) => Self::Seed,
            (false, false) => Self::Fresh,
        }
    }
}

/// Where kept files live on Linux, one per Windows machine: `firefox` is
/// the last copy, `firefox.prev` the one before it.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// $XDG_STATE_HOME/dfnstrate/persist/<computerName>, of whoever runs
    /// dfnstrate (resolved before it reruns itself under sudo).
    pub fn default_dir(computer: &WindowsName) -> Result<PathBuf> {
        let state = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME").context("neither XDG_STATE_HOME nor HOME is set")?).join(".local/state"),
        };
        Ok(state.join("dfnstrate/persist").join(computer.as_str()))
    }

    fn firefox(&self) -> PathBuf {
        self.dir.join("firefox")
    }

    /// Made as the caller, before dfnstrate reruns itself under sudo, so it
    /// and the directories above it are theirs; root only writes inside it.
    /// Cookies and saved passwords: nobody else may read it.
    pub fn create(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir).with_context(|| self.dir.display().to_string())?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    pub fn has_firefox(&self) -> bool {
        self.firefox().join("profiles.ini").is_file()
    }
}

/// Left behind when copying off: Firefox's lock, which nothing holds once
/// Windows is down.
fn kept(rel: &Path) -> bool {
    !named(rel, "parent.lock")
}

/// Not put back (module docs).
fn restored(rel: &Path) -> bool {
    !named(rel, "compatibility.ini")
}

fn named(rel: &Path, name: &str) -> bool {
    rel.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(name))
}

/// The Firefox version in a compatibility.ini's LastVersion
/// ("146.0.1_20261118023529/20261118023529").
fn last_version(compatibility_ini: &str) -> Option<&str> {
    compatibility_ini.lines().find_map(|l| l.trim().strip_prefix("LastVersion=")).and_then(|v| v.split('_').next()).filter(|v| !v.is_empty())
}

/// Copies the install's Firefox directory (C:\ mounted at `root`) into the
/// store, the last copy becoming firefox.prev. False when the install has
/// none, leaving the store as it was.
pub fn save(root: &Path, user: &WindowsName, store: &Store, owner: Option<(u32, u32)>) -> Result<bool> {
    let from = root.join(firefox_dir(user));
    if !from.join("profiles.ini").is_file() {
        println!("no Firefox profile on the install to keep (C:\\{})", firefox_dir(user).display().to_string().replace('/', "\\"));
        return Ok(false);
    }
    store.create()?;
    // Made by root when this was started as root (sudo dfnstrate …).
    if let Some((uid, gid)) = owner {
        chown(&store.dir, Some(uid), Some(gid))?;
    }
    let new = store.dir.join("firefox.new");
    if new.exists() {
        std::fs::remove_dir_all(&new)?;
    }
    let (files, bytes) = copy_tree(&from, &new, Path::new(""), kept).context("copying Firefox's profiles off the install")?;
    if let Some(owner) = owner {
        chown_tree(&new, owner)?;
    }
    let (current, prev) = (store.firefox(), store.dir.join("firefox.prev"));
    if current.exists() {
        if prev.exists() {
            std::fs::remove_dir_all(&prev)?;
        }
        std::fs::rename(&current, &prev)?;
    }
    std::fs::rename(&new, &current)?;
    println!("kept Firefox's profiles ({files} files, {} MiB) in {}", bytes >> 20, current.display());
    Ok(true)
}

/// Copies directories and regular files, nothing else (NTFS junctions and
/// links point anywhere). Never overwrites, and never sets permissions: on
/// an ntfs-3g mount with `inherit`, a chmod would replace the descriptor
/// the new file inherited, so no std::fs::copy. Returns files and bytes.
fn copy_tree(from: &Path, to: &Path, rel: &Path, keep: fn(&Path) -> bool) -> Result<(usize, u64)> {
    std::fs::create_dir(to).with_context(|| to.display().to_string())?;
    let (mut files, mut bytes) = (0, 0);
    let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let rel = rel.join(entry.file_name());
        if !keep(&rel) {
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            let (f, b) = copy_tree(&entry.path(), &to.join(entry.file_name()), &rel, keep)?;
            files += f;
            bytes += b;
        } else if meta.is_file() {
            bytes += copy_file(&entry.path(), &to.join(entry.file_name()))?;
            files += 1;
        }
    }
    Ok((files, bytes))
}

fn copy_file(from: &Path, to: &Path) -> Result<u64> {
    let mut src = File::open(from).with_context(|| from.display().to_string())?;
    let mut dst = File::options().write(true).create_new(true).open(to).with_context(|| to.display().to_string())?;
    let n = std::io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    Ok(n)
}

// The seed.

/// installs.ini's key for C:\Program Files\Mozilla Firefox, where winget
/// puts a machine-wide Firefox: a CityHash64 of the install directory
/// (GetInstallHash, toolkit/mozapps/update/common/commonupdatedir.cpp), the
/// same on every machine. Seen on saturn as the suffix of Firefox's ProgIds
/// (FirefoxURL-308046B0AF4A39CB). A per-user install
/// (%LOCALAPPDATA%\Mozilla Firefox) hashes differently and makes a profile
/// of its own.
const FIREFOX_INSTALL_HASH: &str = "308046B0AF4A39CB";

/// The seeded profile's directory, relative to the Firefox directory.
const SEED_PROFILE: &str = "Profiles/defenestrate.default-release";

/// profiles.ini and installs.ini naming the seeded profile as the
/// machine-wide install's own (Locked), so Firefox opens it rather than
/// making one at its first start.
fn seed_inis() -> (String, String) {
    let install = format!("Default={SEED_PROFILE}\r\nLocked=1\r\n");
    let profiles = format!(
        "[Install{FIREFOX_INSTALL_HASH}]\r\n{install}\r\n[Profile0]\r\nName=default-release\r\nIsRelative=1\r\nPath={SEED_PROFILE}\r\nDefault=1\r\n\r\n[General]\r\nStartWithLastProfile=1\r\nVersion=2\r\n"
    );
    (profiles, format!("[{FIREFOX_INSTALL_HASH}]\r\n{install}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Taken with sqlite's backup, a consistent copy of a database the
    /// browser may have open, its write-ahead log included.
    Sqlite,
    /// Written whole and renamed into place by the browser, so a copy is
    /// never half of one.
    Json,
}

/// The logged-in state, and nothing tied to the browser or its OS. Both
/// login stores: logins.json, and logins.db, the SQLite store newer Geckos
/// mirror it to. Their passwords are encrypted with key4.db's key, which a
/// primary password (if set) protects the same way on Windows.
/// containers.json because a container's cookies are stored under its id
/// (originAttributes' userContextId), which only means that container
/// while containers.json says so.
fn seed_files(history: bool) -> Vec<(&'static str, Kind)> {
    let mut files = vec![
        ("cookies.sqlite", Kind::Sqlite),
        ("logins.json", Kind::Json),
        ("logins.db", Kind::Sqlite),
        ("key4.db", Kind::Sqlite),
        ("containers.json", Kind::Json),
    ];
    if history {
        files.extend([("places.sqlite", Kind::Sqlite), ("favicons.sqlite", Kind::Sqlite)]);
    }
    files
}

/// The container a cookie's originAttributes ("^userContextId=2&partitionKey=…")
/// puts it in; none is the default container.
fn user_context_id(origin_attributes: &str) -> Option<u32> {
    origin_attributes.trim_start_matches('^').split('&').find_map(|kv| kv.strip_prefix("userContextId=")).and_then(|v| v.parse().ok()).filter(|&id| id != 0)
}

/// Every container containers.json defines, the browser's private ones
/// included.
fn container_ids(containers_json: &str) -> Result<BTreeSet<u32>> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Containers {
        identities: Vec<Identity>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity {
        user_context_id: u32,
    }
    let c: Containers = serde_json::from_str(containers_json).context("containers.json")?;
    Ok(c.identities.into_iter().map(|i| i.user_context_id).collect())
}

/// Containers cookies are kept under that containers.json doesn't define.
fn orphans<'a>(origin_attributes: impl IntoIterator<Item = &'a str>, defined: &BTreeSet<u32>) -> BTreeSet<u32> {
    origin_attributes.into_iter().filter_map(user_context_id).filter(|id| !defined.contains(id)).collect()
}

/// The seed's files, copied out of its profile. Removed on drop.
pub struct Snapshot {
    dir: PathBuf,
    files: Vec<&'static str>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Runs sqlite3 as the profile's owner, read-only: as root it would leave
/// root-owned -wal and -shm files in the profile, which the browser then
/// can't open.
fn sqlite(owner: (u32, u32), db: &Path, command: &str) -> Result<String> {
    let mut cmd = SQLITE3.cmd();
    cmd.args(["-readonly", "-bail", "-cmd", ".timeout 3000"]).arg(db).arg(command);
    if crate::disk::is_root() && owner.0 != 0 {
        cmd.uid(owner.0).gid(owner.1);
    }
    let out = cmd.output().context("running sqlite3")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Takes the seed's files out of its profile, while it's in use too.
pub fn snapshot(seed: &FirefoxSeed) -> Result<Snapshot> {
    let profile = Path::new(seed.profile.as_str());
    let meta = std::fs::metadata(profile).with_context(|| format!("persist.firefox.seed.profile {}", profile.display()))?;
    if !profile.join("cookies.sqlite").is_file() {
        bail!("{} has no cookies.sqlite; is it a Firefox (or Zen, LibreWolf) profile?", profile.display());
    }
    let owner = (meta.uid(), meta.gid());
    let dir = std::env::temp_dir().join(format!("dfnstrate-seed-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    if crate::disk::is_root() {
        chown(&dir, Some(owner.0), Some(owner.1))?;
    }
    let mut snap = Snapshot { dir, files: Vec::new() };
    for (name, kind) in seed_files(seed.history) {
        let from = profile.join(name);
        if !from.is_file() {
            continue;
        }
        let to = snap.dir.join(name);
        match kind {
            Kind::Sqlite => {
                sqlite(owner, &from, &format!(".backup '{}'", to.display())).map_err(|e| {
                    if e.to_string().contains("locked") {
                        anyhow::anyhow!(
                            "{} is locked: the browser using {} keeps its databases to itself while it runs. Close it and rerun; nothing was written",
                            name,
                            profile.display()
                        )
                    } else {
                        e.context(format!("copying {}", from.display()))
                    }
                })?;
            }
            Kind::Json => {
                copy_file(&from, &to)?;
            }
        }
        snap.files.push(name);
    }
    // The copy has been made, so this asks it, not the browser's database.
    if snap.files.contains(&"containers.json") {
        let attrs = sqlite(owner, &snap.dir.join("cookies.sqlite"), "SELECT DISTINCT originAttributes FROM moz_cookies;")?;
        let defined = container_ids(&std::fs::read_to_string(snap.dir.join("containers.json"))?)?;
        let lost = orphans(attrs.lines(), &defined);
        if !lost.is_empty() {
            eprintln!("warning: the seed has cookies in containers containers.json doesn't define ({lost:?}); those sessions won't be reachable");
        }
    } else if sqlite(owner, &snap.dir.join("cookies.sqlite"), "SELECT DISTINCT originAttributes FROM moz_cookies;")?
        .lines()
        .any(|l| user_context_id(l).is_some())
    {
        eprintln!("warning: the seed has cookies in containers but no containers.json; those sessions won't be reachable");
    }
    println!("seed: {} from {}", snap.files.join(", "), profile.display());
    Ok(snap)
}

/// What gets written onto the new install, settled before anything is
/// erased (a seed the browser holds locked fails here, not after).
pub enum Prepared {
    Kept(PathBuf),
    Seed(Snapshot),
}

/// What `firefox` (with its store) puts on a new install, if anything.
pub fn prepare(firefox: &FirefoxPersist, store: &Store) -> Result<Option<Prepared>> {
    Ok(match Source::pick(store.has_firefox(), firefox.seed.is_some()) {
        Source::Kept => Some(Prepared::Kept(store.firefox())),
        Source::Seed => Some(Prepared::Seed(snapshot(firefox.seed.as_ref().unwrap())?)),
        Source::Fresh => {
            println!("no Firefox kept for this machine and no seed: Firefox makes a new profile at its first start");
            None
        }
    })
}

impl Prepared {
    /// Writes it onto C:\ mounted at `root` (with Mount::inheriting, so the
    /// files get the profile's permissions). Refuses an install that already
    /// has a Firefox directory.
    pub fn write(&self, root: &Path, user: &WindowsName) -> Result<()> {
        let roaming = root.join("Users").join(user.as_str()).join("AppData/Roaming");
        if !roaming.is_dir() {
            bail!("the install has no C:\\Users\\{user}\\AppData\\Roaming; has {user} logged on?");
        }
        let dest = root.join(firefox_dir(user));
        if dest.exists() {
            bail!("the install already has a Firefox directory ({}); only an install without one gets a kept or seeded one", dest.display());
        }
        let need = match self {
            Self::Kept(from) => tree_size(from)?,
            Self::Seed(s) => tree_size(&s.dir)?,
        };
        let free = free_space(root)?;
        // Windows needs room of its own to boot.
        if need + (1 << 30) > free {
            bail!("the profile is {} MiB and the install has {} MiB free", need >> 20, free >> 20);
        }
        std::fs::create_dir_all(dest.parent().unwrap())?;
        match self {
            Self::Kept(from) => {
                for ini in compatibility_inis(from)? {
                    let text = std::fs::read_to_string(&ini).unwrap_or_default();
                    let profile = ini.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    println!(
                        "{profile}: last run by Firefox {}; left out its compatibility.ini, so an older Firefox from winget opens it too",
                        last_version(&text).unwrap_or("(unknown)")
                    );
                }
                let (files, bytes) = copy_tree(from, &dest, Path::new(""), restored)?;
                println!("restored Firefox's profiles ({files} files, {} MiB)", bytes >> 20);
            }
            Self::Seed(snap) => {
                std::fs::create_dir(&dest)?;
                let (profiles, installs) = seed_inis();
                std::fs::write(dest.join("profiles.ini"), profiles)?;
                std::fs::write(dest.join("installs.ini"), installs)?;
                let profile = dest.join(SEED_PROFILE);
                std::fs::create_dir_all(&profile)?;
                for name in &snap.files {
                    copy_file(&snap.dir.join(name), &profile.join(name))?;
                }
                println!("seeded a new Firefox profile with {}", snap.files.join(", "));
            }
        }
        Ok(())
    }
}

fn compatibility_inis(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if std::fs::symlink_metadata(&path)?.is_dir() {
            out.extend(compatibility_inis(&path)?);
        } else if named(&path, "compatibility.ini") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn tree_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = std::fs::symlink_metadata(&path)?;
        total += if meta.is_dir() { tree_size(&path)? } else { meta.len() };
    }
    Ok(total)
}

fn free_space(dir: &Path) -> Result<u64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
    // SAFETY: c is a valid path and st is written by the call before use.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| dir.display().to_string());
        }
        Ok(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

// Commands.

#[derive(clap::Args)]
pub struct StoreArgs {
    /// Where kept profiles live [default: $XDG_STATE_HOME/dfnstrate/persist/<computerName>]
    #[arg(long, value_name = "DIR")]
    pub store: Option<PathBuf>,
}

impl StoreArgs {
    /// The store, as an absolute path resolved as the caller (sudo changes
    /// HOME), and the arguments that pass it on to the rerun under sudo.
    pub fn resolve(&self, config: &Config) -> Result<(Store, Vec<std::ffi::OsString>)> {
        let dir = match &self.store {
            Some(d) => std::path::absolute(d)?,
            None => Store::default_dir(&config.computer_name)?,
        };
        let arg = vec!["--store".into(), dir.clone().into()];
        Ok((Store::new(dir), arg))
    }
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Copy the install's Firefox (profiles.ini, installs.ini, every
    /// profile) into the store, read-only. `flash` does this itself before
    /// it erases an install.
    Save(Args),
    /// Write the store's Firefox, or else a profile seeded from
    /// persist.firefox.seed, onto an install that has none. `flash` does
    /// this itself onto the install it writes.
    Restore(Args),
}

#[derive(clap::Args)]
pub struct Args {
    /// Config files, layered in order over the built-in defaults.
    #[arg(short, long = "config", value_name = "FILE", required = true)]
    configs: Vec<PathBuf>,
    #[command(flatten)]
    volume: VolumeArgs,
    #[command(flatten)]
    store: StoreArgs,
}

const USAGE: &str = "dfnstrate persist save|restore -c CONFIG... /dev/disk/by-id/<the disk Windows is on>";

pub fn run(cmd: &Cmd) -> Result<()> {
    tools::require(&[NTFS3G, NTFS3G_PROBE])?;
    let (saving, args) = match cmd {
        Cmd::Save(a) => (true, a),
        Cmd::Restore(a) => (false, a),
    };
    let config = &crate::config::load(&args.configs, true)?;
    let firefox = config.persist.firefox.as_ref().context("the config has no persist.firefox, so there's nothing to keep")?;
    if !saving && firefox.seed.is_some() {
        tools::require(&[SQLITE3])?;
    }
    let (store, store_arg) = args.store.resolve(config)?;
    let volume = args.volume.volume(config, USAGE, || {
        store.create()?;
        Ok(store_arg)
    })?;
    store.create()?;
    if saving {
        save_from(&volume, config, &store)?;
    } else if let Some(prepared) = prepare(firefox, &store)? {
        restore_onto(&volume, config, &prepared)?;
    }
    Ok(())
}

/// Copies Firefox off a Windows volume into the store, mounted read-only.
/// It must have been shut down cleanly: a hibernated or crashed Windows may
/// have had Firefox running, its databases half written.
pub fn save_from(volume: &Path, config: &Config, store: &Store) -> Result<bool> {
    ntfs::probe(volume, true)?;
    let mount = Mount::new(volume, std::env::temp_dir().join(format!("dfnstrate-persist-{}", std::process::id())), false)?;
    let saved = save(&mount.dir, &config.user.name, store, sudo_caller())?;
    mount.unmount()?;
    Ok(saved)
}

/// Writes a prepared profile onto a Windows volume.
pub fn restore_onto(volume: &Path, config: &Config, prepared: &Prepared) -> Result<()> {
    ntfs::probe(volume, true)?;
    let mount = Mount::inheriting(volume, std::env::temp_dir().join(format!("dfnstrate-persist-{}", std::process::id())))?;
    prepared.write(&mount.dir, &config.user.name)?;
    mount.unmount()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dfnstrate-test-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn user() -> WindowsName {
        WindowsName::try_from("cramt".to_owned()).unwrap()
    }

    #[test]
    fn a_kept_profile_beats_the_seed() {
        assert_eq!(Source::pick(true, true), Source::Kept);
        assert_eq!(Source::pick(true, false), Source::Kept);
        assert_eq!(Source::pick(false, true), Source::Seed);
        assert_eq!(Source::pick(false, false), Source::Fresh);
    }

    #[test]
    fn firefox_is_under_roaming() {
        assert_eq!(firefox_dir(&user()), Path::new("Users/cramt/AppData/Roaming/Mozilla/Firefox"));
    }

    #[test]
    fn which_files_go_where() {
        for (rel, keep, restore) in [
            ("profiles.ini", true, true),
            ("installs.ini", true, true),
            ("Profiles/x.default-release/cookies.sqlite", true, true),
            ("Profiles/x.default-release/parent.lock", false, true),
            ("Profiles/x.default-release/PARENT.LOCK", false, true),
            ("Profiles/x.default-release/compatibility.ini", true, false),
            ("Profiles/x.default-release/Compatibility.ini", true, false),
            ("Profiles/x.default-release/storage/compatibility.ini.bak", true, true),
        ] {
            assert_eq!(kept(Path::new(rel)), keep, "{rel} kept");
            assert_eq!(restored(Path::new(rel)), restore, "{rel} restored");
        }
    }

    #[test]
    fn reads_last_version() {
        let ini = "[Compatibility]\r\nLastVersion=146.0.1_20261118023529/20261118023529\r\nLastOSABI=WINNT_x86_64-msvc\r\n";
        assert_eq!(last_version(ini), Some("146.0.1"));
        assert_eq!(last_version("[Compatibility]\nLastVersion=1.24.3b_20261118023529/20261118023529"), Some("1.24.3b"));
        assert_eq!(last_version("[Compatibility]\n"), None);
        assert_eq!(last_version("LastVersion=_x"), None);
    }

    #[test]
    fn seed_takes_only_logged_in_state() {
        let names = |h| seed_files(h).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names(false), ["cookies.sqlite", "logins.json", "logins.db", "key4.db", "containers.json"]);
        assert_eq!(names(true)[5..], ["places.sqlite", "favicons.sqlite"]);
        // Every database through sqlite's backup, never copied with its WAL.
        for (n, k) in seed_files(true) {
            assert_eq!(k == Kind::Sqlite, n.ends_with(".sqlite") || n.ends_with(".db"), "{n}");
        }
    }

    #[test]
    fn seeded_profile_is_the_installs_own() {
        let (profiles, installs) = seed_inis();
        assert!(profiles.contains("[Install308046B0AF4A39CB]\r\nDefault=Profiles/defenestrate.default-release\r\nLocked=1\r\n"), "{profiles}");
        assert!(profiles.contains("[Profile0]\r\nName=default-release\r\nIsRelative=1\r\nPath=Profiles/defenestrate.default-release\r\nDefault=1\r\n"));
        assert!(profiles.contains("StartWithLastProfile=1"));
        assert_eq!(installs, "[308046B0AF4A39CB]\r\nDefault=Profiles/defenestrate.default-release\r\nLocked=1\r\n");
    }

    #[test]
    fn container_cookies_line_up() {
        // As Zen on saturn has them (2026-11-26).
        let containers = r#"{"version":8,"lastUserContextId":2,"identities":[{"color":"red","icon":"fingerprint","name":"personal","public":true,"userContextId":1},{"color":"blue","icon":"briefcase","name":"work","public":true,"userContextId":2},{"accessKey":"","color":"","icon":"","name":"userContextIdInternal.thumbnail","public":false,"userContextId":4294967294},{"accessKey":"","color":"","icon":"","name":"userContextIdInternal.webextStorageLocal","public":false,"userContextId":4294967295}],"siteAssociations":{}}"#;
        let defined = container_ids(containers).unwrap();
        assert_eq!(defined, BTreeSet::from([1, 2, 4294967294, 4294967295]));
        assert_eq!(user_context_id(""), None);
        assert_eq!(user_context_id("^userContextId=2"), Some(2));
        assert_eq!(user_context_id("^userContextId=2&partitionKey=%28https%2Cmicrosoft.com%29"), Some(2));
        assert_eq!(user_context_id("^partitionKey=%28https%2Cgoogle.com%29"), None);
        assert_eq!(user_context_id("^userContextId=0"), None);
        let attrs = ["", "^userContextId=2", "^partitionKey=%28https%2Cyoutube.com%29", "^userContextId=7&partitionKey=x"];
        assert_eq!(orphans(attrs, &defined), BTreeSet::from([7]));
        assert!(orphans(attrs[..3].iter().copied(), &defined).is_empty());
        assert!(container_ids("{}").is_err());
    }

    #[test]
    fn save_rotates_and_leaves_locks() {
        let d = tmp("save");
        let root = d.join("c");
        let ff = root.join(firefox_dir(&user()));
        std::fs::create_dir_all(ff.join("Profiles/a.default-release/storage")).unwrap();
        std::fs::write(ff.join("profiles.ini"), "one").unwrap();
        std::fs::write(ff.join("Profiles/a.default-release/cookies.sqlite"), "c").unwrap();
        std::fs::write(ff.join("Profiles/a.default-release/parent.lock"), "").unwrap();
        std::os::unix::fs::symlink("/etc", ff.join("Profiles/link")).unwrap();
        let store = Store::new(d.join("store"));
        assert!(!store.has_firefox());
        assert!(save(&root, &user(), &store, None).unwrap());
        assert!(store.has_firefox());
        let kept = store.firefox();
        assert_eq!(std::fs::read_to_string(kept.join("Profiles/a.default-release/cookies.sqlite")).unwrap(), "c");
        assert!(!kept.join("Profiles/a.default-release/parent.lock").exists());
        assert!(!kept.join("Profiles/link").exists());
        assert_eq!(std::fs::metadata(&store.dir).unwrap().permissions().mode() & 0o777, 0o700);

        std::fs::write(ff.join("profiles.ini"), "two").unwrap();
        assert!(save(&root, &user(), &store, None).unwrap());
        assert_eq!(std::fs::read_to_string(store.firefox().join("profiles.ini")).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(store.dir.join("firefox.prev/profiles.ini")).unwrap(), "one");

        // An install without Firefox leaves the store alone.
        std::fs::remove_dir_all(&ff).unwrap();
        assert!(!save(&root, &user(), &store, None).unwrap());
        assert_eq!(std::fs::read_to_string(store.firefox().join("profiles.ini")).unwrap(), "two");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn restore_drops_compatibility_and_refuses_an_existing_firefox() {
        let d = tmp("restore");
        let kept = d.join("kept");
        std::fs::create_dir_all(kept.join("Profiles/a")).unwrap();
        std::fs::write(kept.join("profiles.ini"), "p").unwrap();
        std::fs::write(kept.join("Profiles/a/compatibility.ini"), "[Compatibility]\nLastVersion=999.0_1/1\n").unwrap();
        std::fs::write(kept.join("Profiles/a/cookies.sqlite"), "c").unwrap();
        let root = d.join("c");
        let prepared = Prepared::Kept(kept.clone());
        assert!(prepared.write(&root, &user()).is_err(), "wrote into a profile that isn't there");
        std::fs::create_dir_all(root.join("Users/cramt/AppData/Roaming")).unwrap();
        prepared.write(&root, &user()).unwrap();
        let ff = root.join(firefox_dir(&user()));
        assert_eq!(std::fs::read_to_string(ff.join("Profiles/a/cookies.sqlite")).unwrap(), "c");
        assert!(!ff.join("Profiles/a/compatibility.ini").exists());
        assert!(kept.join("Profiles/a/compatibility.ini").exists(), "the store's copy keeps it");
        assert!(prepared.write(&root, &user()).is_err(), "wrote over an existing Firefox");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn seed_writes_a_locked_profile() {
        let d = tmp("seed");
        let snap_dir = d.join("snap");
        std::fs::create_dir_all(&snap_dir).unwrap();
        std::fs::write(snap_dir.join("cookies.sqlite"), "c").unwrap();
        std::fs::write(snap_dir.join("key4.db"), "k").unwrap();
        let root = d.join("c");
        std::fs::create_dir_all(root.join("Users/cramt/AppData/Roaming")).unwrap();
        let prepared = Prepared::Seed(Snapshot { dir: snap_dir.clone(), files: vec!["cookies.sqlite", "key4.db"] });
        prepared.write(&root, &user()).unwrap();
        let ff = root.join(firefox_dir(&user()));
        assert_eq!(std::fs::read_to_string(ff.join("installs.ini")).unwrap(), seed_inis().1);
        let mut names: Vec<_> = std::fs::read_dir(ff.join(SEED_PROFILE)).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["cookies.sqlite", "key4.db"]);
        drop(prepared);
        assert!(!snap_dir.exists(), "the snapshot outlived its use");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
