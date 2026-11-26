//! `dfnstrate logs`: copies what Windows wrote about itself off the shut-down
//! Windows partition, read-only, for finding out from Linux why a boot went
//! wrong (saturn's test boots kept coming back to Linux early, with nothing
//! on the Linux side saying why). defenestrate's own logs always; anything
//! else under C:\ with `--also`.

use std::os::unix::fs::chown;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::ntfs::{self, Mount, VolumeArgs};
use crate::tools::{self, NTFS3G, NTFS3G_PROBE};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    volume: VolumeArgs,
    /// Where the copies go, as C:\'s layout under it.
    #[arg(short, long)]
    out: PathBuf,
    /// More to copy: a file or directory under C:\, with / separators
    /// (Users/me/.claude/projects, say). Missing ones are skipped.
    #[arg(long, value_name = "PATH")]
    also: Vec<String>,
}

const MAX_SIZE: u64 = 50 << 20;

const USAGE: &str = "dfnstrate logs -c CONFIG... --out DIR /dev/disk/by-id/<the disk Windows is on>";

/// Where defenestrate's own logs are, under C:\.
fn ours(user: &str) -> [String; 3] {
    [format!("Users/{user}/AppData/Local/defenestrate"), "ProgramData/defenestrate".into(), crate::render::SETUP_DIR.rel().into()]
}

/// Plain relative components only: `--also` mustn't reach outside the volume.
fn plain(path: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    (!parts.is_empty() && parts.iter().all(|p| !p.is_empty() && *p != "." && *p != "..")).then(|| parts.iter().collect())
}

pub fn run(config: &Config, args: &Args) -> Result<()> {
    tools::require(&[NTFS3G, NTFS3G_PROBE])?;
    let mut wanted = Vec::new();
    for p in ours(config.user.name.as_str()).iter().chain(&args.also) {
        wanted.push(plain(p).with_context(|| format!("--also {p:?} isn't a plain path under C:\\"))?);
    }
    let out = std::path::absolute(&args.out)?;
    let volume = args.volume.volume(config, USAGE, || Ok(Vec::new()))?;

    // The copies are the caller's, not root's, when sudo ran this: --out
    // and whichever of its parents this makes, never one that was there.
    let owner = sudo_caller();
    let existed = out.ancestors().find(|a| a.exists()).map(Path::to_path_buf);
    std::fs::create_dir_all(&out)?;
    if let Some((uid, gid)) = owner {
        for d in out.ancestors().take_while(|a| Some(a.to_path_buf()) != existed) {
            chown(d, Some(uid), Some(gid))?;
        }
    }

    ntfs::probe(&volume, false)?;
    let mount = Mount::new(&volume, std::env::temp_dir().join(format!("dfnstrate-logs-{}", std::process::id())), false)?;
    let mut copied = 0;
    for rel in &wanted {
        let from = mount.dir.join(rel);
        if !from.exists() {
            println!("no C:\\{}", rel.display().to_string().replace('/', "\\"));
            continue;
        }
        copied += copy(&from, &out.join(rel), owner)?;
    }
    mount.unmount()?;
    // Everything under --out, the parents of each copied path included
    // (create_dir_all made those as root).
    if let Some(owner) = owner {
        chown_tree(&out, owner)?;
    }
    println!("{copied} files into {}", out.display());
    Ok(())
}

pub(crate) fn chown_tree(dir: &Path, (uid, gid): (u32, u32)) -> Result<()> {
    std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
    if std::fs::symlink_metadata(dir)?.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            chown_tree(&entry?.path(), (uid, gid))?;
        }
    }
    Ok(())
}

pub(crate) fn sudo_caller() -> Option<(u32, u32)> {
    let id = |v| std::env::var(v).ok()?.parse().ok();
    Some((id("SUDO_UID")?, id("SUDO_GID")?))
}

/// Files and directories, not links (NTFS junctions point anywhere).
fn copy(from: &Path, to: &Path, owner: Option<(u32, u32)>) -> Result<usize> {
    let meta = std::fs::symlink_metadata(from)?;
    let made = |p: &Path| owner.map_or(Ok(()), |(u, g)| chown(p, Some(u), Some(g)));
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        made(to)?;
        let mut n = 0;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            n += copy(&entry.path(), &to.join(entry.file_name()), owner)?;
        }
        return Ok(n);
    }
    if !meta.is_file() {
        return Ok(0);
    }
    // Not logs: the WSL MSI, the distro's ext4.vhdx, DISM's 250 MB log of
    // the update it added at setup (all under the dirs above).
    if meta.len() > MAX_SIZE {
        println!("skipped {} ({} MB)", from.display(), meta.len() >> 20);
        return Ok(0);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    made(to)?;
    Ok(1)
}
