# defenestrate

A Windows distro, NixOS style: takes Microsoft's stock Windows 11 ISO and
builds an install with explorer.exe thrown out of the window and the rest of
Microsoft's opinions configured away.

Two halves:

- **`dfnstrate`** (`cli/`), the Linux side: fetches the stock ISO, builds an
  install ISO or a finished whole-disk image from a config, flashes it, and
  sets up WSL to run the machine's Linux install. A plain Rust binary; it works on any distro. The Nix flake is a
  thin layer that runs the same commands, so both paths hit the same code.
- **The shell** (`shell/`), the Windows side: a tiling window manager that
  replaces explorer.

A config is TOML (or a Nix module setting the same keys): edition, locale,
user, disk layout, shell, fonts, the Linux install to reach through WSL, AppX packages to drop, and registry
values per hive. Every config is layered over the distro's own defaults,
[`configs/default.toml`](configs/default.toml).

![Tiled terminals and Firefox, the Super-tap launcher open, all in Iosevka](docs/first-boot.png)

## Status

- [x] Stock ISO → single-edition install ISO (plain ISO9660, split WIM, so it fits a FAT32 stick), BIOS + UEFI boot
- [x] `dfnstrate`: one Rust CLI for fetch/build/flash, layered TOML configs, a hash-addressed download cache. The flake's ISO, image, VM and install outputs run it, with the cache filled by fixed-output derivations, so a non-Nix build runs the same code as the Nix one
  - Seen: the Nix and non-Nix ISOs for the VM config have identical contents (every file and all 187k `install.wim` entries; only timestamps and the shell exe differ, the latter because rustc puts random temp dir names in its import libraries). A non-Nix `build image` booted to the shell like a fresh machine. `flash` is ported line for line from the old install script but hasn't been run onto a disk since
- [x] Stock ISO fetched by driving microsoft.com's download page in headless Chromium (`dfnstrate fetch`); the page's API rejects plain HTTP clients
- [x] Registry config as typed values (`sz`, `expandSz`, `multiSz`, `dword`, `qword`, `binary`, `"delete"`), validated against the image's own hives at build time
- [x] Generated autounattend.xml: local account, no OOBE, no Microsoft account, optional disk wipe + autologon
- [x] QEMU test VM with Secure Boot and TPM 2.0
- [x] Verified install end to end in the VM: unattended setup, specialize registry import, OOBE, first-logon shell switch, our shell running with no explorer.exe
  - Fully hands-off offline (`restrict=on`): a headless run went from the ISO to our shell in ~47 minutes with no input. Online, OOBE's zero-day-patch check fails in the VM and waits for a "Skip" click. On real hardware: install offline, or hit "Try again".
- [x] Rust shell, cross-compiled in the flake (`nix build .#shell`): a tiling window manager with COSMIC's tiling and keybindings, and a Super-tap launcher over the Start Menu's shortcuts. Seen tiling, focusing, moving and launching in the VM; resize mode, workspaces, float and maximize are unit-tested only so far
  - Primary monitor only. Minimized windows drop out of the tiling and, with no taskbar, can't be brought back. Elevated windows and dialogs float, and while an elevated window has focus the shell's keys don't work (Windows doesn't give a non-elevated keyboard hook their input). A crashing shell leaves windows on other workspaces hidden
- [x] Iosevka Nerd Font Mono installed system-wide (`fonts`, fetched from the Nerd Fonts release) and used by the shell and every console. Windows 11 hands consoles to Windows Terminal once it's registered, which ignores console fonts, so the default terminal is pinned to conhost
- [x] libcosmic builds for Windows from Linux (`nix build .#cosmic-bar-prototype`); not run on Windows yet
- [ ] Shell: system tray, notifications, multiple monitors, COSMIC's stacking and swap mode
- [x] Provisioned AppX removal (`appx.remove`): deprovisioned during specialize, names checked against the image at build time. All 17 in the defaults removed in a `build image` run (from `specialize.log` on the image)
- [ ] WSL installed offline, and the machine's Linux install run through it (`wsl.linux`): its disks attached and mounted, its apps in the Start Menu, run chrooted as its user
  - Seen in a VM (Windows nested in KVM, a saturn-shaped btrfs test install as a second disk, `nix build .#test-linux-disk`): WSL installs from the image, the logon task imports the distro, attaches the disk by GPT GUID, mounts it and puts its Firefox in the Start Menu; the launcher starts it, and Firefox runs from the install as its user, with its profile in the install's home. dfn's half is also a flake check (`checks.x86_64-linux.dfn`)
  - Linux GUI windows are a non-goal. WSLg's viewer (msrdc) only connects while a real explorer runs as the shell: a dump of it under our shell shows its connection thread never starting, and faking explorer's documented surface (Shell_TrayWnd and its appbar/tray messages, the shell window, the shell-ready events, TaskbarCreated) doesn't change that. Running explorer hidden underneath works, but this Windows exists to be a console for kernel-anticheat games, so explorer stays out. The experiment is on branch `t3/wslg-windows-under-shell`
  - Terminal programs work: Start Menu → "Linux terminal" (`dfn shell`) opens the user's login shell from the install, as the user, in their home
- [x] Updating an install from Linux without a reflash (`dfnstrate update`, `build.update`): the shell, fonts and WSL payload are rewritten in place on the shut-down Windows partition, each keeping its NTFS security descriptor, and the logon task re-imports the distro when `rootfs.tar` changed
  - Seen on a `build image` output booted in the VM: `update` rewrote `rootfs.tar` and `wsl-logon.ps1` with their descriptors unchanged; at the next logon the distro was re-imported with the new `dfn` and mounted the test install, which the image's older distro had failed to; the logon after that didn't re-import. The partition was reached through ntfs-3g in a user namespace, so the by-id disk route has only been run as far as its refusals (a mounted disk, a partition, a non-by-id path), not onto a real disk
- [x] Whole-disk image built in a sandboxed VM (`dfnstrate build image`, `build.image`) and flashed from Linux with no boot media (`dfnstrate flash`, `build.install`), replacing nixconf's `saturn-windows-image.sh`. Flash-and-boot verified on a disk-sized file, and booted to the shell on real hardware

## Usage

A machine config, e.g. `my-machine.toml`:

```toml
computerName = "saturn-windows"
inputLocale = "0406:00000406"          # Danish keyboard
timeZone = "Romance Standard Time"     # Windows zone ID, not IANA
installTarget = "/dev/disk/by-id/nvme-Samsung_SSD_970_EVO_1TB_S5H9NS1NB05355E"

[user]
name = "cramt"

[wsl.linux]                            # the Linux install on the other disk; see below
user = "cramt"
disks = ["<lsblk -dno PTUUID of its disk>"]
mounts = [
  { device = "LABEL=nvme-pool", target = "/", fsType = "btrfs", options = ["subvol=@root"] },
  { device = "LABEL=nvme-pool", target = "/nix", fsType = "btrfs", options = ["subvol=@nix"] },
  { device = "LABEL=nvme-pool", target = "/home", fsType = "btrfs", options = ["subvol=@home"] },
]

[registry.defaultUser.'Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced']
HideFileExt.dword = 0
```

Configs given with `-c` are layered in order over `configs/default.toml`:
tables merge, lists append, other values are replaced. `user.name` is the
only required key. Leave `disk` at the default `interactive` for a USB
stick; `installTarget` is only for `flash`.

### Without Nix

Build dfnstrate (stable Rust) with the shell compiled in. The shell is a
Windows binary, so it needs the MinGW cross toolchain once:

```sh
# Arch / CachyOS: sudo pacman -S rustup mingw-w64-gcc
rustup target add x86_64-pc-windows-gnu
cargo build --release --manifest-path shell/Cargo.toml --target x86_64-pc-windows-gnu
DEFENESTRATE_SHELL_EXE=$PWD/shell/target/x86_64-pc-windows-gnu/release/defenestrate-shell.exe \
  cargo install --path cli
```

The programs it drives, by command. Each command checks for its own before
starting and names what's missing:

| Command | Needs (Arch packages) |
| --- | --- |
| `fetch` (and `build` without `--iso`) | `chromium` (or Chrome) |
| `build iso` | `7zip wimlib hivex libisoburn` |
| `build image`, `vm` | the above, `qemu-full swtpm edk2-ovmf mtools sleuthkit util-linux`, and access to `/dev/kvm` |
| `flash` | `util-linux ntfs-3g efibootmgr sudo` |
| `update` | `util-linux ntfs-3g sudo` |

Then:

```sh
dfnstrate build iso -c my-machine.toml -o out/iso       # USB stick ISO
dfnstrate build image -c my-machine.toml -o out/image   # finished install, set up in a VM
dfnstrate flash --image out/image -c my-machine.toml    # erases installTarget
dfnstrate update -c my-machine.toml                     # new shell/WSL payload onto it, no erase
```

Both builds fetch the stock ISO the config pins (`[iso]` in
`configs/default.toml`) into `~/.cache/dfnstrate` first, along with the
fonts and app installers, all checked by hash. Pass `--iso FILE` to use one
you already downloaded. `dfnstrate fetch --update` downloads whatever build
Microsoft serves now and prints the new pin. Builds need scratch space
next to `-o` (or `--work-dir`): ~25 GB for an ISO, ~45 GB for an image. Put
it on an SSD for `build image`: on spinning disks the VM's writes starve
its reads from the install CD until Windows drops the CD, and setup fails
with 0x800701B1 or WinPE blue-screens.

On Arch, `edk2-ovmf` has no Microsoft Secure Boot keys enrolled; setup still
installs, with Secure Boot in setup mode. `--ovmf-code`/`--ovmf-vars` pick
other firmware.

### With Nix

```sh
nix run github:cramt/defenestrate -- build iso -c my-machine.toml -o out/iso   # the same CLI, tools included
```

Or as derivations, which fetch everything as fixed-output derivations and
run dfnstrate offline in the sandbox:

```nix
machine = defenestrate.lib.x86_64-linux.mkDistro [ ./my-machine.toml ];  # or a .nix module with the same keys
# machine.build.iso / .image / .vm / .install / .update / .rendered
```

```sh
nix build .#vm-iso   # ISO for the test VM (configs/vm.toml): wipes disk 0, logs straight in
nix run .#vm         # boot it in QEMU; state lives in ./.vm
```

The ISO fixed-output derivation has the same store path as
`nix-store --add-fixed sha256 <the pinned ISO>`, so an ISO already added
that way is used without downloading.

A full install in the VM takes about an hour on a loaded 8-core desktop
with the VM disk on an HDD. Shell logs go to `%LOCALAPPDATA%\defenestrate\shell.log`.
The ISO build takes a few minutes, mostly spent exporting the edition out of
the 8 GB `install.wim`.

### Installing onto a disk without a USB stick

`dfnstrate build image` (`build.image`) is a whole-disk image: the whole
unattended install (setup, specialize, OOBE, first logon's shell switch)
runs in a VM like the one above (KVM; under Nix, `requiredSystemFeatures = [ "kvm" ]`) against a
64 GiB scratch disk, offline, and first logon powers the VM off instead of
rebooting. The build then checks Windows Boot Manager is on the image's ESP
and reads first logon's log straight off NTFS with sleuthkit, which also
proves BitLocker didn't touch C:. Progress goes to the build log; a 20
minute stall or 3 hours fails the build and leaves screenshots in the work
dir (under Nix, keep it with `--keep-failed`). ~10 minutes on an idle desktop; 65–75 on one shared with other heavy jobs. The disk is NVMe, so the
image boots on a real NVMe controller through the same inbox driver, no
sysprep.

`dfnstrate flash` (`build.install`) flashes it, like an SD card image:

```sh
dfnstrate flash --image out/image -c my-machine.toml       # erases installTarget
dfnstrate flash --image out/image /dev/disk/by-id/nvme-…   # or any whole disk, by-id
nix run .#…build.install                                    # the same, from a mkDistro config
```

It refuses a disk that isn't addressed by-id, a partition, anything smaller
than the image, and anything with a mounted filesystem, a member of a
mounted multi-device filesystem, or an md/LVM/dm holder. Then it re-runs
itself under sudo, asks you to type `ERASE`, discards the disk, writes the
image, moves the backup GPT to the disk's end and grows Windows (the last
partition, resized in place so the partition GUID the BCD boots by stays)
with `ntfsresize`. Last it adds a "Windows Boot Manager" firmware entry
without touching the boot order.

Windows takes the whole disk, its own ESP included. The first boot finds the
real hardware the way Windows does after a motherboard swap and comes up at
the shell: on a file the size of a 1 TB disk put through exactly these
steps, booted with fresh firmware variables and a fresh TPM, that took ~2
minutes with no setup screens. Network or not doesn't matter, OOBE already
ran offline in the sandbox.

### Updating an install

A fix to the shell or the WSL payload doesn't need a rebuild and reflash.
With Windows shut down, `dfnstrate update` (`build.update`) renders the
config and rewrites defenestrate's files on the Windows partition from
Linux:

```sh
dfnstrate update -c my-machine.toml --dry-run    # what would change
dfnstrate update -c my-machine.toml              # installTarget, or name the disk by-id
nix run .#…build.update
```

It takes the same disk `flash` does (by-id, whole disk, nothing on it in
use), re-runs itself under sudo and mounts partition 3 with ntfs-3g. It
refuses a hibernated volume, or one Fast Startup left half-open (the default
config turns Fast Startup off), and one Windows didn't shut down cleanly.
Then, per file the build puts on C:

- unchanged: left alone;
- changed, and read at every logon: rewritten in place. That's the shell
  exe, the fonts, and the WSL payload (`wsl-logon.ps1`, `wsl.json`,
  `rootfs.tar`, which the logon task re-imports);
- changed, but only read by setup (the `.reg` files, `specialize.cmd`, the
  WSL MSI and task): left alone and listed, since they take a reflash;
- not on the install: never created, and listed. Each would need setup to
  register it (a font, a shell under a new name, `wsl.linux` added later),
  and ntfs-3g gives a file it creates a descriptor granting Everyone full
  control.

In place means truncated and rewritten, not replaced: the file stays the same
NTFS file with its own security descriptor (see the ACL note at the end).
Changes take effect at the next logon.

### Linux apps, through WSL

There's no Windows app management. Windows gets WSL, and on a dual-boot
machine the apps are the Linux install's own: the same Firefox binary,
profile, bookmarks and logins.

WSL's MSI is pinned in `configs/default.toml` and installed at first logon
from the image, so it needs no network. With `wsl.linux` set, first logon
also registers the logon task `defenestrate-wsl`. It runs elevated
(`wsl --mount` needs admin) in session 0, so it never shows a window, and on
each logon it:

1. imports the `defenestrate` distro from `rootfs.tar` the first time, and
   again whenever that file has changed (by its SHA-256), so `update` reaches
   it. That's static busybox and `dfn` (`cli/assets/dfn.sh`), about a
   megabyte, with nothing in it worth keeping;
2. attaches the disks named in `wsl.linux.disks` to WSL (`wsl --mount
   --bare`), found by GPT disk GUID, which Windows and Linux both report;
3. runs `dfn mount`, which mounts `wsl.linux.mounts` at `/linux` the way
   NixOS mounts its `fileSystems`, by `LABEL=` or `UUID=` because device
   names differ inside WSL, then binds `/proc`, `/dev`, WSLg's sockets and
   WSL's DNS into it, and gives it a fresh `/run` with `current-system`;
4. writes a Start Menu shortcut under Programs\Linux for every desktop entry
   the user would see on Linux. Each one runs `wslg.exe … dfn launch <id>`,
   which chroots into the install and runs the entry's command as the user
   through a login shell, so the profiles' environment applies.

The shell's launcher lists the Start Menu, so Linux apps are a Super-tap
away. Logs go to `%LOCALAPPDATA%\defenestrate\wsl.log`.

Only one OS runs at a time on a dual boot, so sharing the home directory is
safe, as long as Linux was shut down rather than hibernated.

### Keys

COSMIC's defaults (`cosmic-comp`'s `data/keybindings.ron`), where the shell
has the feature:

| Keys | Does |
| --- | --- |
| Super (tap), Super+/, Super+A | Launcher: type to filter, Enter starts it, or runs the text as a command if nothing matches |
| Super+H/J/K/L or arrows | Focus left/down/up/right |
| Super+Shift+direction | Move the window |
| Super+Ctrl+direction | Previous/next workspace (with Shift: take the window along) |
| Super+1–9, Super+Shift+1–9 | Switch to / send the window to a workspace |
| Super+O | Flip the split the window is in |
| Super+M, Super+G, Super+Y | Maximize, float the window, tiling off for the workspace |
| Super+R (Super+Shift+R) | Resize mode: directions grow (shrink) that edge, Enter/Escape leave |
| Super+Q | Close |
| Super+T, Super+Enter | Terminal |
| Super+F | Home folder |
| Super+Escape, Super+Shift+Escape | Lock, log out |

Win+L would lock the session before the shell could see it, so the
default config sets `DisableLockWorkstation`. That blocks `LockWorkStation()`
too, so Super+Escape locks by disconnecting the session (`tsdiscon`), which
shows the same lock screen and reconnects to the same session on sign-in.

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
