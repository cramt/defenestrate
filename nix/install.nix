{ lib, writeShellApplication, qemu_kvm, swtpm, OVMFFull, util-linux, efibootmgr, gnugrep, coreutils, systemd, socat, iso, installTarget }:

# Installs onto a real disk with no boot media: setup runs in a VM that sees
# that one disk and nothing else, and the VM stops at setup's first reboot.
# Specialize and OOBE then run on the real machine, so Windows meets its own
# hardware the normal way.
writeShellApplication {
  name = "defenestrate-install";
  runtimeInputs = [ qemu_kvm swtpm util-linux efibootmgr gnugrep coreutils systemd socat ];
  text = ''
    die() { echo "defenestrate-install: $*" >&2; exit 1; }

    target=''${1:-${lib.optionalString (installTarget != null) installTarget}}
    [ -n "$target" ] || die "usage: defenestrate-install /dev/disk/by-id/<whole disk>  (erases it; installTarget sets a default)"

    # Kernel names (nvme0n1, sda) reorder between boots; by-id doesn't.
    case "$target" in
      /dev/disk/by-id/*-part*) die "$target is a partition. Windows gets the whole disk, its own ESP included." ;;
      /dev/disk/by-id/*) ;;
      *) die "address the disk by /dev/disk/by-id/, not $target" ;;
    esac
    [ -b "$target" ] || die "$target is not a block device"
    dev=$(readlink -f "$target")
    [ "$(lsblk -dno TYPE "$dev")" = disk ] || die "$dev is not a whole disk"

    # Refuse anything in use. Mountpoints alone miss a multi-device btrfs,
    # which shows its mount on one member only, so filesystem UUIDs are
    # matched against everything mounted too. Holders catch md, LVM and dm.
    mounts=$(lsblk -rno MOUNTPOINTS "$dev" | grep -v '^$' || true)
    [ -z "$mounts" ] || die "$dev has mounted filesystems: $mounts"
    mounted_uuids=$(findmnt -rno UUID | grep -v '^$' || true)
    for uuid in $(lsblk -rno UUID "$dev"); do
      if grep -qxF "$uuid" <<<"$mounted_uuids"; then
        die "$dev holds part of the mounted filesystem $uuid"
      fi
    done
    name=$(basename "$dev")
    for holders in /sys/block/"$name"/holders /sys/block/"$name"/"$name"*/holders; do
      [ -d "$holders" ] || continue
      [ -z "$(ls -A "$holders")" ] || die "$dev is in use by $(ls "$holders")"
    done

    [ -d /sys/firmware/efi/efivars ] || die "not booted in UEFI mode, so there's no firmware boot entry to add"
    # Checked as the caller, then again as root: nix evaluates and builds
    # as you, and only writing the disk escalates.
    if [ "$(id -u)" != 0 ]; then
      exec sudo "$0" "$target"
    fi

    lsblk -o NAME,SIZE,MODEL,SERIAL,FSTYPE,LABEL "$dev"
    echo
    echo "Everything on $target will be erased."
    read -rp "Type ERASE to install Windows onto it: " answer
    [ "$answer" = ERASE ] || die "not confirmed, nothing touched"

    state=$(mktemp -d)
    swtpm_pid=
    qemu_pid=
    cleanup() {
      [ -z "$qemu_pid" ] || kill "$qemu_pid" 2>/dev/null || true
      [ -z "$swtpm_pid" ] || kill "$swtpm_pid" 2>/dev/null || true
      umount "$state/esp" 2>/dev/null || true
      rm -rf "$state"
    }
    trap cleanup EXIT

    mkdir "$state/tpm" "$state/esp"
    # The .ms variant ships Microsoft's Secure Boot keys pre-enrolled, which
    # Windows 11 setup insists on alongside the TPM. Both are throwaway: the
    # real machine's firmware and TPM take over at first boot.
    install -m 644 ${OVMFFull.fd}/FV/OVMF_VARS.ms.fd "$state/OVMF_VARS.fd"
    swtpm socket --tpm2 \
      --tpmstate dir="$state/tpm" \
      --ctrl type=unixio,path="$state/swtpm.sock" \
      --terminate &
    swtpm_pid=$!
    while [ ! -S "$state/swtpm.sock" ]; do sleep 0.1; done

    # NVMe, not vm.nix's SATA: the install has to boot on the real NVMe
    # controller through the same inbox stornvme driver it was set up with.
    # CD first, because the disk may still hold something bootable;
    # -no-reboot means it only ever boots once, so setup can't loop.
    # -no-reboot is also the whole trick: qemu exits at setup's first reboot,
    # with the image applied and the disk's own ESP and BCD written.
    # No network: the windowsPE pass needs none.
    # cache=none: the disk is the real one, so writes must land.
    # Headless, so it runs as root with no session to borrow, and you can walk
    # away. The monitor socket is how the screen gets seen anyway.
    qemu-system-x86_64 \
      -name defenestrate-install \
      -machine q35,smm=on,accel=kvm \
      -global driver=cfi.pflash01,property=secure,value=on \
      -global ICH9-LPC.disable_s3=1 \
      -cpu host,hv-passthrough -smp 4 -m 8G \
      -drive if=pflash,format=raw,unit=0,readonly=on,file=${OVMFFull.fd}/FV/OVMF_CODE.ms.fd \
      -drive if=pflash,format=raw,unit=1,file="$state/OVMF_VARS.fd" \
      -chardev socket,id=chrtpm,path="$state/swtpm.sock" \
      -tpmdev emulator,id=tpm0,chardev=chrtpm \
      -device tpm-crb,tpmdev=tpm0 \
      -drive id=disk,file="$dev",if=none,format=raw,cache=none,aio=native \
      -device nvme,drive=disk,serial=defenestrate,bootindex=1 \
      -device ahci,id=ahci \
      -drive id=cd,file=${iso}/defenestrate.iso,if=none,media=cdrom,readonly=on \
      -device ide-cd,drive=cd,bus=ahci.0,bootindex=0 \
      -nic none \
      -device qemu-xhci -device usb-tablet \
      -display none \
      -monitor unix:"$state/monitor.sock",server=on,wait=off \
      -no-reboot &
    qemu_pid=$!

    # Setup is unattended, so the only questions are "is it moving" and, if
    # not, "what's on screen". Bytes written to the disk answer the first; a
    # screenshot every 5 minutes, and on a 10 minute stall, the second.
    shot=/tmp/defenestrate-install.png
    rm -f "$shot"
    screendump() {
      echo "screendump $shot -f png" | socat - UNIX-CONNECT:"$state/monitor.sock" > /dev/null 2>&1 || true
    }
    written() {
      local w
      read -r _ _ _ _ _ _ w _ < "/sys/block/$name/stat"
      echo "$w"
    }
    echo "setup is running headless; it stops by itself at its first reboot. Screen: $shot"
    start=$(written)
    last=$start
    idle=0
    ticks=0
    while kill -0 "$qemu_pid" 2>/dev/null; do
      sleep 30
      now=$(written)
      ticks=$((ticks + 1))
      if [ "$now" = "$last" ]; then idle=$((idle + 30)); else idle=0; fi
      last=$now
      echo "$(date +%T)  $(( (now - start) * 512 / 1073741824 )) GiB written"
      if [ $((ticks % 10)) = 0 ] || [ "$idle" = 600 ]; then screendump; fi
      if [ "$idle" = 600 ]; then
        echo "nothing written for 10 minutes; setup may be stuck on a dialog, see $shot"
      fi
    done
    wait "$qemu_pid" || die "qemu exited with an error"
    qemu_pid=

    # Setup failing doesn't always end qemu with an error, so check the
    # result rather than trusting the exit.
    blockdev --rereadpt "$dev" 2>/dev/null || true
    udevadm settle
    esp="$target-part1"
    [ -b "$esp" ] || die "setup didn't finish: $dev has no partition 1"
    mount -o ro "$esp" "$state/esp" || die "setup didn't finish: $esp doesn't mount"
    [ -f "$state/esp/EFI/Microsoft/Boot/bootmgfw.efi" ] || die "setup didn't finish: no Windows Boot Manager on $esp"
    umount "$state/esp"

    # The VM's boot entry went away with its OVMF vars. Add one to the real
    # firmware, unless it already has one for this ESP. --create-only leaves
    # BootOrder alone, so the existing OS still boots by default.
    partuuid=$(lsblk -no PARTUUID "$esp")
    [ -n "$partuuid" ] || die "$esp has no partition UUID, so there's nothing to point a boot entry at"
    if efibootmgr -v | grep -qi "$partuuid"; then
      echo "firmware already has a boot entry for $esp"
    else
      efibootmgr --create-only --disk "$dev" --part 1 \
        --label "Windows Boot Manager" \
        --loader '\EFI\Microsoft\Boot\bootmgfw.efi' > /dev/null
      echo "added a \"Windows Boot Manager\" firmware entry for $esp"
    fi

    echo "Setup's first stage is done. Boot the machine into Windows to finish it on the real hardware."
  '';
}
