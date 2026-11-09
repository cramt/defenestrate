# defenestrate

A Windows distro, NixOS style: takes Microsoft's stock Windows 11 ISO and
builds an install with explorer.exe thrown out of the window and the rest of
Microsoft's opinions configured away.

Two halves:

- **`dfnstrate`** (`cli/`), the Linux side: fetches the stock ISO, builds an
  install ISO or a finished whole-disk image from a config, flashes it, and
  pins apps. A plain Rust binary; it works on any distro. The Nix flake is a
  thin layer that runs the same commands, so both paths hit the same code.
- **The shell** (`shell/`), the Windows side: a tiling window manager that
  replaces explorer.

A config is TOML (or a Nix module setting the same keys): edition, locale,
user, disk layout, shell, fonts, apps, AppX packages to drop, and registry
values per hive. Every config is layered over the distro's own defaults,
[`configs/default.toml`](configs/default.toml).

![Tiled terminals and Firefox, the Super-tap launcher open, all in Iosevka](docs/first-boot.png)

## Status

- [x] Stock ISO → single-edition install ISO (plain ISO9660, split WIM, so it fits a FAT32 stick), BIOS + UEFI boot
- [x] `dfnstrate`: one Rust CLI for fetch/build/flash/lock, layered TOML configs, a hash-addressed download cache. The flake's ISO, image, VM and install outputs run it, with the cache filled by fixed-output derivations, so a non-Nix build runs the same code as the Nix one
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
- [x] Declarative apps (`apps`, `wingetApps`, `storeApps`) pinned in `apps.lock.json` by `dfnstrate lock`; baked installers go into the image and install on the first real boot. Verified on saturn's image booted fresh in QEMU: Firefox (machine-wide, as SYSTEM) and Discord (per-user) installed with the shell's keys working throughout; League's installer asks for elevation
  - Not yet observed: the online `wingetApps`/`storeApps` path (the test VMs are offline). `wingetApps` installs a pinned version, which fails once winget-pkgs drops it; store apps get whatever is current. Baked apps that are bootstrappers (Steam, League's Riot Client) still download the real thing at runtime.
- [ ] Shell: system tray, notifications, multiple monitors, COSMIC's stacking and swap mode
- [x] Provisioned AppX removal (`appx.remove`): deprovisioned during specialize, names checked against the image at build time. All 17 in the defaults removed in a `build image` run (from `specialize.log` on the image)
- [ ] WSL2 with a nix-built kernel and NixOS-WSL, mounting the Linux drives
- [x] Whole-disk image built in a sandboxed VM (`dfnstrate build image`, `build.image`) and flashed from Linux with no boot media (`dfnstrate flash`, `build.install`), replacing nixconf's `saturn-windows-image.sh`. Flash-and-boot verified on a disk-sized file, and booted to the shell on real hardware

## Usage

A machine config, e.g. `my-machine.toml`:

```toml
computerName = "saturn-windows"
inputLocale = "0406:00000406"          # Danish keyboard
timeZone = "Romance Standard Time"     # Windows zone ID, not IANA
installTarget = "/dev/disk/by-id/nvme-Samsung_SSD_970_EVO_1TB_S5H9NS1NB05355E"
apps = ["Mozilla.Firefox"]
appsLock = "apps.lock.json"            # relative to this file

[user]
name = "cramt"

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

Then:

```sh
dfnstrate build iso -c my-machine.toml -o out/iso       # USB stick ISO
dfnstrate build image -c my-machine.toml -o out/image   # finished install, set up in a VM
dfnstrate flash --image out/image -c my-machine.toml    # erases installTarget
```

Both builds fetch the stock ISO the config pins (`[iso]` in
`configs/default.toml`) into `~/.cache/dfnstrate` first, along with the
fonts and app installers, all checked by hash. Pass `--iso FILE` to use one
you already downloaded. `dfnstrate fetch --update` downloads whatever build
Microsoft serves now and prints the new pin. Builds need ~25 GB of scratch
next to `-o` (or `--work-dir`).

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
# machine.build.iso / .image / .vm / .install / .rendered
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

### Apps

Apps are winget package IDs, pinned in `apps.lock.json` like an npm
lockfile:

```toml
apps = ["Mozilla.Firefox"]          # baked into the image, installed offline
wingetApps = ["Valve.Steam"]        # winget installs the locked version, online
storeApps = ["9NKSQGP7F2NH"]        # Microsoft Store product, online
appsLock = "apps.lock.json"
```

```sh
dfnstrate lock Mozilla.Firefox            # lock (or re-resolve) a baked app
dfnstrate lock --winget Valve.Steam --msstore 9NKSQGP7F2NH
dfnstrate lock --bump                     # newest winget-pkgs, relock everything
```

`dfnstrate lock` resolves each ID against the winget-pkgs commit recorded in the
lock, picks the installer winget would (x64, en-US, machine scope first),
and records its URL, hash, type and silent switches. Manifests without
switches get their type's standard ones (msi `/qn`, nullsoft `/S`, inno
`/VERYSILENT …`, burn `/quiet`). It downloads every baked installer once and
checks it against the manifest's hash, so a URL whose contents have moved on
fails at lock time; the download stays in the cache for the build.

Unknown IDs, msix/zip/portable-only packages, and exe installers with no
silent mode fail at lock time. An ID that isn't locked, or is locked as a
different kind than the config says, fails the build with the command that
fixes it.

The build puts every baked installer into the image under
`C:\Windows\Setup\defenestrate\apps`. The image's first logon (in the
sandbox) registers two scheduled tasks that run at the user's next logon,
the machine's first real boot, next to the shell rather than in front of it:

- As SYSTEM, in session 0: machine-wide installers. Logs to
  `C:\Windows\Setup\defenestrate\apps.log`. Not elevated in the user's
  session, because the shell's keyboard hook gets no keys while an elevated
  window has focus, and the installers held it.
- As the plain user: per-user installers (the lock records each installer's
  scope), which break when elevated, then winget/store apps, retrying for up
  to an hour while winget registers and the network comes up. Logs to
  `%LOCALAPPDATA%\defenestrate\apps.log`.

Each reruns at the next logon if anything failed and deletes itself once its
apps are in.

Some per-user installers ask for elevation themselves: League of Legends'
Riot installer shows a UAC prompt on the first boot, and clicking No (or
nothing, until it times out) leaves it for the next logon. Discord asks for
its "System Helper" on its own first launch.

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
