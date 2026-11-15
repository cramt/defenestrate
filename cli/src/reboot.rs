//! `dfnstrate reboot`: into Windows once, like picking it from the
//! firmware's boot menu. BootNext lasts a single boot, so the reboot after
//! that is back on the normal BootOrder, which `flash` leaves on Linux.

use std::path::Path;

use anyhow::{Result, bail};

use crate::disk;
use crate::firmware::{self, BootNum, WindowsEntry};
use crate::tools::{self, EFIBOOTMGR, SYSTEMCTL};

#[derive(clap::Args)]
pub struct Args {
    /// Firmware entry to boot, the XXXX of BootXXXX [default: the Windows
    /// Boot Manager entry whose ESP is on a disk here]
    #[arg(long, value_name = "XXXX")]
    entry: Option<BootNum>,
    /// Only set BootNext, and reboot later.
    #[arg(long)]
    no_reboot: bool,
}

pub fn run(args: &Args) -> Result<()> {
    tools::require(&[EFIBOOTMGR, SYSTEMCTL])?;
    if !Path::new("/sys/firmware/efi/efivars").is_dir() {
        bail!("not booted in UEFI mode, so there's no BootNext to set");
    }
    // Picked before sudo, so a machine with nothing to boot fails without
    // asking for a password.
    let entry = match &args.entry {
        Some(n) => n.clone(),
        None => pick(firmware::windows_entries(&tools::output_string(EFIBOOTMGR.cmd().arg("-v"))?), WindowsEntry::live)?,
    };
    if !disk::is_root() {
        disk::rerun_as_root(None)?;
    }
    tools::run(EFIBOOTMGR.cmd().arg("--bootnext").arg(&entry))?;
    if args.no_reboot {
        println!("the next boot is Windows (Boot{entry}); the one after it is back to normal");
        return Ok(());
    }
    println!("rebooting into Windows (Boot{entry})");
    tools::run(SYSTEMCTL.cmd().arg("reboot"))
}

fn pick(entries: Vec<WindowsEntry>, live: impl Fn(&WindowsEntry) -> bool) -> Result<BootNum> {
    let total = entries.len();
    let mut live: Vec<WindowsEntry> = entries.into_iter().filter(|e| live(e)).collect();
    // Windows' first boot adds its own entry for the ESP flash already has
    // one for. Both start the same bootmgfw.efi, so that's one Windows.
    let mut seen = Vec::new();
    live.retain(|e| {
        let new = !seen.contains(&e.esp);
        seen.push(e.esp.clone());
        new
    });
    match (live.len(), total) {
        (1, _) => Ok(live.remove(0).num),
        (0, 0) => bail!("the firmware has no Windows Boot Manager entry; `dfnstrate flash` adds one"),
        (0, _) => bail!("every Windows Boot Manager entry points at an ESP that isn't on any disk here"),
        _ => {
            let list: Vec<String> = live.iter().map(|e| format!("  Boot{} (ESP {})", e.num, e.esp)).collect();
            bail!("more than one Windows here; pick one with --entry:\n{}", list.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(num: &str, esp: &str) -> WindowsEntry {
        WindowsEntry { num: num.parse().unwrap(), esp: esp.into() }
    }

    #[test]
    fn picks_the_one_live_windows() {
        let entries = vec![entry("0000", "gone"), entry("0003", "here")];
        assert_eq!(pick(entries, |e| e.esp == "here").unwrap(), "0003".parse().unwrap());
    }

    #[test]
    fn one_esp_is_one_windows() {
        let entries = vec![entry("0000", "esp"), entry("0004", "esp")];
        assert_eq!(pick(entries, |_| true).unwrap(), "0000".parse().unwrap());
    }

    #[test]
    fn refuses_to_guess() {
        assert!(pick(vec![], |_| true).unwrap_err().to_string().contains("flash"));
        assert!(pick(vec![entry("0000", "gone")], |_| false).unwrap_err().to_string().contains("isn't on any disk"));
        let two = vec![entry("0000", "a"), entry("0003", "b")];
        let e = pick(two, |_| true).unwrap_err().to_string();
        assert!(e.contains("Boot0000") && e.contains("Boot0003"), "{e}");
    }
}
