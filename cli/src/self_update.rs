//! `dfnstrate self-update`: replaces this binary with the latest GitHub
//! release, the way it got here. Only in the release binaries (the
//! `self-update` feature, which dist-workspace.toml turns on): the AUR and
//! Nix packages are updated by their package managers instead.
//!
//! axoupdater reads the receipt the `curl | sh` installer wrote
//! ($XDG_CONFIG_HOME/dfnstrate/dfnstrate-receipt.json) and runs the new
//! release's own installer into the same directory.

use anyhow::{Result, anyhow};
use axoupdater::{AxoUpdater, AxoupdateError};

#[derive(clap::Args)]
pub struct Args {
    /// Only say whether a newer release exists.
    #[arg(long)]
    check: bool,
}

pub fn run(args: &Args) -> Result<()> {
    let mut updater = AxoUpdater::new_for(env!("CARGO_PKG_NAME"));
    match updater.load_receipt() {
        Ok(_) => {}
        Err(AxoupdateError::NoReceipt { .. }) => {
            return Err(anyhow!(
                "this dfnstrate wasn't installed by the release installer, so there's nothing to update in place; \
                 update it the way you installed it (README, Install)"
            ));
        }
        Err(e) => return Err(e.into()),
    }
    // A receipt from an install elsewhere (another copy on PATH) would make
    // the installer update that one, not the binary that was run.
    if !updater.check_receipt_is_for_this_executable()? {
        return Err(anyhow!(
            "this binary isn't the one the release installer put in place; run `self-update` from that one, \
             or reinstall"
        ));
    }
    // Unauthenticated GitHub API calls are limited to 60 an hour per IP,
    // which shared CI or NAT can run out of.
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        updater.set_github_token(&token);
    }
    if args.check {
        match updater.is_update_needed_sync()? {
            true => println!("a newer dfnstrate release is out; `dfnstrate self-update` installs it"),
            false => println!("dfnstrate {} is the latest release", env!("CARGO_PKG_VERSION")),
        }
        return Ok(());
    }
    match updater.run_sync()? {
        Some(r) => println!("dfnstrate updated to {} in {}", r.new_version, r.install_prefix),
        None => println!("dfnstrate {} is already the latest release", env!("CARGO_PKG_VERSION")),
    }
    Ok(())
}
