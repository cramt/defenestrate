//! `dfnstrate doctor`: whether this machine is ready for defenestrate, and
//! what to fix, before an hour goes into a build. Runs as the user, changes
//! nothing, and reads only what any distro has: sysfs, efivarfs, PATH.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::cache::Cache;
use crate::config::IsoPin;
use crate::firmware::{self, WindowsEntry};
use crate::tools::{CHROMIUM, EFIBOOTMGR, FINDMNT, LSBLK, NTFS3G, NTFS3G_PROBE, SUDO, SYSTEMCTL, Tool};
use crate::{flash, image, iso, vm};

pub enum Outcome {
    Ok(String),
    Warn { found: String, fix: String },
    Fail { found: String, fix: String },
}

use Outcome::{Fail, Ok as Pass, Warn};

fn warn(found: impl Into<String>, fix: impl Into<String>) -> Outcome {
    Warn { found: found.into(), fix: fix.into() }
}

fn fail(found: impl Into<String>, fix: impl Into<String>) -> Outcome {
    Fail { found: found.into(), fix: fix.into() }
}

fn pass(found: impl Into<String>) -> Outcome {
    Pass(found.into())
}

pub struct Check {
    name: &'static str,
    outcome: Outcome,
}

pub fn run(iso: &IsoPin, cache: &Cache, work_dir: &Path) -> Result<()> {
    let checks = checks(iso, cache, work_dir);
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut failed = 0;
    for Check { name, outcome } in &checks {
        match outcome {
            Pass(found) => println!("ok    {name:width$}  {found}"),
            Warn { found, fix } => println!("warn  {name:width$}  {found}\n      {:width$}  fix: {fix}", ""),
            Fail { found, fix } => {
                failed += 1;
                println!("FAIL  {name:width$}  {found}\n      {:width$}  fix: {fix}", "");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} check(s) failed");
    }
    Ok(())
}

fn checks(iso_pin: &IsoPin, cache: &Cache, work_dir: &Path) -> Vec<Check> {
    let cpu = CpuVendor::detect();
    let iso_cached = cache.lookup(&iso_pin.hash).is_some();
    let mut out = Vec::new();
    let mut add = |name, outcome| out.push(Check { name, outcome });

    let uefi = Path::new(EFIVARS).is_dir();
    if uefi {
        add("uefi", pass("booted in UEFI mode"));
        add("secure boot", secure_boot(&BootChain::detect()));
    } else {
        // Secure Boot and boot entries only exist under UEFI, so they'd
        // just repeat this.
        add(
            "uefi",
            fail(
                "booted in legacy BIOS mode",
                "Windows 11, Secure Boot and `flash`'s firmware entry all need UEFI: switch the firmware to UEFI (CSM off) and reinstall Linux's bootloader for it",
            ),
        );
    }
    add("tpm", tpm(read_tpm(), cpu));
    add("kvm", kvm(File::options().read(true).write(true).open("/dev/kvm").map(|_| ()).map_err(|e| e.kind()), cpu));

    add("tools: fetch", fetch_tools(iso_cached));
    add("tools: build iso", tool_outcome(&[iso::TOOLS], true, None));
    add("tools: build image", tool_outcome(&[iso::TOOLS, image::TOOLS], true, ovmf()));
    add("tools: flash", tool_outcome(&[flash::TOOLS, &[SUDO, SYSTEMCTL]], true, None));
    add("tools: update", tool_outcome(&[&[NTFS3G, NTFS3G_PROBE, LSBLK, FINDMNT, SUDO]], false, None));

    let work = existing_ancestor(work_dir);
    let cache_fs = existing_ancestor(&cache.dir);
    let shared = same_fs(&work, &cache_fs);
    let iso_need = if iso_cached { 0 } else { ISO_BYTES };
    add("work dir space", work_space(&work, free_bytes(&work), if shared { iso_need } else { 0 }));
    if !shared {
        add("cache space", cache_space(&cache.dir, free_bytes(&cache_fs), iso_cached));
    }
    add("work dir disk", work_disk(&work, rotational(&work)));

    if uefi {
        add("windows boot entries", boot_entries());
        add("windows boot manager", boot_manager(match efivar("db", firmware::DB_GUID) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }));
    }
    add("hardware clock", rtc(std::fs::read_to_string("/etc/adjtime").ok().as_deref()));
    add("hibernation", hibernation(std::fs::read_to_string("/proc/swaps").ok().as_deref()));
    out
}

const EFIVARS: &str = "/sys/firmware/efi/efivars";
const EFI_GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
/// systemd's Boot Loader Interface, which systemd-boot, systemd-stub,
/// lanzaboote's stub and GRUB's bli module fill in.
const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const SHIM_GUID: &str = "605dab50-e046-4300-abb6-3dd810dd8b23";

fn efivar(name: &str, guid: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(Path::new(EFIVARS).join(format!("{name}-{guid}")))
}

/// efivarfs files start with the variable's 4-byte attributes.
fn efivar_bool(bytes: &[u8]) -> Option<bool> {
    match bytes {
        [_, _, _, _, 0] => Some(false),
        [_, _, _, _, 1] => Some(true),
        _ => None,
    }
}

/// A NUL-terminated UTF-16LE string, as the Boot Loader Interface writes.
fn efivar_string(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes.get(4..)?.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).take_while(|&u| u != 0).collect();
    String::from_utf16(&units).ok()
}

#[derive(Debug, PartialEq, Eq)]
enum SecureBoot {
    On,
    /// Setup mode: no Platform Key enrolled, so sbctl can enroll keys from
    /// Linux without a trip into the firmware's key menus.
    Off { setup_mode: bool },
    /// No SecureBoot variable: the firmware can't do it at all.
    Unsupported,
}

fn secure_boot_state(secure_boot: Option<&[u8]>, setup_mode: Option<&[u8]>) -> Option<SecureBoot> {
    let Some(sb) = secure_boot else { return Some(SecureBoot::Unsupported) };
    Some(if efivar_bool(sb)? { SecureBoot::On } else { SecureBoot::Off { setup_mode: setup_mode.and_then(efivar_bool).unwrap_or(false) } })
}

#[derive(Debug, PartialEq, Eq)]
enum Loader {
    Lanzaboote,
    SystemdBoot,
    Grub,
    Unknown,
}

/// What boots Linux here, which decides how it survives Secure Boot.
#[derive(Debug, PartialEq, Eq)]
struct BootChain {
    loader: Loader,
    /// Booted through shim, which Microsoft's UEFI CA signs, so it already
    /// passes Secure Boot with the factory keys.
    shim: bool,
    nixos: bool,
}

impl BootChain {
    fn detect() -> Self {
        let read = |name| efivar(name, LOADER_GUID).ok().and_then(|b| efivar_string(&b));
        let shim = ["MokListRT", "SbatLevelRT"].iter().any(|n| Path::new(EFIVARS).join(format!("{n}-{SHIM_GUID}")).exists());
        let grub_dir = ["/boot/grub", "/boot/grub2"].iter().any(|d| Path::new(d).is_dir());
        BootChain {
            loader: loader(read("LoaderInfo").as_deref(), read("StubInfo").as_deref(), grub_dir),
            shim,
            nixos: Path::new("/etc/NIXOS").exists(),
        }
    }

    fn describe(&self) -> String {
        let loader = match self.loader {
            Loader::Lanzaboote => "lanzaboote",
            Loader::SystemdBoot => "systemd-boot",
            Loader::Grub => "GRUB",
            Loader::Unknown => "an unknown bootloader",
        };
        if self.shim { format!("{loader} via shim") } else { loader.into() }
    }
}

fn loader(loader_info: Option<&str>, stub_info: Option<&str>, grub_dir: bool) -> Loader {
    let has = |s: Option<&str>, needle: &str| s.is_some_and(|s| s.to_ascii_lowercase().contains(needle));
    if has(stub_info, "lanza") || has(loader_info, "lanza") {
        Loader::Lanzaboote
    } else if has(loader_info, "systemd-boot") {
        Loader::SystemdBoot
    } else if has(loader_info, "grub") || (loader_info.is_none() && grub_dir) {
        Loader::Grub
    } else {
        Loader::Unknown
    }
}

/// Vanguard refuses to start on Windows 11 without Secure Boot (VAN 9003):
/// https://support-leagueoflegends.riotgames.com/hc/en-us/articles/48069703999379-Error-VAN-9003
/// (the same article on every Riot game's support site), and lists UEFI,
/// TPM 2.0 and Secure Boot as its requirements:
/// https://support-valorant.riotgames.com/hc/en-us/articles/48069695920787-Error-VAN-9005
fn secure_boot(chain: &BootChain) -> Outcome {
    let read = |name| match efivar(name, EFI_GLOBAL) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    match (read("SecureBoot"), read("SetupMode")) {
        (Ok(sb), Ok(setup)) => match secure_boot_state(sb.as_deref(), setup.as_deref()) {
            Some(state) => secure_boot_outcome(state, chain),
            None => warn("SecureBoot variable has an unexpected size", "check it with `bootctl status` or `mokutil --sb-state`"),
        },
        (Err(e), _) | (_, Err(e)) => warn(format!("can't read the SecureBoot variable: {e}"), "check it with `bootctl status` or `mokutil --sb-state`"),
    }
}

fn secure_boot_outcome(state: SecureBoot, chain: &BootChain) -> Outcome {
    let (setup_mode, found) = match state {
        SecureBoot::On => return pass(format!("on ({})", chain.describe())),
        SecureBoot::Unsupported => {
            return fail("the firmware has no Secure Boot", "Vanguard won't run without it; look for a firmware update that adds it")
        }
        SecureBoot::Off { setup_mode: true } => (true, format!("off, firmware in setup mode (no keys enrolled); Linux boots with {}", chain.describe())),
        SecureBoot::Off { setup_mode: false } => (false, format!("off; Linux boots with {}", chain.describe())),
    };
    // Without Microsoft's keys next to your own, Windows' boot manager and
    // most GPUs' option ROMs stop loading, hence --microsoft everywhere.
    let enroll = if setup_mode {
        "enroll keys with Microsoft's (`sbctl create-keys && sbctl enroll-keys --microsoft`, setup mode allows it now)"
    } else {
        "put the firmware in setup mode (clear its Secure Boot keys), enroll keys with Microsoft's (`sbctl create-keys && sbctl enroll-keys --microsoft`)"
    };
    let fix = if chain.shim {
        if setup_mode {
            "Vanguard needs it on (VAN 9003). shim is signed by Microsoft: restore the firmware's default Secure Boot keys, then turn Secure Boot on".to_string()
        } else {
            "Vanguard needs it on (VAN 9003). shim is signed by Microsoft, so turning Secure Boot on in the firmware should just work".to_string()
        }
    } else {
        let sign = match (&chain.loader, chain.nixos) {
            (Loader::Lanzaboote, _) => "lanzaboote already signs the boot files".to_string(),
            (_, true) => "switch to lanzaboote (https://github.com/nix-community/lanzaboote)".to_string(),
            (Loader::SystemdBoot, false) => "sign systemd-boot and the kernels/UKIs with sbctl, or boot through your distro's shim".to_string(),
            (Loader::Grub | Loader::Unknown, false) => "boot through your distro's signed shim (package shim / shim-signed), or sign the loader with sbctl".to_string(),
        };
        format!("Vanguard needs it on (VAN 9003), but turned on as is it stops an unsigned Linux bootloader from booting: {sign}, {enroll}, then turn Secure Boot on")
    };
    fail(found, fix)
}

#[derive(Clone, Copy)]
enum CpuVendor {
    Intel,
    Amd,
    Other,
}

impl CpuVendor {
    fn detect() -> Self {
        let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        if info.contains("GenuineIntel") {
            CpuVendor::Intel
        } else if info.contains("AuthenticAMD") {
            CpuVendor::Amd
        } else {
            CpuVendor::Other
        }
    }

    fn firmware_tpm(self) -> &'static str {
        match self {
            CpuVendor::Intel => "Intel PTT",
            CpuVendor::Amd => "AMD fTPM",
            CpuVendor::Other => "the firmware TPM (Intel PTT / AMD fTPM)",
        }
    }

    fn virtualization(self) -> &'static str {
        match self {
            CpuVendor::Intel => "VT-x, module kvm_intel",
            CpuVendor::Amd => "AMD-V/SVM, module kvm_amd",
            CpuVendor::Other => "VT-x / AMD-V, module kvm_intel / kvm_amd",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Tpm {
    None,
    V1,
    V2,
}

fn read_tpm() -> Tpm {
    let dir = Path::new("/sys/class/tpm/tpm0");
    tpm_version(dir.exists(), std::fs::read_to_string(dir.join("tpm_version_major")).ok().as_deref(), Path::new("/dev/tpmrm0").exists())
}

/// tpm_version_major is from Linux 5.x; before it, only TPM 2.0 gets a
/// kernel resource manager device (tpmrm0).
fn tpm_version(present: bool, version_major: Option<&str>, tpmrm: bool) -> Tpm {
    match (present, version_major.map(str::trim)) {
        (false, _) => Tpm::None,
        (true, Some("2")) => Tpm::V2,
        (true, Some(_)) => Tpm::V1,
        (true, None) if tpmrm => Tpm::V2,
        (true, None) => Tpm::V1,
    }
}

/// VAN 9001: "This build of Vanguard requires TPM 2.0 to be enabled":
/// https://support-2xko.riotgames.com/hc/en-us/articles/31897339172627-2XKO-How-to-Fix-Error-VAN9001-by-Enabling-TPM-2-0-Win-11-Only
fn tpm(tpm: Tpm, cpu: CpuVendor) -> Outcome {
    match tpm {
        Tpm::V2 => pass("TPM 2.0"),
        // A firmware TPM that's switched off isn't in ACPI, so Linux sees
        // nothing at all.
        Tpm::None => fail("no TPM visible", format!("Vanguard needs TPM 2.0 (VAN 9001): turn on {} in the firmware", cpu.firmware_tpm())),
        Tpm::V1 => fail("TPM 1.2", format!("Vanguard needs TPM 2.0 (VAN 9001): switch the firmware to {}", cpu.firmware_tpm())),
    }
}

fn kvm(open: Result<(), ErrorKind>, cpu: CpuVendor) -> Outcome {
    match open {
        Ok(()) => pass("/dev/kvm usable"),
        Err(ErrorKind::NotFound) => fail(
            "no /dev/kvm",
            format!("`build image` installs Windows in a KVM VM: turn on virtualization in the firmware ({})", cpu.virtualization()),
        ),
        Err(ErrorKind::PermissionDenied) => fail("/dev/kvm isn't writable by you", "`sudo usermod -aG kvm $USER`, then log in again"),
        Err(e) => fail(format!("/dev/kvm: {e}"), "`build image` needs read/write access to /dev/kvm"),
    }
}

fn missing(groups: &[&[Tool]]) -> Vec<Tool> {
    let mut out: Vec<Tool> = Vec::new();
    for t in groups.iter().flat_map(|g| g.iter()) {
        if t.path().is_none() && !out.iter().any(|o| o.names == t.names) {
            out.push(*t);
        }
    }
    out
}

fn list_missing(tools: &[Tool], extra: Option<&str>) -> String {
    tools.iter().map(|t| format!("{} ({})", t.names[0], t.package)).chain(extra.map(String::from)).collect::<Vec<_>>().join(", ")
}

/// `ovmf` is `build image`'s firmware: Err with what's missing.
fn tool_outcome(groups: &[&[Tool]], required: bool, ovmf: Option<Result<(), String>>) -> Outcome {
    let tools = missing(groups);
    let ovmf_missing = match &ovmf {
        Some(Err(e)) => Some(e.as_str()),
        _ => None,
    };
    if tools.is_empty() && ovmf_missing.is_none() {
        return pass(if ovmf.is_some() { "all on PATH, Secure Boot OVMF found" } else { "all on PATH" });
    }
    let found = format!("missing {}", list_missing(&tools, ovmf_missing));
    let fix = "install the packages in parentheses (or run dfnstrate from its Nix package, which brings them)";
    if required { fail(found, fix) } else { warn(found, fix) }
}

fn ovmf() -> Option<Result<(), String>> {
    Some(vm::Firmware::from_env().resolve().map(|_| ()).map_err(|_| "Secure Boot OVMF (edk2-ovmf / ovmf)".into()))
}

/// Only needed when the ISO isn't cached and no --iso is given.
fn fetch_tools(iso_cached: bool) -> Outcome {
    match (CHROMIUM.path(), iso_cached) {
        (Some(p), _) => pass(format!("{}", p.display())),
        (None, true) => pass("no Chromium, but the pinned ISO is already cached"),
        (None, false) => warn(
            "no Chromium or Chrome, so the stock ISO can't be fetched",
            "install chromium, or download the ISO yourself and pass `--iso FILE` to build",
        ),
    }
}

/// From the README: ~25 GB scratch for `build iso`, ~45 GB for `build
/// image`, and the stock ISO itself (7.x GB for 25H2).
const ISO_SCRATCH: u64 = 25 << 30;
const IMAGE_SCRATCH: u64 = 45 << 30;
const ISO_BYTES: u64 = 8 << 30;

fn gib(b: u64) -> String {
    format!("{} GiB", b >> 30)
}

fn existing_ancestor(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    abs.ancestors().find(|a| a.exists()).unwrap_or(Path::new("/")).to_path_buf()
}

fn same_fs(a: &Path, b: &Path) -> bool {
    matches!((a.metadata(), b.metadata()), (Ok(a), Ok(b)) if a.dev() == b.dev())
}

fn free_bytes(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the struct we hand it.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

/// `extra` is the ISO's share when the cache is on the same filesystem.
fn work_space(work: &Path, free: Option<u64>, extra: u64) -> Outcome {
    let Some(free) = free else { return warn(format!("can't tell free space on {}", work.display()), "builds need ~45 GB scratch") };
    let at = format!("{} free on {}", gib(free), work.display());
    let also = if extra > 0 { " plus the ISO in the cache" } else { "" };
    if free >= IMAGE_SCRATCH + extra {
        pass(at)
    } else if free >= ISO_SCRATCH + extra {
        warn(format!("{at}: enough for `build iso`, not `build image`"), format!("`build image` needs ~45 GB{also}: free some up, or pass `--work-dir` elsewhere"))
    } else {
        fail(at, format!("builds need ~25 GB (`build iso`) to ~45 GB (`build image`) scratch{also}: free some up, or pass `--work-dir` elsewhere"))
    }
}

fn cache_space(dir: &Path, free: Option<u64>, iso_cached: bool) -> Outcome {
    if iso_cached {
        return pass(format!("the pinned ISO is already in {}", dir.display()));
    }
    match free {
        Some(f) if f >= ISO_BYTES => pass(format!("{} free for the ISO in {}", gib(f), dir.display())),
        Some(f) => fail(format!("{} free in {}", gib(f), dir.display()), "the stock ISO needs ~8 GB: free some up, or point --cache / DFNSTRATE_CACHE elsewhere"),
        None => warn(format!("can't tell free space for {}", dir.display()), "the stock ISO needs ~8 GB"),
    }
}

/// Spinning disks make `build image` fail: the VM's writes starve its
/// reads from the install CD until Windows drops it (see image::build).
fn work_disk(work: &Path, rotational: Option<bool>) -> Outcome {
    match rotational {
        Some(false) => pass(format!("{} is on an SSD", work.display())),
        Some(true) => warn(
            format!("{} is on a spinning disk", work.display()),
            "`build image` fails there (0x800701B1, or WinPE blue-screens): run it from an SSD or pass `--work-dir` on one",
        ),
        None => pass(format!("can't tell what disk {} is on (fine on an SSD; `build image` fails on spinning disks)", work.display())),
    }
}

/// Some(true) if any disk under the filesystem holding `p` spins.
fn rotational(p: &Path) -> Option<bool> {
    let out = Command::new(FINDMNT.path()?).args(["-Pno", "FSTYPE,UUID,MAJ:MIN", "-T"]).arg(p).stderr(Stdio::null()).output().ok()?;
    let line = String::from_utf8(out.stdout).ok()?;
    let fields = findmnt_pairs(line.lines().next()?);
    let get = |k| fields.iter().find(|(key, _)| *key == k).map(|(_, v)| *v).unwrap_or("");
    // btrfs reports an anonymous device number; its member disks are listed
    // under its UUID instead.
    let devices: Vec<PathBuf> = if get("FSTYPE") == "btrfs" && !get("UUID").is_empty() {
        std::fs::read_dir(Path::new("/sys/fs/btrfs").join(get("UUID")).join("devices")).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect()
    } else {
        vec![Path::new("/sys/dev/block").join(get("MAJ:MIN"))]
    };
    devices.iter().filter_map(|d| block_rotational(d)).reduce(|a, b| a || b)
}

/// Follows partitions to their disk and dm/md devices to what's under them.
fn block_rotational(sys: &Path) -> Option<bool> {
    let sys = std::fs::canonicalize(sys).ok()?;
    let slaves: Vec<PathBuf> = std::fs::read_dir(sys.join("slaves")).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default();
    if !slaves.is_empty() {
        return slaves.iter().filter_map(|s| block_rotational(s)).reduce(|a, b| a || b);
    }
    let disk = if sys.join("partition").exists() { sys.parent()?.to_path_buf() } else { sys };
    match std::fs::read_to_string(disk.join("queue/rotational")).ok()?.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// `findmnt -P`: KEY="value" pairs, separated by spaces.
fn findmnt_pairs(line: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while let Some((key, after)) = rest.split_once("=\"") {
        let Some((value, next)) = after.split_once('"') else { break };
        out.push((key.trim(), value));
        rest = next;
    }
    out
}

fn boot_entries() -> Outcome {
    let Some(path) = EFIBOOTMGR.path() else {
        return warn("efibootmgr isn't installed, so firmware entries can't be listed", "install efibootmgr (`flash` and `reboot` need it too)");
    };
    match Command::new(path).arg("-v").stderr(Stdio::piped()).output() {
        Ok(o) if o.status.success() => entries_outcome(&firmware::windows_entries(&String::from_utf8_lossy(&o.stdout)), WindowsEntry::live),
        Ok(o) => warn(
            format!("efibootmgr failed: {}", String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("no output")),
            "some systems only let root read boot entries: `sudo efibootmgr -v` shows them",
        ),
        Err(e) => warn(format!("couldn't run efibootmgr: {e}"), "`sudo efibootmgr -v` shows them"),
    }
}

/// Which boot manager `flash` will leave on Windows' ESP (see
/// firmware::WindowsBootManager); worth knowing before Secure Boot goes on.
fn boot_manager(db: std::io::Result<Option<Vec<u8>>>) -> Outcome {
    use firmware::WindowsBootManager::{Ca2023, Pca2011};
    match db {
        Err(e) => warn(format!("can't read Secure Boot's db: {e}"), "`flash` checks it again as root; nothing to do here"),
        Ok(db) => match firmware::WindowsBootManager::for_db(db.as_deref()) {
            Ca2023 => pass("Secure Boot's db trusts Windows UEFI CA 2023, so Windows keeps its current boot manager"),
            Pca2011 => warn(
                if db.is_some() { "Secure Boot's db lacks Windows UEFI CA 2023" } else { "no Secure Boot db (setup mode)" },
                "nothing needed to install: `flash` puts Windows' frozen 2011-signed boot manager on its ESP, and Windows Update moves it to the 2023 one once it has added the CA. A firmware update that adds Windows UEFI CA 2023 is better: Microsoft says future Windows releases won't install without it (KB5132953)",
            ),
        },
    }
}

fn entries_outcome(entries: &[WindowsEntry], live: impl Fn(&WindowsEntry) -> bool) -> Outcome {
    let (live, stale): (Vec<&WindowsEntry>, Vec<&WindowsEntry>) = entries.iter().partition(|e| live(e));
    let nums = |es: &[&WindowsEntry]| es.iter().map(|e| format!("Boot{}", e.num)).collect::<Vec<_>>().join(", ");
    if !stale.is_empty() {
        return warn(
            format!("{} point at an ESP that's on no disk here", nums(&stale)),
            "harmless clutter; `dfnstrate flash` prunes them, or `sudo efibootmgr -b XXXX -B` each",
        );
    }
    // As `reboot` counts them: Windows' first boot adds its own entry next
    // to flash's, both on one ESP, and that's still one Windows.
    let mut esps: Vec<&str> = live.iter().map(|e| e.esp.as_str()).collect();
    esps.sort_unstable();
    esps.dedup();
    match esps.len() {
        0 => pass("none yet; `dfnstrate flash` adds one"),
        1 => pass(format!("{}, one Windows on a disk here; `dfnstrate reboot` boots it", nums(&live))),
        _ => warn(format!("{} are all on disks here", nums(&live)), "`dfnstrate reboot` won't guess: pick one with `--entry XXXX`"),
    }
}

/// The default config sets Windows' RealTimeIsUniversal, so both OSes keep
/// the RTC in UTC; Linux on local time would leave one of them hours off.
/// /etc/adjtime's third line says which; no file means UTC.
fn rtc(adjtime: Option<&str>) -> Outcome {
    match adjtime.and_then(|a| a.lines().nth(2)).map(str::trim) {
        Some("LOCAL") => warn(
            "Linux keeps the hardware clock in local time",
            "defenestrate's Windows keeps it in UTC (RealTimeIsUniversal): `timedatectl set-local-rtc 0` (NixOS: `time.hardwareClockInLocalTime = false`)",
        ),
        _ => pass("UTC, as defenestrate's Windows keeps it"),
    }
}

/// zram can't hold a hibernation image; any other swap can.
fn hibernation(swaps: Option<&str>) -> Outcome {
    let disk_swap: Vec<&str> = swaps
        .unwrap_or("")
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .filter(|f| !f.starts_with("/dev/zram"))
        .collect();
    if disk_swap.is_empty() {
        pass("no disk swap, so Linux can't hibernate")
    } else {
        warn(
            format!("Linux could hibernate (swap on {})", disk_swap.join(", ")),
            "shut Linux down, never hibernate it, before booting Windows: with `wsl.linux` Windows mounts the same disks",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(o: &Outcome) -> &'static str {
        match o {
            Pass(_) => "ok",
            Warn { .. } => "warn",
            Fail { .. } => "fail",
        }
    }

    fn fix(o: &Outcome) -> &str {
        match o {
            Pass(_) => "",
            Warn { fix, .. } | Fail { fix, .. } => fix,
        }
    }

    #[test]
    fn secure_boot_variables() {
        let on = [6, 0, 0, 0, 1];
        let off = [6, 0, 0, 0, 0];
        assert_eq!(secure_boot_state(Some(&on), Some(&off)), Some(SecureBoot::On));
        assert_eq!(secure_boot_state(Some(&off), Some(&on)), Some(SecureBoot::Off { setup_mode: true }));
        assert_eq!(secure_boot_state(Some(&off), Some(&off)), Some(SecureBoot::Off { setup_mode: false }));
        assert_eq!(secure_boot_state(Some(&off), None), Some(SecureBoot::Off { setup_mode: false }));
        assert_eq!(secure_boot_state(None, None), Some(SecureBoot::Unsupported));
        assert_eq!(secure_boot_state(Some(&[6, 0, 0, 0]), None), None);
        assert_eq!(secure_boot_state(Some(&[6, 0, 0, 0, 2]), None), None);
    }

    #[test]
    fn loader_info_strings() {
        // This machine's LoaderInfo: attributes, then "systemd-boot 262" in
        // UTF-16LE with its NUL.
        let mut bytes = vec![6, 0, 0, 0];
        bytes.extend("systemd-boot 262\0".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(efivar_string(&bytes).as_deref(), Some("systemd-boot 262"));
        assert_eq!(efivar_string(&[6, 0]), None);
    }

    #[test]
    fn detects_loaders() {
        assert_eq!(loader(Some("systemd-boot 262"), None, false), Loader::SystemdBoot);
        assert_eq!(loader(Some("systemd-boot 256"), Some("lanzastub 0.4.2"), false), Loader::Lanzaboote);
        assert_eq!(loader(Some("GRUB 2.12"), None, false), Loader::Grub);
        assert_eq!(loader(None, None, true), Loader::Grub);
        assert_eq!(loader(None, None, false), Loader::Unknown);
    }

    #[test]
    fn secure_boot_fixes_follow_the_loader() {
        let chain = |loader, shim, nixos| BootChain { loader, shim, nixos };
        let off = || SecureBoot::Off { setup_mode: false };
        let nixos = secure_boot_outcome(SecureBoot::Off { setup_mode: true }, &chain(Loader::SystemdBoot, false, true));
        assert_eq!(label(&nixos), "fail");
        assert!(fix(&nixos).contains("lanzaboote") && fix(&nixos).contains("--microsoft") && fix(&nixos).contains("allows it now"), "{}", fix(&nixos));
        let arch = secure_boot_outcome(off(), &chain(Loader::SystemdBoot, false, false));
        assert!(fix(&arch).contains("sbctl") && fix(&arch).contains("setup mode (clear"), "{}", fix(&arch));
        let ubuntu = secure_boot_outcome(off(), &chain(Loader::Grub, true, false));
        assert!(fix(&ubuntu).contains("should just work"), "{}", fix(&ubuntu));
        let fedora = secure_boot_outcome(off(), &chain(Loader::Grub, false, false));
        assert!(fix(&fedora).contains("shim-signed"), "{}", fix(&fedora));
        assert_eq!(label(&secure_boot_outcome(SecureBoot::On, &chain(Loader::Lanzaboote, false, true))), "ok");
    }

    #[test]
    fn tpm_versions() {
        assert_eq!(tpm_version(false, None, false), Tpm::None);
        assert_eq!(tpm_version(true, Some("2\n"), true), Tpm::V2);
        assert_eq!(tpm_version(true, Some("1\n"), false), Tpm::V1);
        assert_eq!(tpm_version(true, None, true), Tpm::V2);
        assert_eq!(tpm_version(true, None, false), Tpm::V1);
        assert!(fix(&tpm(Tpm::None, CpuVendor::Intel)).contains("Intel PTT"));
    }

    #[test]
    fn kvm_errors() {
        assert_eq!(label(&kvm(Ok(()), CpuVendor::Amd)), "ok");
        assert!(fix(&kvm(Err(ErrorKind::PermissionDenied), CpuVendor::Amd)).contains("kvm"));
        assert!(fix(&kvm(Err(ErrorKind::NotFound), CpuVendor::Amd)).contains("kvm_amd"));
    }

    #[test]
    fn findmnt_output() {
        let pairs = findmnt_pairs("FSTYPE=\"btrfs\" UUID=\"b6cf3d6a-b7b8-4135-b3a3-17794ec86601\" MAJ:MIN=\"0:46\"\n");
        assert_eq!(pairs, vec![("FSTYPE", "btrfs"), ("UUID", "b6cf3d6a-b7b8-4135-b3a3-17794ec86601"), ("MAJ:MIN", "0:46")]);
        assert_eq!(findmnt_pairs("FSTYPE=\"tmpfs\" UUID=\"\" MAJ:MIN=\"0:30\""), vec![("FSTYPE", "tmpfs"), ("UUID", ""), ("MAJ:MIN", "0:30")]);
    }

    #[test]
    fn space_thresholds() {
        let w = Path::new("/w");
        assert_eq!(label(&work_space(w, Some(50 << 30), 0)), "ok");
        assert_eq!(label(&work_space(w, Some(50 << 30), ISO_BYTES)), "warn");
        assert_eq!(label(&work_space(w, Some(30 << 30), 0)), "warn");
        assert_eq!(label(&work_space(w, Some(10 << 30), 0)), "fail");
        assert_eq!(label(&cache_space(w, Some(1 << 30), true)), "ok");
        assert_eq!(label(&cache_space(w, Some(1 << 30), false)), "fail");
    }

    #[test]
    fn windows_entries() {
        let e = |num: &str, esp: &str| WindowsEntry { num: num.parse().unwrap(), esp: esp.into() };
        assert_eq!(label(&entries_outcome(&[], |_| true)), "ok");
        assert_eq!(label(&entries_outcome(&[e("0003", "here")], |x| x.esp == "here")), "ok");
        let stale = entries_outcome(&[e("0003", "here"), e("0001", "gone")], |x| x.esp == "here");
        assert_eq!(label(&stale), "warn");
        assert!(matches!(&stale, Warn { found, .. } if found.contains("Boot0001") && !found.contains("Boot0003")));
        assert!(fix(&entries_outcome(&[e("0003", "a"), e("0004", "b")], |_| true)).contains("--entry"));
        assert_eq!(label(&entries_outcome(&[e("0003", "esp"), e("0006", "esp")], |_| true)), "ok");
    }

    #[test]
    fn hardware_clock() {
        assert_eq!(label(&rtc(None)), "ok");
        assert_eq!(label(&rtc(Some("0.0 0 0.0\n0\nUTC\n"))), "ok");
        assert_eq!(label(&rtc(Some("0.0 0 0.0\n0\nLOCAL\n"))), "warn");
    }

    #[test]
    fn swap_and_hibernation() {
        let header = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n";
        assert_eq!(label(&hibernation(Some(header))), "ok");
        assert_eq!(label(&hibernation(Some(&format!("{header}/dev/zram0  partition 8388604 0 100\n")))), "ok");
        assert_eq!(label(&hibernation(Some(&format!("{header}/dev/nvme0n1p3  partition 33554428 0 -2\n")))), "warn");
    }
}
