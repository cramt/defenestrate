{ runCommand, qemu_kvm, swtpm, OVMFFull, socat, util-linux, jq, mtools, sleuthkit, zstd, e2fsprogs, iso, size ? "64G" }:

# A whole-disk Windows image, built in the sandbox: the whole unattended
# install (windowsPE, specialize, OOBE, first logon's shell switch) runs in
# a VM against a scratch disk, offline, and ends with first logon powering
# the VM off. The real machine's first boot is then just a boot: Windows
# finds its new hardware the way it does after a motherboard swap. The disk
# is NVMe so the image boots on a real NVMe controller through the same
# inbox driver it was installed with, no sysprep needed.
runCommand "defenestrate-image"
  {
    nativeBuildInputs = [ qemu_kvm swtpm socat util-linux jq mtools sleuthkit zstd e2fsprogs ];
    requiredSystemFeatures = [ "kvm" ];
    # The install ISO is ~7 GB that only this machine has.
    preferLocalBuild = true;
    allowSubstitutes = false;
  }
  ''
    mkdir tpm
    truncate -s 0 disk.raw
    # Build dirs can sit on btrfs, where a raw disk being rewritten in place
    # fragments badly. Only takes on an empty file; harmless elsewhere.
    chattr +C disk.raw 2>/dev/null || true
    truncate -s ${size} disk.raw
    install -m 644 ${OVMFFull.fd}/FV/OVMF_VARS.ms.fd OVMF_VARS.fd

    swtpm socket --tpm2 --tpmstate dir=tpm \
      --ctrl type=unixio,path=swtpm.sock --terminate &
    swtpm_pid=$!
    for _ in $(seq 100); do [ -S swtpm.sock ] && break; sleep 0.1; done
    [ -S swtpm.sock ] || { echo "swtpm never came up" >&2; exit 1; }

    # See vm.nix for hv-passthrough and the Secure Boot firmware. Disk first:
    # blank, it falls through to the CD; installed, it boots Windows instead
    # of starting setup over. No network, so OOBE skips its update check
    # instead of waiting on it.
    qemu-system-x86_64 \
      -name defenestrate-image \
      -machine q35,smm=on,accel=kvm \
      -global driver=cfi.pflash01,property=secure,value=on \
      -global ICH9-LPC.disable_s3=1 \
      -cpu host,hv-passthrough -smp 4 -m 8G \
      -drive if=pflash,format=raw,unit=0,readonly=on,file=${OVMFFull.fd}/FV/OVMF_CODE.ms.fd \
      -drive if=pflash,format=raw,unit=1,file=OVMF_VARS.fd \
      -chardev socket,id=chrtpm,path=swtpm.sock \
      -tpmdev emulator,id=tpm0,chardev=chrtpm \
      -device tpm-crb,tpmdev=tpm0 \
      -drive id=disk,file=disk.raw,if=none,format=raw,cache=unsafe \
      -device nvme,drive=disk,serial=defenestrate,bootindex=0 \
      -device ahci,id=ahci \
      -drive id=cd,file=${iso}/defenestrate.iso,if=none,media=cdrom,readonly=on \
      -device ide-cd,drive=cd,bus=ahci.0,bootindex=1 \
      -nic none \
      -device qemu-xhci -device usb-tablet \
      -display none \
      -monitor unix:monitor.sock,server=on,wait=off &
    qemu_pid=$!

    # Unattended setup either writes steadily or is stuck on a dialog nobody
    # will click. Fail on a stall instead of hanging the build, and leave
    # screenshots behind for `nix build -K`.
    screendump() {
      (echo "screendump $PWD/screen-$1.png -f png"; sleep 2) \
        | timeout 10 socat - UNIX-CONNECT:monitor.sock > /dev/null 2>&1 || true
    }
    allocated() { du -k disk.raw | cut -f1; }
    last=$(allocated)
    idle=0
    elapsed=0
    while kill -0 "$qemu_pid" 2>/dev/null; do
      sleep 30
      elapsed=$((elapsed + 30))
      now=$(allocated)
      if [ "$now" = "$last" ]; then idle=$((idle + 30)); else idle=0; fi
      last=$now
      echo "$((elapsed / 60))m: $((now / 1048576)) GiB written"
      if [ $((elapsed % 300)) = 0 ]; then screendump "$((elapsed / 60))m"; fi
      # Before setup touches the disk (boot, WinPE start) quiet is normal.
      if [ "$now" -gt 1048576 ] && [ "$idle" -ge 1200 ]; then
        screendump stalled
        echo "nothing written for 20 minutes, setup is stuck" >&2
        exit 1
      fi
      if [ "$elapsed" -ge 10800 ]; then
        screendump timeout
        echo "setup took over 3 hours" >&2
        exit 1
      fi
    done
    wait "$qemu_pid"
    kill "$swtpm_pid" 2>/dev/null || true

    # A power-off only means something shut Windows down, so check what's on
    # the disk: Windows Boot Manager on its own ESP, and the log first logon
    # writes, read straight off NTFS. That last one also fails on a volume
    # BitLocker got to, which would be a brick once flashed.
    esp_start=$(sfdisk --json disk.raw | jq '.partitiontable.partitions[0].start')
    mdir -i "disk.raw@@$((esp_start * 512))" ::/EFI/Microsoft/Boot/bootmgfw.efi > /dev/null \
      || { echo "setup didn't finish: no Windows Boot Manager on the image's ESP" >&2; exit 1; }
    windows_start=$(sfdisk --json disk.raw | jq '.partitiontable.partitions[2].start')
    ifind -o "$windows_start" -n /Windows/Setup/defenestrate/firstlogon.log disk.raw > /dev/null \
      || { echo "setup didn't finish: first logon never ran (or C: isn't readable NTFS)" >&2; exit 1; }

    mkdir $out
    stat -c %s disk.raw > $out/size
    zstd -T0 -q disk.raw -o $out/windows.img.zst
  ''
