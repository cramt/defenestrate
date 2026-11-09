//! A whole-disk Windows image: the whole unattended install (windowsPE,
//! specialize, OOBE, first logon's shell switch) runs in a VM against a
//! scratch disk, offline, and ends with first logon powering the VM off.
//! The real machine's first boot is then just a boot: Windows finds its new
//! hardware the way it does after a motherboard swap. The disk is NVMe so
//! the image boots on a real NVMe controller through the same inbox driver
//! it was installed with, no sysprep needed.

use std::fs::File;
use std::io::{BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::tools::{self, ICAT, IFIND, MDIR, QEMU, SFDISK, SWTPM};
use crate::vm::{self, Firmware, Tpm};

pub const IMAGE_FILE: &str = "windows.img.zst";
pub const SIZE_FILE: &str = "size";

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    firmware: Firmware,
    /// Size of the image's disk. `flash` grows Windows to fill the real one.
    #[arg(long, default_value = "64G", value_parser = parse_size)]
    size: u64,
}

fn parse_size(s: &str) -> Result<u64, String> {
    let (num, mult) = match s.as_bytes().last() {
        Some(b'G') => (&s[..s.len() - 1], 1u64 << 30),
        Some(b'M') => (&s[..s.len() - 1], 1 << 20),
        _ => (s, 1),
    };
    num.parse::<u64>().map(|n| n * mult).map_err(|_| format!("{s:?} isn't a size like 64G"))
}

/// What `build` needs, checked before anything slow (like building the ISO
/// it installs from) starts.
pub struct Ready {
    code: PathBuf,
    vars_template: PathBuf,
}

pub fn preflight(args: &Args) -> Result<Ready> {
    tools::require(&[QEMU, SWTPM, SFDISK, MDIR, IFIND, ICAT])?;
    File::options().read(true).write(true).open("/dev/kvm").context("/dev/kvm: the install runs in a KVM VM")?;
    let (code, vars_template) = args.firmware.resolve()?;
    Ok(Ready { code, vars_template })
}

pub fn build(config: &Config, args: &Args, ready: Ready, iso: &Path, work: &Path, out: &Path) -> Result<()> {
    let Ready { code, vars_template } = ready;
    std::fs::create_dir_all(out)?;

    let disk = work.join("disk.raw");
    {
        let f = File::create(&disk)?;
        // Build dirs can sit on btrfs, where a raw disk being rewritten in
        // place fragments badly. Only takes on an empty file.
        tools::no_cow(&disk);
        f.set_len(args.size)?;
    }
    let vars = work.join("OVMF_VARS.fd");
    vm::copy_writable(&vars_template, &vars)?;
    let tpm = Tpm::start(&work.join("tpm"), &work.join("swtpm.sock"))?;
    let monitor = work.join("monitor.sock");

    // Disk first: blank, it falls through to the CD; installed, it boots
    // Windows instead of starting setup over. No network, so OOBE skips its
    // update check instead of waiting on it.
    let mut qemu = vm::qemu_base("defenestrate-image", &code, &vars, &tpm.socket);
    qemu.arg("-drive")
        .arg(format!("id=disk,file={},if=none,format=raw,cache=unsafe", vm::drive_path(&disk)?))
        .args(["-device", "nvme,drive=disk,serial=defenestrate,bootindex=0"])
        .args(["-device", "ahci,id=ahci"])
        .arg("-drive")
        .arg(format!("id=cd,file={},if=none,media=cdrom,readonly=on", vm::drive_path(iso)?))
        .args(["-device", "ide-cd,drive=cd,bus=ahci.0,bootindex=1"])
        .args(["-nic", "none"])
        .args(["-device", "qemu-xhci", "-device", "usb-tablet"])
        .args(["-display", "none"])
        .arg("-monitor")
        .arg(format!("unix:{},server=on,wait=off", monitor.display()));
    let mut child = qemu.spawn().context("starting QEMU")?;

    // Unattended setup either writes steadily or is stuck on a dialog nobody
    // will click. Fail on a stall instead of hanging, and leave screenshots
    // in the work dir.
    const GIB: u64 = 1 << 30;
    let allocated = || std::fs::metadata(&disk).map(|m| m.blocks() * 512).unwrap_or(0);
    let screendump = |name: &str| {
        let path = work.join(format!("screen-{name}.png"));
        if let Ok(mut s) = UnixStream::connect(&monitor) {
            let _ = writeln!(s, "screendump {} -f png", path.display());
            std::thread::sleep(Duration::from_secs(2));
        }
    };
    let fail = |child: &mut std::process::Child, msg: String| -> Result<()> {
        let _ = child.kill();
        let _ = child.wait();
        bail!("{msg} (screenshots in {})", work.display())
    };
    let mut last = allocated();
    let (mut idle, mut elapsed) = (0u64, 0u64);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("QEMU exited with {status}");
            }
            break;
        }
        std::thread::sleep(Duration::from_secs(30));
        elapsed += 30;
        let now = allocated();
        idle = if now == last { idle + 30 } else { 0 };
        last = now;
        eprintln!("{}m: {} GiB written", elapsed / 60, now / GIB);
        if elapsed % 300 == 0 {
            screendump(&format!("{}m", elapsed / 60));
        }
        // Before setup touches the disk (boot, WinPE start) quiet is normal.
        if now > GIB && idle >= 1200 {
            screendump("stalled");
            fail(&mut child, "nothing written for 20 minutes, setup is stuck".into())?;
        }
        // Setup applies the image within ~15 minutes even on a loaded host;
        // nothing by now means it stopped on a dialog before touching the disk.
        if now <= GIB && elapsed >= 3600 {
            screendump("stalled");
            fail(&mut child, "setup hasn't written to the disk in an hour, it's stuck before the install".into())?;
        }
        if elapsed >= 3 * 3600 {
            screendump("timeout");
            fail(&mut child, "setup took over 3 hours".into())?;
        }
    }
    drop(tpm);

    verify(config, &disk)?;

    eprintln!("compressing the image");
    std::fs::write(out.join(SIZE_FILE), format!("{}\n", args.size))?;
    let mut input = BufReader::with_capacity(16 << 20, File::open(&disk)?);
    let mut encoder = zstd::Encoder::new(File::create(out.join(IMAGE_FILE))?, 3)?;
    encoder.multithread(std::thread::available_parallelism().map_or(1, |n| n.get() as u32))?;
    std::io::copy(&mut input, &mut encoder)?;
    encoder.finish()?.sync_all()?;
    std::fs::remove_file(&disk)?;
    Ok(())
}

/// A power-off only means something shut Windows down, so check what's on
/// the disk: Windows Boot Manager on its own ESP, and the log first logon
/// writes, read straight off NTFS. That last one also fails on a volume
/// BitLocker got to, which would be a brick once flashed.
fn verify(config: &Config, disk: &Path) -> Result<()> {
    let table: serde_json::Value = serde_json::from_slice(&tools::output(SFDISK.cmd().arg("--json").arg(disk))?)?;
    let start = |i: usize| table["partitiontable"]["partitions"][i]["start"].as_u64().with_context(|| format!("the image has no partition {}", i + 1));
    let (esp, windows) = (start(0)?, start(2)?);

    tools::run(MDIR.cmd().arg("-i").arg(format!("{}@@{}", disk.display(), esp * 512)).arg("::/EFI/Microsoft/Boot/bootmgfw.efi"))
        .context("setup didn't finish: no Windows Boot Manager on the image's ESP")?;
    let inode = tools::output_string(IFIND.cmd().arg("-o").arg(windows.to_string()).arg("-n").arg("/Windows/Setup/defenestrate/firstlogon.log").arg(disk))
        .context("setup didn't finish: first logon never ran (or C: isn't readable NTFS)")?;

    if config.has_apps() {
        // Without the tasks, the apps in the image would never get installed.
        let log = tools::output(ICAT.cmd().arg("-o").arg(windows.to_string()).arg(disk).arg(inode.trim()))?;
        let log = String::from_utf8_lossy(&log);
        eprint!("{log}");
        for task in ["defenestrate-apps", "defenestrate-apps-user"] {
            if !log.contains(&format!("\"{task}\" has successfully been created")) {
                bail!("first logon didn't register the {task} task");
            }
        }
    }
    Ok(())
}
