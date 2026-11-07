# defenestrate

A Windows distro, NixOS style: a flake that takes Microsoft's stock Windows 11
ISO and builds an install ISO with explorer.exe thrown out of the window and
the rest of Microsoft's opinions configured away.

Everything is declared in `configs/` as typed Nix options: edition, locale,
user, disk layout, replacement shell, and registry values per hive.

![First boot: no explorer, our bar, Win+Enter opened a terminal](docs/first-boot.png)

## Status

- [x] Stock ISO → single-edition install ISO (plain ISO9660, split WIM, so it fits a FAT32 stick), BIOS + UEFI boot
- [x] Registry config as typed Nix (`sz`, `expandSz`, `multiSz`, `dword`, `qword`, `binary`, delete), validated against the image's own hives at build time
- [x] Generated autounattend.xml: local account, no OOBE, no Microsoft account, optional disk wipe + autologon
- [x] QEMU test VM with Secure Boot and TPM 2.0
- [x] Verified install end to end in the VM: unattended setup, specialize registry import, OOBE, first-logon shell switch, our shell running with no explorer.exe
  - Fully hands-off offline (`restrict=on`): a headless run went from the ISO to our shell in ~47 minutes with no input. Online, OOBE's zero-day-patch check fails in the VM and waits for a "Skip" click. On real hardware: install offline, or hit "Try again".
- [x] Rust shell v0, cross-compiled in the flake (`nix build .#shell`): top bar with a clock and reserved work area, Win+Enter opens a terminal (via a low-level keyboard hook; `RegisterHotKey` can't get Win+Enter even without explorer)
- [x] libcosmic builds for Windows from Linux (`nix build .#cosmic-bar-prototype`); not run on Windows yet
- [ ] Shell: system tray, window list, launcher, notifications
- [ ] Provisioned AppX removal (Outlook, Teams, Copilot, …)
- [ ] WSL2 with a nix-built kernel and NixOS-WSL, mounting the Linux drives
- [ ] Install straight onto a real disk from Linux, no boot media (`build.install`, replaces nixconf's `saturn-windows-image.sh`). Written and guardrails tested; not yet run on hardware

## Usage

Download the Windows 11 ISO named in `nix/source.nix` from
<https://www.microsoft.com/software-download/windows11> and add it to the store
once:

```sh
nix-store --add-root ./windows-iso.gcroot -r \
  "$(nix-store --add-fixed sha256 Windows11_Client_x64_en-us_26300_9457.iso)"
```

The GC root matters: nothing else references the ISO, so the next
`nix-collect-garbage` would delete it.

Then:

```sh
nix build .#vm-iso   # ISO for the test VM: wipes disk 0, logs straight in
nix run .#vm         # boot it in QEMU; state lives in ./.vm
```

A full install in the VM takes about an hour on a loaded 8-core desktop
with the VM disk on an HDD. Shell logs go to `%LOCALAPPDATA%\defenestrate\shell.log`.

The build needs ~25 GB of scratch space and takes a few minutes, mostly
spent exporting the edition out of the 8 GB `install.wim`.

For real hardware, build a config with `lib.x86_64-linux.mkDistro [ ./configs/default.nix ./my-machine.nix ]`.
It must set `user.name` and should leave `disk` at the default `interactive`.

### Installing onto a disk without a USB stick

`build.install` runs setup in the VM above against a real disk and nothing
else, then stops at setup's first reboot. Specialize and OOBE run when the
machine itself boots the disk, so Windows meets its real hardware the normal
way. The VM presents the disk as NVMe, so this is for NVMe targets: Windows
boots on the controller it was installed through.

```sh
nix run .#…build.install                         # erases installTarget
nix run .#…build.install -- /dev/disk/by-id/nvme-…   # or any whole disk, by-id
```

It checks the disk as you and re-runs itself under sudo only to write it.

Windows takes the whole disk, its own ESP included, because the VM can't see
the machine's other disks. That's also why it can wipe disk 0 without asking,
whatever `disk` is set to. The runner refuses a disk that isn't addressed
by-id, a partition, and anything with a mounted filesystem, a member of a
mounted multi-device filesystem, or an md/LVM/dm holder. It asks you to type
`ERASE`, then adds a "Windows Boot Manager" firmware entry without touching
the boot order. Leave the machine offline for the first boot, for the same
OOBE reason as the VM.


## How it works

1. 7-Zip unpacks the ISO. Microsoft's ISO is UDF-only, so xorriso can't read it.
2. wimlib exports the one edition we want from `install.wim`.
3. Each hive's `.reg` file is merged into a scratch copy of that hive with
   hivex. This is only a check: a malformed `.reg` fails the build instead
   of a VM install.
4. The `.reg` files and `specialize.cmd` are added to the image under
   `C:\Windows\Setup\defenestrate`, and the shell under
   `C:\Program Files\defenestrate`. During the specialize pass, Windows runs
   `reg import` on them itself. The shell switch is the exception: it runs on
   the first real logon and then reboots, because OOBE's own temporary
   session needs explorer and falls over without it.
5. `boot.wim` gets the answer file and a `winpeshl.ini` that forces legacy
   setup, because the 24H2+ setup drops the specialize/oobeSystem passes.
6. `install.wim` is split into `install.swm` parts under 4 GiB and xorriso
   writes a plain ISO9660 image. The single-edition image is 7.5 GB, and the
   first attempt (one big file in a UDF image from cdrtools' mkisofs) left
   Windows setup stuck at 0% without ever reading past the file's start.

Why the registry isn't edited offline: when wimlib replaces an existing file
from Linux, the file loses its NTFS security descriptor and gets its parent's
inherited ACL. For registry hives that's a HiveNightmare-style bug, so Windows
applies the edits instead.
