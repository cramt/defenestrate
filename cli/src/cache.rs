//! Files the build needs from the network, kept by hash. Whoever fills the
//! cache doesn't matter to the build: dfnstrate downloads into it, and the
//! Nix module hands over a directory of fixed-output derivations named the
//! same way (`Sri::cache_key`), with `--offline`.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::config::{FileSource, Sri};

pub struct Cache {
    pub dir: PathBuf,
    pub offline: bool,
}

impl Cache {
    pub fn new(dir: Option<PathBuf>, offline: bool) -> Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => {
                let base = std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                    .context("neither XDG_CACHE_HOME nor HOME is set; pass --cache")?;
                base.join("dfnstrate")
            }
        };
        Ok(Self { dir, offline })
    }

    pub fn path(&self, hash: &Sri) -> PathBuf {
        self.dir.join(hash.cache_key())
    }

    pub fn lookup(&self, hash: &Sri) -> Option<PathBuf> {
        let p = self.path(hash);
        p.exists().then_some(p)
    }

    /// The file with this hash, downloading it from `url` if it isn't cached.
    pub fn get(&self, url: &str, hash: &Sri) -> Result<PathBuf> {
        if let Some(p) = self.lookup(hash) {
            return Ok(p);
        }
        if self.offline {
            bail!("{url} ({hash}) isn't in the cache at {} and --offline is set", self.dir.display());
        }
        let got = self.download(url)?;
        if got.hash != *hash {
            let _ = std::fs::remove_file(&got.path);
            bail!("{url} hashes to {}, but the config says {hash}", got.hash);
        }
        self.keep(got)
    }

    /// Downloads into the cache without knowing the hash yet (lock, fetch).
    pub fn download(&self, url: &str) -> Result<Downloaded> {
        if self.offline {
            bail!("can't download {url} with --offline");
        }
        eprintln!("downloading {url}");
        let resp = ureq::get(url).call().with_context(|| format!("GET {url}"))?;
        let total: Option<u64> = resp.header("Content-Length").and_then(|l| l.parse().ok());
        self.ingest(resp.into_reader(), total)
    }

    /// Streams `reader` into a temporary file in the cache, hashing it.
    pub fn ingest(&self, mut reader: impl Read, total: Option<u64>) -> Result<Downloaded> {
        std::fs::create_dir_all(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        self.remove_stale_partials();
        let tmp = self.dir.join(format!(".partial-{}", std::process::id()));
        let mut out = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut done: u64 = 0;
        let mut last_report = 0;
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("download interrupted"),
            };
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            done += n as u64;
            if done >> 28 > last_report {
                last_report = done >> 28;
                match total {
                    Some(t) => eprintln!("  {} / {} MiB", done >> 20, t >> 20),
                    None => eprintln!("  {} MiB", done >> 20),
                }
            }
        }
        out.sync_all()?;
        Ok(Downloaded { path: tmp, hash: Sri::from_digest(hasher.finalize().into()) })
    }

    /// An interrupted download leaves its .partial-<pid> behind, up to the
    /// ISO's 9 GB. Ones whose process is gone are garbage.
    fn remove_stale_partials(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid) = name.to_str().and_then(|n| n.strip_prefix(".partial-")) else { continue };
            if !Path::new("/proc").join(pid).exists() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Moves a finished download to its place by hash.
    pub fn keep(&self, got: Downloaded) -> Result<PathBuf> {
        let dest = self.path(&got.hash);
        std::fs::rename(&got.path, &dest)?;
        Ok(dest)
    }

    /// Adds a local file the user pointed at (e.g. `--iso`), checking it's
    /// the one the config pins.
    pub fn add_local(&self, path: &Path, hash: &Sri) -> Result<PathBuf> {
        if let Some(p) = self.lookup(hash) {
            return Ok(p);
        }
        eprintln!("hashing {}", path.display());
        let actual = hash_file(path)?;
        if actual != *hash {
            bail!("{} hashes to {actual}, but the config pins {hash}", path.display());
        }
        std::fs::create_dir_all(&self.dir)?;
        let dest = self.path(hash);
        // Hard link when possible: the ISO alone is ~8 GB.
        if std::fs::hard_link(path, &dest).is_err() {
            std::fs::copy(path, &dest)?;
        }
        Ok(dest)
    }

    /// The file a source names, as a path on disk. Members of archives are
    /// unpacked into `work`.
    pub fn materialize(&self, source: &FileSource, work: &Path) -> Result<PathBuf> {
        let whole = match source {
            FileSource::Fetched(f) => self.get(&f.url, &f.hash)?,
            FileSource::Local(l) => {
                if !l.path.exists() {
                    bail!("{} doesn't exist", l.path.display());
                }
                l.path.clone()
            }
        };
        let Some(member) = source.member() else { return Ok(whole) };
        let dest_dir = work.join("members");
        std::fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join(source.file_name()?);
        extract_member(&whole, member, &dest).with_context(|| format!("{member} from {}", whole.display()))?;
        Ok(dest)
    }
}

pub struct Downloaded {
    pub path: PathBuf,
    pub hash: Sri,
}

pub fn hash_file(path: &Path) -> Result<Sri> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    io::copy(&mut f, &mut hasher)?;
    Ok(Sri::from_digest(hasher.finalize().into()))
}

fn extract_member(archive: &Path, member: &str, dest: &Path) -> Result<()> {
    let name = archive.to_string_lossy();
    let file = File::open(archive)?;
    // Cache entries are named by hash, so sniff instead of trusting a suffix.
    let mut magic = [0u8; 6];
    File::open(archive)?.read_exact(&mut magic).ok();
    if magic != *b"\xfd7zXZ\0" {
        bail!("{name} isn't a .tar.xz, the only archive kind supported");
    }
    let mut tar = tar::Archive::new(xz2::read::XzDecoder::new(file));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        if path.trim_start_matches("./") == member {
            let mut out = File::create(dest)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("no {member} in the archive")
}
