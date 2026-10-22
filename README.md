# defenestrate

A Windows distro, NixOS style: a flake that takes Microsoft's stock Windows 11
ISO and builds an install ISO with explorer.exe thrown out of the window and
the rest of Microsoft's opinions configured away.

Everything is declared in `configs/` as typed Nix options: edition, locale,
user, disk layout, replacement shell, and registry values per hive.

## Status

- [x] Stock ISO → single-edition install ISO, UDF, BIOS + UEFI boot
- [x] Registry config as typed Nix (`sz`, `expandSz`, `multiSz`, `dword`, `qword`, `binary`, delete), validated against the image's own hives at build time
- [x] Generated autounattend.xml: local account, no OOBE, no Microsoft account, optional disk wipe + autologon
- [x] QEMU test VM with Secure Boot and TPM 2.0
- [ ] Verified install end to end in the VM
- [ ] Rust shell (cross-compiled in the flake) replacing the `cmd.exe` placeholder
- [ ] Provisioned AppX removal (Outlook, Teams, Copilot, …)
- [ ] WSL2 with a nix-built kernel and NixOS-WSL, mounting the Linux drives
- [ ] Output an installed partition image instead of an ISO (replaces nixconf's `saturn-windows-image.sh`)

## Usage

Download the Windows 11 ISO named in `nix/source.nix` from
<https://www.microsoft.com/software-download/windows11> and add it to the store
once:

```sh
nix-store --add-fixed sha256 Windows11_Client_x64_en-us_26300_9457.iso
```

Then:

```sh
nix build .#vm-iso   # ISO for the test VM: wipes disk 0, logs straight in
nix run .#vm         # boot it in QEMU; state lives in ./.vm
```

The build needs ~25 GB of scratch space and takes a few minutes, mostly
spent exporting the edition out of the 8 GB `install.wim`.

For real hardware, build a config with `lib.x86_64-linux.mkDistro [ ./configs/default.nix ./my-machine.nix ]`.
It must set `user.name` and should leave `disk` at the default `interactive`.

## How it works

1. 7-Zip unpacks the ISO. Microsoft's ISO is UDF-only, so xorriso can't read it.
2. wimlib exports the one edition we want from `install.wim`.
3. Each hive's `.reg` file is merged into a scratch copy of that hive with
   hivex. This is only a check: a malformed `.reg` fails the build instead
   of a VM install.
4. The `.reg` files and `specialize.cmd` are added to the image under
   `C:\Windows\Setup\defenestrate`. During the specialize pass, Windows runs
   `reg import` on them itself.
5. `boot.wim` gets the answer file and a `winpeshl.ini` that forces legacy
   setup, because the 24H2+ setup drops the specialize/oobeSystem passes.
6. cdrtools' mkisofs writes a UDF ISO. The single-edition `install.wim` is still
   7.5 GB, over ISO9660's 4 GiB file limit.

Why the registry isn't edited offline: when wimlib replaces an existing file
from Linux, the file loses its NTFS security descriptor and gets its parent's
inherited ACL. For registry hives that's a HiveNightmare-style bug, so Windows
applies the edits instead.
