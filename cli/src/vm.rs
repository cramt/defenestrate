//! QEMU, for trying an ISO (`dfnstrate vm`) and for `build image`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::tools::{self, QEMU, QEMU_IMG, SWTPM};

#[derive(clap::Args)]
pub struct Firmware {
    /// OVMF code image with Secure Boot [default: the distro's, found under /usr/share]
    #[arg(long, env = "DFNSTRATE_OVMF_CODE", value_name = "FILE")]
    ovmf_code: Option<PathBuf>,
    /// OVMF variable store template to copy, ideally with Microsoft's keys
    /// enrolled [default: the distro's, next to the code image]
    #[arg(long, env = "DFNSTRATE_OVMF_VARS", value_name = "FILE")]
    ovmf_vars: Option<PathBuf>,
}

/// Where distros put a Secure Boot OVMF and its variable store. Windows 11
/// setup insists on Secure Boot support next to the TPM; the .ms variants
/// also ship Microsoft's keys enrolled.
const OVMF: &[(&str, &str)] = &[
    // Debian, Ubuntu
    ("/usr/share/OVMF/OVMF_CODE_4M.ms.fd", "/usr/share/OVMF/OVMF_VARS_4M.ms.fd"),
    // Fedora
    ("/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd", "/usr/share/edk2/ovmf/OVMF_VARS.secboot.fd"),
    // Arch, CachyOS (edk2-ovmf): no Microsoft keys enrolled, Secure Boot in setup mode
    ("/usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd", "/usr/share/edk2/x64/OVMF_VARS.4m.fd"),
    // openSUSE
    ("/usr/share/qemu/ovmf-x86_64-ms-code.bin", "/usr/share/qemu/ovmf-x86_64-ms-vars.bin"),
];

impl Firmware {
    pub fn resolve(&self) -> Result<(PathBuf, PathBuf)> {
        match (&self.ovmf_code, &self.ovmf_vars) {
            (Some(c), Some(v)) => {
                for f in [c, v] {
                    if !f.is_file() {
                        bail!("firmware {} doesn't exist", f.display());
                    }
                }
                Ok((c.clone(), v.clone()))
            }
            (None, None) => OVMF
                .iter()
                .map(|(c, v)| (PathBuf::from(c), PathBuf::from(v)))
                .find(|(c, v)| c.exists() && v.exists())
                .context("no Secure Boot OVMF found; install edk2-ovmf / ovmf, or pass --ovmf-code and --ovmf-vars"),
            _ => bail!("pass both --ovmf-code and --ovmf-vars, or neither"),
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// The ISO to boot (e.g. out/defenestrate.iso from `build iso`).
    #[arg(long)]
    iso: PathBuf,
    /// Disk, TPM and firmware variables live here.
    #[arg(long, env = "DEFENESTRATE_VM_DIR", default_value = ".vm")]
    state: PathBuf,
    #[command(flatten)]
    firmware: Firmware,
    /// Extra QEMU arguments, after --.
    #[arg(last = true)]
    qemu_args: Vec<String>,
}

pub fn run(args: &Args) -> Result<()> {
    tools::require(&[QEMU, QEMU_IMG, SWTPM])?;
    let (code, vars_template) = args.firmware.resolve()?;
    let state = &args.state;
    std::fs::create_dir_all(state.join("tpm"))?;
    // qcow2 on a copy-on-write filesystem (btrfs) fragments into oblivion.
    // Only affects files created afterwards.
    tools::no_cow(state);

    let disk = state.join("disk.qcow2");
    if !disk.exists() {
        tools::run(QEMU_IMG.cmd().args(["create", "-q", "-f", "qcow2"]).arg(&disk).arg("64G"))?;
    }
    let vars = state.join("OVMF_VARS.fd");
    if !vars.exists() {
        copy_writable(&vars_template, &vars)?;
    }
    let tpm = Tpm::start(&state.join("tpm"), &state.join("swtpm.sock"))?;

    // Disk before CD: a blank disk falls through to the installer, an
    // installed one boots Windows instead of starting setup over.
    // SATA and e1000e because stock Windows has no virtio drivers.
    // restrict=on: no internet. Online, OOBE's zero-day-patch update check
    // fails in this VM ("Something went wrong, OOBEZDP") and waits for a
    // click; there's no setting to turn it off, offline it's skipped.
    // BypassNRO lets OOBE carry on without a network.
    let mut qemu = qemu_base("defenestrate", &code, &vars, &tpm.socket);
    qemu.args(["-device", "ahci,id=ahci"])
        .arg("-drive")
        .arg(format!("id=disk,file={},if=none,format=qcow2,cache=unsafe", drive_path(&disk)?))
        .args(["-device", "ide-hd,drive=disk,bus=ahci.0,bootindex=0"])
        .arg("-drive")
        .arg(format!("id=cd,file={},if=none,media=cdrom,readonly=on", drive_path(&args.iso)?))
        .args(["-device", "ide-cd,drive=cd,bus=ahci.1,bootindex=1"])
        .args(["-nic", "user,model=e1000e,restrict=on"])
        .args(["-device", "qemu-xhci", "-device", "usb-tablet"])
        .arg("-qmp")
        .arg(format!("unix:{},server=on,wait=off", state.join("qmp.sock").display()))
        .args(&args.qemu_args);
    let status = qemu.status().context("starting QEMU")?;
    drop(tpm);
    if !status.success() {
        bail!("QEMU exited with {status}");
    }
    Ok(())
}

/// The machine both the VM and `build image` use.
/// hv-passthrough: without the Hyper-V enlightenments Windows burns its
/// vCPUs on timer and spinlock emulation. On a loaded host (load ~35 on 8
/// cores) setup's image apply sat at 0% for 30+ minutes without them and
/// finished in ~21 with them; boot-to-apply went from 44 to 15 minutes.
/// It includes hv-crash, so a blue screen makes QEMU stop (a GUEST_PANICKED
/// event); `build image` reports that rather than mistaking it for the
/// guest's own power-off.
/// cache=unsafe on the disks: they're disposable or a build's scratch, so
/// NTFS flushes needn't fsync.
pub fn qemu_base(name: &str, code: &Path, vars: &Path, tpm_socket: &Path) -> Command {
    let mut c = QEMU.cmd();
    c.args(["-name", name])
        .args(["-machine", "q35,smm=on,accel=kvm"])
        .args(["-global", "driver=cfi.pflash01,property=secure,value=on"])
        .args(["-global", "ICH9-LPC.disable_s3=1"])
        .args(["-cpu", "host,hv-passthrough", "-smp", "4", "-m", "8G"])
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,unit=0,readonly=on,file={}", code.display()))
        .arg("-drive")
        .arg(format!("if=pflash,format=raw,unit=1,file={}", vars.display()))
        .arg("-chardev")
        .arg(format!("socket,id=chrtpm,path={}", tpm_socket.display()))
        .args(["-tpmdev", "emulator,id=tpm0,chardev=chrtpm"])
        .args(["-device", "tpm-crb,tpmdev=tpm0"]);
    c
}

/// QEMU's -drive splits on commas.
pub fn drive_path(p: &Path) -> Result<String> {
    let s = std::path::absolute(p)?.to_str().context("non-UTF-8 path")?.to_owned();
    Ok(s.replace(',', ",,"))
}

pub fn copy_writable(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// swtpm for one VM run; stopped on drop.
pub struct Tpm {
    child: Child,
    pub socket: PathBuf,
}

impl Tpm {
    pub fn start(state: &Path, socket: &Path) -> Result<Self> {
        use std::os::unix::fs::FileTypeExt;
        std::fs::create_dir_all(state)?;
        let _ = std::fs::remove_file(socket);
        // --terminate: swtpm exits once QEMU disconnects.
        let child = SWTPM
            .cmd()
            .args(["socket", "--tpm2"])
            .arg("--tpmstate")
            .arg(format!("dir={}", state.display()))
            .arg("--ctrl")
            .arg(format!("type=unixio,path={}", socket.display()))
            .arg("--terminate")
            .spawn()
            .context("starting swtpm")?;
        let tpm = Self { child, socket: socket.to_path_buf() };
        let start = Instant::now();
        while !std::fs::symlink_metadata(socket).is_ok_and(|m| m.file_type().is_socket()) {
            if start.elapsed() > Duration::from_secs(10) {
                bail!("swtpm never came up");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(tpm)
    }
}

impl Drop for Tpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
