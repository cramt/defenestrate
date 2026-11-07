{ lib, writeShellApplication, util-linux, efibootmgr, gnugrep, coreutils, systemd, zstd, ntfs3g, image, installTarget }:

# Flashes image.nix's whole-disk image onto a real disk, then grows Windows
# to fill it. Like flashing an SD card: no VM touches the real disk. The
# image stops before specialize, so Windows meets its real hardware on the
# first boot the normal way.
writeShellApplication {
  name = "defenestrate-install";
  runtimeInputs = [ util-linux efibootmgr gnugrep coreutils systemd zstd ntfs3g ];
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

    image_size=$(cat ${image}/size)
    disk_size=$(( $(cat /sys/block/"$name"/size) * 512 ))
    [ "$disk_size" -ge "$image_size" ] || die "$dev is $((disk_size >> 30)) GiB, the image needs $((image_size >> 30)) GiB"

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

    # Discarding first means nothing of the old contents survives past the
    # image, and the SSD knows the rest is free.
    blkdiscard -f "$dev" || echo "discard unsupported, writing over the old contents"
    echo "writing $((image_size >> 30)) GiB"
    zstd -dc ${image}/windows.img.zst \
      | dd of="$dev" bs=16M iflag=fullblock oflag=direct status=progress
    sync

    # The image is smaller than the disk: put the backup GPT at the disk's
    # real end, then grow Windows, the last partition, into the rest. sfdisk
    # resizes in place, keeping the partition GUID the BCD boots by.
    sfdisk --relocate gpt-bak-std "$dev"
    echo ', +' | sfdisk --no-reread --wipe-partitions never -N 3 "$dev"
    blockdev --rereadpt "$dev"
    udevadm settle
    windows="$target-part3"
    [ -b "$windows" ] || die "$windows didn't show up after the resize"
    # Leaves the volume marked for a check, so Windows runs chkdsk once on
    # first boot. That's ntfsresize being careful, not a problem.
    echo y | ntfsresize --force --no-progress-bar "$windows" > /dev/null

    # The image has no firmware entry of its own. --create-only leaves
    # BootOrder alone, so the existing OS still boots by default.
    esp="$target-part1"
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

    echo "Done. Boot it, offline, and Windows finishes setting itself up on this hardware."
  '';
}
