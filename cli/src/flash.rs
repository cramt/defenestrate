//! Writes `build image`'s whole-disk image onto a real disk, then grows
//! Windows to fill it. Like flashing an SD card: no VM touches the real
//! disk, and the image is a finished install, so the first boot only has to
//! find the real hardware, like after a motherboard swap.

use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::{FileExt, FileTypeExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config;
use crate::disk::{self, Disk};
use crate::firmware;
use crate::gpt::{self, ImagePart};
use crate::image::{IMAGE_FILE, SIZE_FILE};
use crate::tools::{self, BLKDISCARD, BLOCKDEV, EFIBOOTMGR, FINDMNT, LSBLK, MCOPY, NTFSCAT, NTFSRESIZE, PARTX, SFDISK, UDEVADM};

pub const TOOLS: &[tools::Tool] = &[LSBLK, FINDMNT, BLKDISCARD, SFDISK, PARTX, BLOCKDEV, UDEVADM, NTFSRESIZE, EFIBOOTMGR, MCOPY, NTFSCAT];

#[derive(clap::Args)]
pub struct Args {
    /// A `build image` output directory.
    #[arg(long)]
    image: PathBuf,
    /// Whole disk to write, by /dev/disk/by-id/ [default: the config's installTarget]
    target: Option<String>,
    /// Keep everything on the disk and put Windows into its unpartitioned
    /// space instead of erasing it. Linux can stay mounted.
    #[arg(long)]
    into_free_space: bool,
    /// Configs, for installTarget.
    #[arg(short, long = "config", value_name = "FILE")]
    configs: Vec<PathBuf>,
}

pub fn run(args: &Args) -> Result<()> {
    tools::require(TOOLS)?;
    const USAGE: &str = "dfnstrate flash --image DIR [--into-free-space] /dev/disk/by-id/<whole disk>  (a config's installTarget sets a default)";
    let install_target = if args.configs.is_empty() { None } else { config::load(&args.configs, true)?.install_target };
    let target = disk::target(args.target.as_deref(), install_target, USAGE)?;
    let disk = if args.into_free_space { disk::partitioned(target)? } else { disk::unused(target)? };

    let image_size: u64 = std::fs::read_to_string(args.image.join(SIZE_FILE))
        .with_context(|| format!("{} isn't a `build image` output", args.image.display()))?
        .trim()
        .parse()?;
    if !Path::new("/sys/firmware/efi/efivars").is_dir() {
        bail!("not booted in UEFI mode, so there's no firmware boot entry to add");
    }

    // Only reading and writing the disk needs root.
    if !disk::is_root() {
        disk::rerun_as_root(args.target.is_none().then(|| disk.target.as_str().into()))?;
    }

    let image = args.image.join(IMAGE_FILE);
    let (esp, windows) = if args.into_free_space { into_free_space(&disk, &image, image_size)? } else { whole_disk(&disk, &image, image_size)? };
    fit_boot_manager(&disk.partition(esp), &disk.partition(windows))?;
    add_boot_entry(&disk, esp)?;
    println!("Done. Windows is fully set up on it; boot it and it comes up at the shell.");
    Ok(())
}

fn confirm(word: &str, what: &str) -> Result<()> {
    print!("Type {word} to {what}: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != word {
        bail!("not confirmed, nothing touched");
    }
    Ok(())
}

/// Erases the disk for the image, growing Windows to fill it. Returns the
/// ESP's and Windows' partition numbers.
fn whole_disk(disk: &Disk, image: &Path, image_size: u64) -> Result<(u32, u32)> {
    let (target, dev) = (disk.target.as_str(), disk.dev.as_path());
    let disk_size = std::fs::read_to_string(disk.sys.join("size"))?.trim().parse::<u64>()? * 512;
    if disk_size < image_size {
        bail!("{} is {} GiB, the image needs {} GiB", dev.display(), disk_size >> 30, image_size >> 30);
    }
    std::process::Command::new(LSBLK.path().unwrap()).args(["-o", "NAME,SIZE,MODEL,SERIAL,FSTYPE,LABEL"]).arg(dev).status()?;
    println!("\nEverything on {target} will be erased.");
    confirm("ERASE", "install Windows onto it")?;

    // Discarding first means nothing of the old contents survives past the
    // image, and the SSD knows the rest is free.
    if tools::run(BLKDISCARD.cmd().arg("-f").arg(dev)).is_err() {
        eprintln!("discard unsupported, writing over the old contents");
    }
    write_image(image, dev, image_size)?;

    // The image is smaller than the disk: put the backup GPT at the disk's
    // real end, then grow Windows, the last partition, into the rest. sfdisk
    // resizes in place, keeping the partition GUID the BCD boots by.
    tools::run(SFDISK.cmd().args(["--relocate", "gpt-bak-std"]).arg(dev))?;
    tools::run_with_input(SFDISK.cmd().args(["--wipe-partitions", "never", "-N", &ImagePart::Windows.number().to_string()]).arg(dev), ", +\n")?;
    // sfdisk re-reads the table itself; a second BLKRRPART races udev's probe
    // of the new partitions and fails with EBUSY. partx updates the kernel's
    // view partition by partition, which works on a busy disk.
    tools::run(UDEVADM.cmd().arg("settle"))?;
    tools::run(PARTX.cmd().arg("-u").arg(dev))?;
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let windows = disk.partition(ImagePart::Windows.number());
    if !std::fs::metadata(&windows).is_ok_and(|m| m.file_type().is_block_device()) {
        bail!("{windows} didn't show up after the resize");
    }
    // ntfsresize grows to whatever size the kernel reports, so a stale view
    // would quietly leave Windows at the image's size.
    let part_size: u64 = tools::output_string(BLOCKDEV.cmd().arg("--getsize64").arg(&windows))?.trim().parse()?;
    if part_size <= image_size {
        bail!("the kernel still sees {windows} at its old size; rerun to finish");
    }
    grow_ntfs(&windows)?;
    Ok((ImagePart::Esp.number(), ImagePart::Windows.number()))
}

/// Adds the image's partitions in the disk's one big free region, leaving
/// every existing partition alone, and grows Windows to fill the region.
/// Returns the new ESP's and Windows' partition numbers.
///
/// Windows gets its own ESP rather than a share of Linux's: a NixOS ESP is
/// often nearly full of kernels, Windows updates rewrite the ESP they boot
/// from (\EFI\Boot\bootx64.efi included, which may be Linux's fallback
/// loader), and copying the image's ESP whole means no FAT merging. UEFI
/// allows any number of ESPs per disk; the firmware entry names which.
///
/// The partitions keep the image's partition GUIDs: Windows' MountedDevices
/// finds C: by it, so a new one would leave C: unassigned. The disk keeps
/// its own GUID, Linux's.
fn into_free_space(disk: &Disk, image: &Path, image_size: u64) -> Result<(u32, u32)> {
    let (target, dev) = (disk.target.as_str(), disk.dev.as_path());
    let table = gpt::parse(&tools::output_string(SFDISK.cmd().arg("--json").arg(dev))?)?;
    // Partitions resized while mounted (shrinking Linux to make the space)
    // can leave the kernel with the old extents. A mounted filesystem would
    // then still reach into the "free" space, so the table must match.
    let on_disk: Vec<(u32, u64, u64)> = {
        let mut v: Vec<_> = table.parts.iter().map(|p| (p.number, p.start, p.size)).collect();
        v.sort();
        v
    };
    if disk::kernel_partitions(disk)? != on_disk {
        bail!("the kernel's view of {}'s partitions differs from its partition table; reboot so it matches, then rerun", dev.display());
    }
    let image_table = gpt::Image::from_table(image_gpt(image, image_size)?)?;
    let image_disk = image_table.disk;
    let plan = gpt::plan(&table, image_table)?;

    std::process::Command::new(LSBLK.path().unwrap()).args(["-o", "NAME,SIZE,MODEL,SERIAL,FSTYPE,LABEL,MOUNTPOINTS"]).arg(dev).status()?;
    println!("\n{target} keeps every partition it has. Windows goes into its free space:\n");
    println!("  {:>12} {:>12} {:>9}", "start", "end", "size");
    let mut rows: Vec<(u64, u64, String)> = table.parts.iter().map(|p| (p.start, p.end(), format!("partition {}, {} (kept)", p.number, p.name))).collect();
    rows.extend(plan.placements().iter().map(|p| (p.start, p.end(), format!("new: {}", p.source.name))));
    rows.sort();
    for (start, end, what) in rows {
        let size = (end - start) * gpt::SECTOR;
        let size = if size >= 1 << 30 { format!("{} GiB", size >> 30) } else { format!("{} MiB", size >> 20) };
        println!("  {start:>12} {end:>12} {size:>9}  {what}");
    }
    println!("\nOnly sectors {}–{} are written, plus the partition table.", plan.region.start, plan.region.end);
    confirm("INSTALL", "write Windows into the free space")?;

    // Data first, partition table last: an interrupted run leaves only
    // garbage in still-free space, not half-written partitions.
    copy_partitions(image, dev, image_size, &plan)?;

    let mut script = String::new();
    for p in plan.placements() {
        let s = &p.source;
        script += &format!("start={}, size={}, type={}, uuid={}, name=\"{}\"", p.start, p.size, s.kind, s.uuid, s.name);
        if let Some(a) = &s.attrs {
            script += &format!(", attrs=\"{a}\"");
        }
        script += "\n";
    }
    // --no-reread skips sfdisk's in-use check, which a disk with Linux
    // mounted fails. The kernel learns of the new partitions below.
    tools::run_with_input(SFDISK.cmd().args(["--append", "--no-reread", "--no-tell-kernel", "--wipe", "never", "--wipe-partitions", "never"]).arg(dev), &script)?;
    let after = gpt::parse(&tools::output_string(SFDISK.cmd().arg("--json").arg(dev))?)?;
    let number = |p: &gpt::Placement| {
        after.parts.iter().find(|a| a.start == p.start && a.size == p.size && a.uuid == p.source.uuid).map(|a| a.number).context("sfdisk didn't add the partitions as asked")
    };
    let (esp, windows) = (number(&plan.esp)?, number(&plan.windows)?);
    // Even with --no-tell-kernel, udev reacting to the write had already
    // added them in the test VM, and partx -a fails on one that exists. So
    // partx adds only what's missing; either way the check after this
    // compares every extent with the plan.
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let known: Vec<u32> = disk::kernel_partitions(disk)?.iter().map(|&(n, _, _)| n).collect();
    for p in plan.placements() {
        let n = number(p)?;
        if !known.contains(&n) {
            tools::run(PARTX.cmd().args(["-a", "--nr", &n.to_string()]).arg(dev))?;
        }
    }
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let mut expected = on_disk;
    for p in plan.placements() {
        expected.push((number(p)?, p.start, p.size));
    }
    expected.sort();
    if disk::kernel_partitions(disk)? != expected {
        bail!("the kernel didn't pick up the new partitions; reboot, then run ntfsresize on Windows' partition and add a boot entry by hand");
    }

    let esp_node = disk.partition(esp);
    retarget_bcd(&esp_node, image_disk, table.id)?;
    grow_ntfs(&disk.partition(windows))?;
    Ok((esp, windows))
}

/// The image's partition table, from its first MiB.
fn image_gpt(image: &Path, image_size: u64) -> Result<gpt::Table> {
    let head = std::env::temp_dir().join(format!("dfnstrate-image-gpt-{}", std::process::id()));
    let result = (|| {
        let mut input = zstd::Decoder::new(File::open(image).with_context(|| image.display().to_string())?)?;
        let mut buf = vec![0u8; 1 << 20];
        input.read_exact(&mut buf)?;
        let f = File::create(&head)?;
        f.write_all_at(&buf, 0)?;
        // Sparse, full size, so the table's last usable LBA makes sense to sfdisk.
        f.set_len(image_size)?;
        // stderr dropped: sfdisk rightly notes the backup GPT isn't there.
        let out = SFDISK.cmd().arg("--json").arg(&head).stderr(std::process::Stdio::null()).output()?;
        if !out.status.success() {
            bail!("sfdisk couldn't read the image's partition table");
        }
        gpt::parse(&String::from_utf8(out.stdout)?)
    })();
    std::fs::remove_file(&head).ok();
    result
}

/// Streams the image, writing the bytes of each of its partitions to where
/// the plan puts it and nothing else.
fn copy_partitions(image: &Path, dev: &Path, image_size: u64, plan: &gpt::Plan) -> Result<()> {
    println!("writing {} GiB", gpt::gib(plan.placements().iter().map(|p| p.source.size).sum()));
    let mut input = zstd::Decoder::new(File::open(image).with_context(|| image.display().to_string())?)?;
    let out = File::options().write(true).open(dev).with_context(|| dev.display().to_string())?;
    let ranges: Vec<(u64, u64, u64)> = plan
        .placements()
        .iter()
        .map(|p| {
            // Only ever inside the free region: the plan's invariant, checked
            // again where it would do damage.
            assert!(plan.region.start <= p.start && p.start + p.source.size <= plan.region.end);
            (p.source.start * gpt::SECTOR, p.source.end() * gpt::SECTOR, p.start * gpt::SECTOR)
        })
        .collect();
    let mut buf = vec![0u8; 16 << 20];
    let mut done: u64 = 0;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        let (lo, hi) = (done, done + n as u64);
        for &(src, src_end, dst) in &ranges {
            let (a, b) = (lo.max(src), hi.min(src_end));
            if a < b {
                out.write_all_at(&buf[(a - lo) as usize..(b - lo) as usize], dst + (a - src))?;
            }
        }
        done = hi;
        if done >> 30 != lo >> 30 {
            print!("\r{} / {} GiB", done >> 30, image_size >> 30);
            std::io::stdout().flush()?;
        }
    }
    println!();
    out.sync_all()?;
    if done != image_size {
        bail!("the image decompressed to {done} bytes, its size file says {image_size}");
    }
    Ok(())
}

/// Points the BCD on the new ESP at this disk (see gpt::retarget_bcd).
fn retarget_bcd(esp: &str, from: gpt::Guid, to: gpt::Guid) -> Result<()> {
    const BCD: &str = "::/EFI/Microsoft/Boot/BCD";
    let tmp = std::env::temp_dir().join(format!("dfnstrate-bcd-{}", std::process::id()));
    // mtools refuses a partition whose size isn't a whole number of its
    // idea of tracks otherwise.
    let mtools = |args: &[&std::ffi::OsStr]| tools::run(MCOPY.cmd().env("MTOOLS_SKIP_CHECK", "1").arg("-i").arg(esp).args(args));
    let result = (|| {
        mtools(&["-n".as_ref(), BCD.as_ref(), tmp.as_os_str()])?;
        let mut hive = std::fs::read(&tmp)?;
        let n = gpt::retarget_bcd(&mut hive, from, to)?;
        std::fs::write(&tmp, &hive)?;
        mtools(&["-o".as_ref(), tmp.as_os_str(), BCD.as_ref()])?;
        File::open(esp)?.sync_all()?;
        println!("pointed {n} BCD device entries at this disk");
        Ok(())
    })();
    std::fs::remove_file(&tmp).ok();
    result
}

/// Puts the boot manager this machine's Secure Boot db trusts on the ESP,
/// as Setup would have (see firmware::WindowsBootManager). `build image`
/// ran Setup under OVMF's keys, which include Windows UEFI CA 2023, so the
/// image's ESP has the 2023-signed one: on firmware without that CA, Secure
/// Boot refuses it and Windows doesn't start. The frozen 2011-signed one is
/// in the install's own C:\Windows\Boot\EFI, where Setup takes it from.
fn fit_boot_manager(esp: &str, windows: &str) -> Result<()> {
    let db = match std::fs::read(Path::new("/sys/firmware/efi/efivars").join(format!("db-{}", firmware::DB_GUID))) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("reading Secure Boot's db"),
    };
    if firmware::WindowsBootManager::for_db(db.as_deref()) == firmware::WindowsBootManager::Ca2023 {
        println!("the firmware trusts Windows UEFI CA 2023: keeping the image's boot manager");
        return Ok(());
    }
    let tmp = std::env::temp_dir().join(format!("dfnstrate-bootmgfw-{}", std::process::id()));
    let result = (|| {
        // -f: ntfsresize leaves the volume marked for a check.
        let bootmgr = tools::output(NTFSCAT.cmd().arg("-f").arg(windows).arg("/Windows/Boot/EFI/bootmgfw.efi"))?;
        if !firmware::contains(&bootmgr, b"Microsoft Windows Production PCA 2011") {
            bail!("C:\\Windows\\Boot\\EFI\\bootmgfw.efi in the image isn't signed by the Windows Production PCA 2011");
        }
        std::fs::write(&tmp, &bootmgr)?;
        for dest in ["::/EFI/Microsoft/Boot/bootmgfw.efi", "::/EFI/BOOT/BOOTX64.EFI"] {
            tools::run(MCOPY.cmd().env("MTOOLS_SKIP_CHECK", "1").arg("-i").arg(esp).arg("-o").arg(&tmp).arg(dest))?;
        }
        File::open(esp)?.sync_all()?;
        println!(
            "the firmware doesn't trust Windows UEFI CA 2023: put Windows' 2011-signed boot manager on the ESP (Windows moves to the 2023 one by itself once the firmware trusts it)"
        );
        Ok(())
    })();
    std::fs::remove_file(&tmp).ok();
    result
}

fn grow_ntfs(windows: &str) -> Result<()> {
    // Leaves the volume marked for a check, so Windows runs chkdsk once on
    // first boot. That's ntfsresize being careful, not a problem.
    tools::run_with_input(NTFSRESIZE.cmd().args(["--force", "--no-progress-bar"]).arg(windows), "y\n")
}

/// Adds a firmware entry for the ESP, partition `esp` of the disk.
fn add_boot_entry(disk: &Disk, esp: u32) -> Result<()> {
    // Each flash gives the ESP a new partition UUID, which leaves the
    // previous flash's entry pointing at nothing. Drop every Windows Boot
    // Manager entry whose ESP isn't on any disk here; ones for a Windows that
    // still exists, on this disk or another, stay.
    tools::run(UDEVADM.cmd().arg("settle"))?;
    let entries = tools::output_string(EFIBOOTMGR.cmd().arg("-v"))?;
    for e in firmware::windows_entries(&entries) {
        if !e.live() {
            tools::run(EFIBOOTMGR.cmd().arg("--bootnum").arg(&e.num).arg("--delete-bootnum"))?;
            println!("removed stale firmware entry Boot{} (its ESP {} is gone)", e.num, e.esp);
        }
    }

    // The image has no firmware entry of its own. --create-only leaves
    // BootOrder alone, so the existing OS still boots by default.
    let node = disk.partition(esp);
    let partuuid = tools::output_string(LSBLK.cmd().args(["-no", "PARTUUID"]).arg(&node))?.trim().to_owned();
    if partuuid.is_empty() {
        bail!("{node} has no partition UUID, so there's nothing to point a boot entry at");
    }
    let entries = tools::output_string(EFIBOOTMGR.cmd().arg("-v"))?;
    if entries.to_lowercase().contains(&partuuid.to_lowercase()) {
        println!("firmware already has a boot entry for {node}");
    } else {
        tools::run(EFIBOOTMGR.cmd().args(["--create-only", "--disk"]).arg(&disk.dev).args([
            "--part",
            &esp.to_string(),
            "--label",
            "Windows Boot Manager",
            "--loader",
            "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
        ]))?;
        println!("added a \"Windows Boot Manager\" firmware entry for {node}");
    }
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
