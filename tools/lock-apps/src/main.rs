//! Resolves winget package IDs against one pinned commit of
//! microsoft/winget-pkgs and writes apps.lock.json, which nix/apps.nix
//! builds from. Think npm's lockfile: the config names packages, the lock
//! pins exactly what gets installed.
//!
//!   lock-apps [--lock FILE] [--bump] [--winget ID]... [--msstore ID]...
//!             [--remove ID]... [ID]...
//!
//! Bare IDs are baked into the image: their installer is pinned (URL +
//! hash) and installed offline on the machine's first real boot. The rest
//! is for what can't be pinned, and winget installs it online: `--winget`
//! IDs get a winget-pkgs version (bootstrappers, URLs whose hash drifts),
//! `--msstore` IDs are Microsoft Store products, which have no versions to
//! pin. `--bump` moves the pin to winget-pkgs' current master and
//! re-resolves every app already locked.
//!
//! Only a few manifests are needed, so they come from GitHub one by one
//! rather than through a 1 GB checkout. Set GITHUB_TOKEN if the API's
//! 60 requests an hour run out.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::process::Command;

use base64::Engine;
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const REPO: &str = "microsoft/winget-pkgs";

// The lockfile. nix/apps.nix re-checks this shape with module types, so a
// hand-edited lock fails evaluation the same way.

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Lock {
    winget_pkgs_rev: String,
    apps: BTreeMap<String, LockedApp>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum LockedApp {
    /// Installer pinned and baked into the image, installed offline.
    Baked(Baked),
    /// A winget-pkgs version that winget installs online.
    Winget { version: String },
    /// The store only serves a product's current version, so this pins
    /// nothing; it records that the ID exists.
    Msstore {},
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Baked {
    version: String,
    url: String,
    /// SRI, as fetchurl takes it.
    hash: String,
    /// Store name, and the file name on the machine.
    file: String,
    installer_type: InstallerType,
    /// Decides how it's run: per-user installers break when elevated
    /// (Discord's Squirrel Update.exe crashes with 0xC0000005), so those
    /// run as the plain user, everything else elevated.
    scope: Scope,
    /// Appended verbatim to the command line: Windows hands programs one
    /// string, not argv, and manifests write switches that way.
    args: String,
    success_codes: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Machine,
    User,
    /// The manifest doesn't say.
    Unspecified,
}

/// The installer types that can run unattended from a local file. msix,
/// zip, portable, msstore and friends aren't here, so they can't be locked
/// as baked at all.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
enum InstallerType {
    Msi,
    Wix,
    Nullsoft,
    Inno,
    Burn,
    Exe,
}

impl InstallerType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "msi" => Self::Msi,
            "wix" => Self::Wix,
            "nullsoft" => Self::Nullsoft,
            "inno" => Self::Inno,
            "burn" => Self::Burn,
            "exe" => Self::Exe,
            _ => return None,
        })
    }

    /// What winget itself passes when a manifest has no Silent switch.
    /// A bare exe has no convention, so the manifest must say.
    fn default_silent(self) -> Option<&'static str> {
        match self {
            Self::Msi | Self::Wix => Some("/qn /norestart"),
            Self::Nullsoft => Some("/S"),
            Self::Inno => Some("/SP- /VERYSILENT /SUPPRESSMSGBOXES /NORESTART"),
            Self::Burn => Some("/quiet /norestart"),
            Self::Exe => None,
        }
    }

    /// 3010 and 1641 are Windows Installer's "done, reboot needed/started".
    fn default_success_codes(self) -> &'static [i64] {
        match self {
            Self::Msi | Self::Wix | Self::Burn => &[0, 3010, 1641],
            Self::Nullsoft | Self::Inno | Self::Exe => &[0],
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Msi | Self::Wix => "msi",
            _ => "exe",
        }
    }
}

// winget-pkgs installer manifests. Most fields can sit at the root as a
// default for every installer, or on one installer to override it.

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    #[serde(flatten)]
    defaults: Fields,
    #[serde(default)]
    installers: Vec<Installer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Installer {
    architecture: String,
    installer_url: Option<String>,
    installer_sha256: Option<String>,
    #[serde(flatten)]
    fields: Fields,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Fields {
    installer_type: Option<String>,
    nested_installer_type: Option<String>,
    scope: Option<String>,
    installer_locale: Option<String>,
    installer_switches: Option<Switches>,
    install_modes: Option<Vec<String>>,
    installer_success_codes: Option<Vec<i64>>,
    dependencies: Option<Dependencies>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Switches {
    silent: Option<String>,
    custom: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Dependencies {
    #[serde(default)]
    package_dependencies: Vec<PackageDependency>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PackageDependency {
    package_identifier: String,
}

struct GitHub {
    token: Option<String>,
}

impl GitHub {
    fn api(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let mut req = ureq::get(&format!("https://api.github.com/{path}"))
            .set("Accept", "application/vnd.github+json")
            .set("User-Agent", "defenestrate-lock-apps");
        if let Some(token) = &self.token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.call() {
            Ok(resp) => Ok(Some(resp.into_json()?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(format!("GitHub API {path}: {e}").into()),
        }
    }

    fn master_rev(&self) -> Result<String> {
        let commit = self.api(&format!("repos/{REPO}/commits/master"))?.ok_or("winget-pkgs has no master")?;
        Ok(commit["sha"].as_str().ok_or("commit without sha")?.to_owned())
    }

    fn raw(&self, rev: &str, path: &str) -> Result<Option<String>> {
        match ureq::get(&format!("https://raw.githubusercontent.com/{REPO}/{rev}/{path}")).call() {
            Ok(resp) => Ok(Some(resp.into_string()?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(format!("{path}@{rev}: {e}").into()),
        }
    }
}

/// winget's ordering: dot-separated parts, each a number then an optional
/// suffix, missing parts count as 0 (so 1.0 == 1.0.0).
fn cmp_versions(a: &str, b: &str) -> Ordering {
    fn parts(v: &str) -> Vec<(u64, String)> {
        v.split('.')
            .map(|p| {
                let digits = p.chars().take_while(char::is_ascii_digit).count();
                (p[..digits].parse().unwrap_or(0), p[digits..].to_ascii_lowercase())
            })
            .collect()
    }
    let (a, b) = (parts(a), parts(b));
    let zero = (0, String::new());
    (0..a.len().max(b.len()))
        .map(|i| a.get(i).unwrap_or(&zero).cmp(b.get(i).unwrap_or(&zero)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// Newest version of `id` at `rev`, with its installer manifest.
fn latest(gh: &GitHub, rev: &str, id: &str) -> Result<(String, Manifest)> {
    let first = id.chars().next().ok_or("empty package ID")?.to_ascii_lowercase();
    let dir = format!("manifests/{first}/{}", id.replace('.', "/"));
    let listing = gh
        .api(&format!("repos/{REPO}/contents/{dir}?ref={rev}"))?
        .ok_or_else(|| format!("{id}: no such package in winget-pkgs@{rev}"))?;
    let mut names: Vec<&str> = listing
        .as_array()
        .ok_or("directory listing isn't a list")?
        .iter()
        .filter(|e| e["type"] == "dir")
        .filter_map(|e| e["name"].as_str())
        .collect();
    names.sort_by(|a, b| cmp_versions(b, a));
    // The directory also holds sub-packages (Mozilla.Firefox.ESR lives in
    // Mozilla/Firefox/ESR), which have no manifest for this ID, so the
    // newest name with one is the newest version.
    for version in names {
        for file in [format!("{id}.installer.yaml"), format!("{id}.yaml")] {
            if let Some(text) = gh.raw(rev, &format!("{dir}/{version}/{file}"))? {
                let manifest = serde_yaml::from_str(&text).map_err(|e| format!("{id} {version}: {file}: {e}"))?;
                return Ok((version.to_owned(), manifest));
            }
        }
    }
    Err(format!("{id}: no versions in winget-pkgs@{rev}").into())
}

/// Picks the installer winget would on an x64 English machine installing
/// for everyone, and pins it.
fn bake(id: &str, version: &str, manifest: &Manifest) -> Result<Baked> {
    let d = &manifest.defaults;
    let arch_rank = |a: &str| match a {
        "x64" => Some(0),
        "neutral" => Some(1),
        "x86" => Some(2),
        _ => None,
    };
    let mut candidates = Vec::new();
    let mut seen_types = Vec::new();
    for i in &manifest.installers {
        let Some(arch) = arch_rank(&i.architecture) else { continue };
        let f = &i.fields;
        let ty_name = f.installer_type.as_ref().or(d.installer_type.as_ref()).ok_or_else(|| format!("{id}: installer without InstallerType"))?;
        seen_types.push(ty_name.clone());
        let Some(ty) = InstallerType::parse(ty_name) else { continue };
        if f.nested_installer_type.is_some() || d.nested_installer_type.is_some() {
            continue;
        }
        let locale = match f.installer_locale.as_ref().or(d.installer_locale.as_ref()) {
            None => 0,
            Some(l) if l.eq_ignore_ascii_case("en-US") => 0,
            Some(_) => 1,
        };
        let (rank, scope) = match f.scope.as_ref().or(d.scope.as_ref()).map(String::as_str) {
            Some("machine") => (0, Scope::Machine),
            None => (1, Scope::Unspecified),
            Some(_) => (2, Scope::User),
        };
        candidates.push(((arch, locale, rank), i, ty, scope));
    }
    candidates.sort_by_key(|c| c.0);
    let Some(&(_, installer, ty, scope)) = candidates.first() else {
        return Err(format!(
            "{id} {version}: no x64/neutral/x86 installer of a type that installs offline (has: {}). Lock it with --winget instead.",
            if seen_types.is_empty() { "none for this architecture".to_owned() } else { seen_types.join(", ") }
        )
        .into());
    };
    let f = &installer.fields;

    let url = installer.installer_url.clone().ok_or_else(|| format!("{id} {version}: installer has no InstallerUrl"))?;
    let manifest_hash = installer.installer_sha256.as_ref().ok_or_else(|| format!("{id} {version}: installer has no InstallerSha256"))?;
    let hash = sri_from_hex(manifest_hash).ok_or_else(|| format!("{id} {version}: InstallerSha256 {manifest_hash} isn't a sha256"))?;

    let switches = |pick: fn(&Switches) -> &Option<String>| {
        f.installer_switches.as_ref().and_then(|s| pick(s).clone()).or_else(|| d.installer_switches.as_ref().and_then(|s| pick(s).clone()))
    };
    // An exe that's silent with no switches at all (Squirrel, e.g. Discord)
    // says so with InstallModes, which is also what winget goes by.
    let silent_bare = f.install_modes.as_ref().or(d.install_modes.as_ref()).is_some_and(|m| m.iter().any(|m| m == "silent"));
    let silent = switches(|s| &s.silent)
        .or_else(|| ty.default_silent().map(str::to_owned))
        .or_else(|| silent_bare.then(String::new))
        .ok_or_else(|| format!("{id} {version}: a plain exe installer with no Silent switch and no silent install mode. Lock it with --winget instead."))?;
    let args = match switches(|s| &s.custom) {
        Some(custom) if !silent.is_empty() => format!("{silent} {custom}"),
        Some(custom) => custom,
        None => silent,
    };

    let mut success_codes = ty.default_success_codes().to_vec();
    for code in f.installer_success_codes.as_ref().or(d.installer_success_codes.as_ref()).into_iter().flatten() {
        if !success_codes.contains(code) {
            success_codes.push(*code);
        }
    }

    let deps: Vec<&str> = f
        .dependencies
        .as_ref()
        .or(d.dependencies.as_ref())
        .map(|d| d.package_dependencies.iter().map(|p| p.package_identifier.as_str()).collect())
        .unwrap_or_default();
    if !deps.is_empty() {
        eprintln!("warning: {id} depends on {}; list those before it in `apps`", deps.join(", "));
    }

    let file = store_name(&format!("{id}-{version}.{}", ty.extension()));
    // Downloads it into the store under the name fetchurl will use, so the
    // build finds it there instead of downloading again, and a URL whose
    // contents changed since the manifest was written fails now.
    let actual = prefetch(&url, &file)?;
    if actual != hash {
        return Err(format!(
            "{id} {version}: {url} hashes to {actual}, but the manifest says {hash}. Probably an unversioned URL that has moved on; lock it with --winget instead."
        )
        .into());
    }

    Ok(Baked { version: version.to_owned(), url, hash, file, installer_type: ty, scope, args, success_codes })
}

fn sri_from_hex(hex: &str) -> Option<String> {
    if hex.len() != 64 {
        return None;
    }
    let bytes = (0..32).map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()).collect::<Option<Vec<u8>>>()?;
    Some(format!("sha256-{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Nix store names allow only these characters.
fn store_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || "+-._?=".contains(c) { c } else { '-' }).collect()
}

fn prefetch(url: &str, name: &str) -> Result<String> {
    eprintln!("fetching {url}");
    let out = Command::new("nix")
        .args(["--extra-experimental-features", "nix-command", "store", "prefetch-file", "--json", "--hash-type", "sha256", "--name", name, url])
        .output()?;
    if !out.status.success() {
        return Err(format!("downloading {url} failed:\n{}", String::from_utf8_lossy(&out.stderr)).into());
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(json["hash"].as_str().ok_or("nix store prefetch-file printed no hash")?.to_owned())
}

/// Checks a store product exists, through the endpoint winget's msstore
/// source queries.
fn check_msstore(id: &str) -> Result<()> {
    match ureq::get(&format!("https://storeedgefd.dsx.mp.microsoft.com/v9.0/packageManifests/{id}")).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => Err(format!("{id}: no such Microsoft Store product").into()),
        Err(e) => Err(format!("{id}: asking the Microsoft Store: {e}").into()),
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Baked,
    Winget,
    Msstore,
}

fn resolve(gh: &GitHub, rev: &str, id: &str, kind: Kind) -> Result<LockedApp> {
    let app = match kind {
        Kind::Baked => {
            let (version, manifest) = latest(gh, rev, id)?;
            LockedApp::Baked(bake(id, &version, &manifest)?)
        }
        Kind::Winget => LockedApp::Winget { version: latest(gh, rev, id)?.0 },
        Kind::Msstore => {
            check_msstore(id)?;
            LockedApp::Msstore {}
        }
    };
    eprintln!("{id}: {}", match &app {
        LockedApp::Baked(b) => format!("{}, baked {:?} installer ({:?} scope), args {}", b.version, b.installer_type, b.scope, b.args),
        LockedApp::Winget { version } => format!("{version}, winget at first boot"),
        LockedApp::Msstore {} => "Microsoft Store, at first boot".to_owned(),
    });
    Ok(app)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("lock-apps: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut lock_path = PathBuf::from("apps.lock.json");
    let mut bump = false;
    let mut wanted: Vec<(String, Kind)> = Vec::new();
    let mut remove = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--lock" => lock_path = value("--lock")?.into(),
            "--bump" => bump = true,
            "--winget" => wanted.push((value("--winget")?, Kind::Winget)),
            "--msstore" => wanted.push((value("--msstore")?, Kind::Msstore)),
            "--remove" => remove.push(value("--remove")?),
            s if s.starts_with('-') => return Err(format!("unknown flag {s}").into()),
            id => wanted.push((id.to_owned(), Kind::Baked)),
        }
    }
    if wanted.is_empty() && remove.is_empty() && !bump {
        return Err("usage: lock-apps [--lock FILE] [--bump] [--winget ID]... [--msstore ID]... [--remove ID]... [ID]...".into());
    }

    let gh = GitHub { token: std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()) };
    let mut lock = match std::fs::read_to_string(&lock_path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", lock_path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Lock { winget_pkgs_rev: gh.master_rev()?, apps: BTreeMap::new() },
        Err(e) => return Err(format!("{}: {e}", lock_path.display()).into()),
    };

    for id in &remove {
        lock.apps.remove(id).ok_or_else(|| format!("{id} isn't locked"))?;
    }
    if bump {
        lock.winget_pkgs_rev = gh.master_rev()?;
        eprintln!("winget-pkgs pinned at {}", lock.winget_pkgs_rev);
        for (id, app) in &lock.apps {
            if !wanted.iter().any(|(w, _)| w == id) {
                let kind = match app {
                    LockedApp::Baked(_) => Kind::Baked,
                    LockedApp::Winget { .. } => Kind::Winget,
                    LockedApp::Msstore {} => Kind::Msstore,
                };
                wanted.push((id.clone(), kind));
            }
        }
    }
    // Everything resolves before anything is written, so one bad ID leaves
    // the lock as it was.
    let resolved = wanted
        .iter()
        .map(|(id, kind)| Ok((id.clone(), resolve(&gh, &lock.winget_pkgs_rev, id, *kind)?)))
        .collect::<Result<Vec<_>>>()?;
    lock.apps.extend(resolved);

    std::fs::write(&lock_path, serde_json::to_string_pretty(&lock)? + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_like_winget() {
        assert_eq!(cmp_versions("158.0.1", "158.0"), Ordering::Greater);
        assert_eq!(cmp_versions("159.0", "158.0.1"), Ordering::Greater);
        assert_eq!(cmp_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp_versions("10.0", "9.9"), Ordering::Greater);
        assert_eq!(cmp_versions("ESR", "1.0"), Ordering::Less);
    }

    #[test]
    fn manifest_hash_becomes_sri() {
        assert_eq!(
            sri_from_hex("E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855").as_deref(),
            Some("sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=")
        );
        assert_eq!(sri_from_hex("nope"), None);
    }
}
