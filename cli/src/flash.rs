//! Writes `build image`'s whole-disk image onto a real disk, then grows
//! Windows to fill it. Like flashing an SD card: no VM touches the real
//! disk, and the image is a finished install, so the first boot only has to
//! find the real hardware, like after a motherboard swap.

use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config;
use crate::disk;
use crate::image::{IMAGE_FILE, SIZE_FILE};
use crate::tools::{self, BLKDISCARD, BLOCKDEV, EFIBOOTMGR, FINDMNT, LSBLK, NTFSRESIZE, PARTX, SFDISK, UDEVADM};

#[derive(clap::Args)]
pub struct Args {
    /// A `build image` output directory.
    #[arg(long, value_name = "DIR")]
    image: PathBuf,
    /// Whole disk to erase, by /dev/disk/by-id/ [default: the config's installTarget]
    target: Option<String>,
    /// Configs, for installTarget.
    #[arg(short, long = "config", value_name = "FILE")]
    configs: Vec<PathBuf>,
}

pub fn run(args: &Args) -> Result<()> {
    tools::require(&[LSBLK, FINDMNT, BLKDISCARD, SFDISK, PARTX, BLOCKDEV, UDEVADM, NTFSRESIZE, EFIBOOTMGR])?;
    const USAGE: &str = "dfnstrate flash --image DIR /dev/disk/by-id/<whole disk>  (erases it; a config's installTarget sets a default)";
    let install_target = if args.configs.is_empty() { None } else { config::load(&args.configs, true)?.install_target };
    let disk = disk::unused(disk::target(args.target.as_deref(), install_target, USAGE)?)?;
    let (target, dev, sys) = (disk.target.as_str(), disk.dev.as_path(), &disk.sys);
    let die = |msg: String| -> Result<()> { bail!("{msg}") };

    let image_size: u64 = std::fs::read_to_string(args.image.join(SIZE_FILE))
        .with_context(|| format!("{} isn't a `build image` output", args.image.display()))?
        .trim()
        .parse()?;
    let disk_size = std::fs::read_to_string(sys.join("size"))?.trim().parse::<u64>()? * 512;
    if disk_size < image_size {
        die(format!("{} is {} GiB, the image needs {} GiB", dev.display(), disk_size >> 30, image_size >> 30))?;
    }
    if !Path::new("/sys/firmware/efi/efivars").is_dir() {
        die("not booted in UEFI mode, so there's no firmware boot entry to add".into())?;
    }

    // Only writing the disk needs root.
    if !disk::is_root() {
        disk::rerun_as_root(args.target.is_none().then(|| target.into()))?;
    }

    std::process::Command::new(LSBLK.path().unwrap()).args(["-o", "NAME,SIZE,MODEL,SERIAL,FSTYPE,LABEL"]).arg(dev).status()?;
    println!("\nEverything on {target} will be erased.");
    print!("Type ERASE to install Windows onto it: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != "ERASE" {
        die("not confirmed, nothing touched".into())?;
    }

    // Discarding first means nothing of the old contents survives past the
    // image, and the SSD knows the rest is free.
    if tools::run(BLKDISCARD.cmd().arg("-f").arg(dev)).is_err() {
        eprintln!("discard unsupported, writing over the old contents");
    }
    write_image(&args.image.join(IMAGE_FILE), dev, image_size)?;

    // The image is smaller than the disk: put the backup GPT at the disk's
    // real end, then grow Windows, the last partition, into the rest. sfdisk
    // resizes in place, keeping the partition GUID the BCD boots by.
    tools::run(SFDISK.cmd().args(["--relocate", "gpt-bak-std"]).arg(dev))?;
    tools::run_with_input(SFDISK.cmd().args(["--wipe-partitions", "never", "-N", &disk::WINDOWS_PART.to_string()]).arg(dev), ", +\n")?;
    // sfdisk re-reads the table itself; a second BLKRRPART races udev's probe
    // of the new partitions and fails with EBUSY. partx updates the kernel's
    // view partition by partition, which works on a busy disk.
    tools::run(UDEVADM.cmd().arg("settle"))?;
    tools::run(PARTX.cmd().arg("-u").arg(dev))?;
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let windows = disk.partition(disk::WINDOWS_PART);
    if !std::fs::metadata(&windows).is_ok_and(|m| m.file_type().is_block_device()) {
        die(format!("{windows} didn't show up after the resize"))?;
    }
    // ntfsresize grows to whatever size the kernel reports, so a stale view
    // would quietly leave Windows at the image's size.
    let part_size: u64 = tools::output_string(BLOCKDEV.cmd().arg("--getsize64").arg(&windows))?.trim().parse()?;
    if part_size <= image_size {
        die(format!("the kernel still sees {windows} at its old size; rerun to finish"))?;
    }
    // Leaves the volume marked for a check, so Windows runs chkdsk once on
    // first boot. That's ntfsresize being careful, not a problem.
    tools::run_with_input(NTFSRESIZE.cmd().args(["--force", "--no-progress-bar"]).arg(&windows), "y\n")?;

    // Each flash gives the ESP a new partition UUID, which leaves the
    // previous flash's entry pointing at nothing. Drop every Windows Boot
    // Manager entry whose ESP isn't on any disk here; ones for a Windows that
    // still exists, on this disk or another, stay.
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let entries = tools::output_string(EFIBOOTMGR.cmd().arg("-v"))?;
    for (num, uuid) in windows_entries(&entries) {
        if !Path::new("/dev/disk/by-partuuid").join(&uuid).exists() {
            tools::run(EFIBOOTMGR.cmd().args(["--bootnum", &num, "--delete-bootnum"]))?;
            println!("removed stale firmware entry Boot{num} (its ESP {uuid} is gone)");
        }
    }

    // The image has no firmware entry of its own. --create-only leaves
    // BootOrder alone, so the existing OS still boots by default.
    let esp = disk.partition(1);
    let partuuid = tools::output_string(LSBLK.cmd().args(["-no", "PARTUUID"]).arg(&esp))?.trim().to_owned();
    if partuuid.is_empty() {
        die(format!("{esp} has no partition UUID, so there's nothing to point a boot entry at"))?;
    }
    let entries = tools::output_string(EFIBOOTMGR.cmd().arg("-v"))?;
    if entries.to_lowercase().contains(&partuuid.to_lowercase()) {
        println!("firmware already has a boot entry for {esp}");
    } else {
        tools::run(EFIBOOTMGR.cmd().args(["--create-only", "--disk"]).arg(dev).args([
            "--part",
            "1",
            "--label",
            "Windows Boot Manager",
            "--loader",
            "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
        ]))?;
        println!("added a \"Windows Boot Manager\" firmware entry for {esp}");
    }

    println!("Done. Windows is fully set up on it; boot it and it comes up at the shell.");
    Ok(())
}

fn write_image(image: &Path, dev: &Path, size: u64) -> Result<()> {
    println!("writing {} GiB", size >> 30);
    let mut input = zstd::Decoder::new(File::open(image).with_context(|| image.display().to_string())?)?;
    let mut out = File::options().write(true).open(dev).with_context(|| dev.display().to_string())?;
    let mut buf = vec![0u8; 16 << 20];
    let mut done: u64 = 0;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        out.write_all(&buf[..n])?;
        let before = done;
        done += n as u64;
        if done >> 30 != before >> 30 {
            print!("\r{} / {} GiB", done >> 30, size >> 30);
            std::io::stdout().flush()?;
        }
    }
    println!();
    out.sync_all()?;
    if done != size {
        bail!("the image decompressed to {done} bytes, its size file says {size}");
    }
    Ok(())
}

/// (Boot number, ESP partition UUID) of each "Windows Boot Manager" entry
/// in `efibootmgr -v` output.
fn windows_entries(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Boot")?;
            let (num, rest) = rest.split_at_checked(4)?;
            if !num.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let rest = rest.strip_prefix('*').unwrap_or(rest);
            if !rest.starts_with(" Windows Boot Manager") {
                return None;
            }
            let hd = &rest[rest.find("HD(")? + 3..];
            let fields: Vec<&str> = hd.split(',').collect();
            if fields.get(1) != Some(&"GPT") {
                return None;
            }
            Some((num.to_owned(), fields.get(2)?.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_windows_entries() {
        let text = "BootCurrent: 0001\nBoot0000* Windows Boot Manager\tHD(1,GPT,0fd4e1d5-aaaa-bbbb-cccc-0123456789ab,0x800,0x96000)/File(\\EFI\\Microsoft\\Boot\\bootmgfw.efi)\nBoot0001* NixOS\tHD(1,GPT,11111111-2222-3333-4444-555555555555,0x800,0x100000)/File(\\EFI\\BOOT\\BOOTX64.EFI)\n";
        assert_eq!(super::windows_entries(text), vec![("0000".to_owned(), "0fd4e1d5-aaaa-bbbb-cccc-0123456789ab".to_owned())]);
    }
}
