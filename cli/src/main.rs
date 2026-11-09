//! dfnstrate: builds defenestrate (a declarative Windows) from Microsoft's
//! stock ISO, and flashes it. The Nix flake runs exactly these commands.

mod cache;
mod config;
mod fetch;
mod flash;
mod image;
mod iso;
mod registry;
mod render;
mod tasks;
mod tools;
mod unattend;
mod vm;
mod wsl;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use cache::Cache;
use config::Config;
use unattend::Variant;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Args)]
struct ConfigArgs {
    /// Config files, layered in order over the built-in defaults
    /// (configs/default.toml). TOML, or JSON as the Nix module writes it.
    #[arg(short, long = "config", value_name = "FILE")]
    configs: Vec<PathBuf>,
    /// Leave out the built-in defaults.
    #[arg(long)]
    no_defaults: bool,
}

impl ConfigArgs {
    fn load(&self) -> Result<Config> {
        config::load(&self.configs, !self.no_defaults)
    }
}

#[derive(clap::Args)]
struct CacheArgs {
    /// Where downloads are kept, by hash [default: $XDG_CACHE_HOME/dfnstrate]
    #[arg(long, env = "DFNSTRATE_CACHE", value_name = "DIR")]
    cache: Option<PathBuf>,
    /// Never download; everything must already be in the cache.
    #[arg(long)]
    offline: bool,
}

impl CacheArgs {
    fn open(&self) -> Result<Cache> {
        Cache::new(self.cache.clone(), self.offline)
    }
}

#[derive(clap::Args)]
struct BuildArgs {
    #[command(flatten)]
    config: ConfigArgs,
    #[command(flatten)]
    cache: CacheArgs,
    #[command(flatten)]
    source: fetch::SourceArgs,
    /// Output directory.
    #[arg(short, long)]
    out: PathBuf,
    /// Scratch space (~25 GB) [default: next to --out, removed afterwards]
    #[arg(long)]
    work_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download the stock ISO the config pins into the cache.
    Fetch {
        #[command(flatten)]
        config: ConfigArgs,
        #[command(flatten)]
        cache: CacheArgs,
        #[command(flatten)]
        args: fetch::Args,
    },
    /// Build install media.
    #[command(subcommand)]
    Build(BuildCmd),
    /// Write what the config generates (registry, answer file, apps) to a
    /// directory, without building anything.
    Render {
        #[command(flatten)]
        config: ConfigArgs,
        #[command(flatten)]
        cache: CacheArgs,
        /// Render the variant `build image` uses.
        #[arg(long)]
        for_image: bool,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Boot an ISO in a throwaway QEMU VM (Secure Boot, TPM 2.0).
    Vm(vm::Args),
    /// Write a built image onto a whole disk, like an SD card.
    Flash(flash::Args),
}

#[derive(Subcommand)]
enum BuildCmd {
    /// An install ISO, for a USB stick or a VM: out/defenestrate.iso.
    Iso {
        #[command(flatten)]
        build: BuildArgs,
        /// Build the variant `build image` installs from (wipes disk 0,
        /// powers off after first logon).
        #[arg(long)]
        for_image: bool,
    },
    /// A finished whole-disk install, set up in a sandboxed VM, for `flash`:
    /// out/windows.img.zst and out/size. Needs KVM.
    Image {
        #[command(flatten)]
        build: BuildArgs,
        #[command(flatten)]
        image: image::Args,
        /// A `build iso --for-image` output to install from instead of
        /// building one.
        #[arg(long, value_name = "DIR")]
        from_iso: Option<PathBuf>,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("dfnstrate: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Fetch { config, cache, args } => {
            let pin = config::load_iso(&config.configs, !config.no_defaults)?;
            fetch::run(&pin, &cache.open()?, &args)
        }
        Cmd::Build(BuildCmd::Iso { build, for_image }) => {
            let config = build.config.load()?;
            let cache = build.cache.open()?;
            let variant = if for_image { Variant::for_image() } else { Variant::from_config(&config) };
            with_work(&build, |work| {
                let source = build.source.resolve(&config.iso, &cache)?;
                iso::build(&config, &variant, &source, &cache, work, &build.out)
            })
        }
        Cmd::Build(BuildCmd::Image { build, image, from_iso }) => {
            let config = build.config.load()?;
            let cache = build.cache.open()?;
            let ready = image::preflight(&image)?;
            with_work(&build, |work| {
                let iso_dir = match &from_iso {
                    Some(d) => d.clone(),
                    None => {
                        tools::require(iso::TOOLS)?;
                        let source = build.source.resolve(&config.iso, &cache)?;
                        let d = work.join("iso-out");
                        iso::build(&config, &Variant::for_image(), &source, &cache, &work.join("iso-work"), &d)?;
                        std::fs::remove_dir_all(work.join("iso-work"))?;
                        d
                    }
                };
                image::build(&config, &image, ready, &iso_dir.join(iso::ISO_NAME), work, &build.out)
            })
        }
        Cmd::Render { config, cache, for_image, out } => {
            let config = config.load()?;
            let cache = cache.open()?;
            let variant = if for_image { Variant::for_image() } else { Variant::from_config(&config) };
            let work = out.join(".work");
            render::render(&config, &variant, &cache, &work, &out)?;
            std::fs::remove_dir_all(&work).ok();
            Ok(())
        }
        Cmd::Vm(args) => vm::run(&args),
        Cmd::Flash(args) => flash::run(&args),
    }
}

/// Runs `f` with a scratch directory, removed afterwards unless the user
/// picked it (Nix builds in the sandbox's own scratch).
fn with_work(build: &BuildArgs, f: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    if let Some(w) = &build.work_dir {
        std::fs::create_dir_all(w)?;
        return f(w);
    }
    let out = std::path::absolute(&build.out)?;
    let parent = out.parent().context("--out has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let work = parent.join(format!(".{}.work", out.file_name().context("--out has no name")?.to_string_lossy()));
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let result = f(&work);
    if let Err(e) = std::fs::remove_dir_all(&work) {
        eprintln!("warning: couldn't remove {}: {e}", work.display());
    }
    result
}
