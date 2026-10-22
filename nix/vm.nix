{ writeShellApplication, qemu_kvm, swtpm, e2fsprogs, OVMFFull, iso }:

writeShellApplication {
  name = "defenestrate-vm";
  runtimeInputs = [ qemu_kvm swtpm e2fsprogs ];
  text = ''
    state=''${DEFENESTRATE_VM_DIR:-$PWD/.vm}
    mkdir -p "$state/tpm"
    # qcow2 on a copy-on-write filesystem (btrfs) fragments into oblivion.
    # Only affects files created afterwards, and fails harmlessly elsewhere.
    chattr +C "$state" 2>/dev/null || true

    if [ ! -f "$state/disk.qcow2" ]; then
      qemu-img create -q -f qcow2 "$state/disk.qcow2" 64G
    fi
    # The .ms variant ships Microsoft's Secure Boot keys pre-enrolled, which
    # Windows 11 setup insists on alongside the TPM.
    if [ ! -f "$state/OVMF_VARS.fd" ]; then
      install -m 644 ${OVMFFull.fd}/FV/OVMF_VARS.ms.fd "$state/OVMF_VARS.fd"
    fi

    swtpm socket --tpm2 \
      --tpmstate dir="$state/tpm" \
      --ctrl type=unixio,path="$state/swtpm.sock" \
      --terminate &
    swtpm_pid=$!
    trap 'kill $swtpm_pid 2>/dev/null || true' EXIT
    while [ ! -S "$state/swtpm.sock" ]; do sleep 0.1; done

    # Disk before CD: a blank disk falls through to the installer, an
    # installed one boots Windows instead of starting setup over.
    # SATA and e1000e because stock Windows has no virtio drivers.
    # hv-passthrough: without the Hyper-V enlightenments Windows burns its
    # vCPUs on timer and spinlock emulation. On a loaded host (load ~35 on 8
    # cores) setup's image apply sat at 0% for 30+ minutes without them and
    # finished in ~21 with them; boot-to-apply went from 44 to 15 minutes.
    # cache=unsafe: this disk is disposable, so NTFS flushes needn't fsync.
    # restrict=on: no internet. Online, OOBE's zero-day-patch update check
    # fails in this VM ("Something went wrong, OOBEZDP") and waits for a click;
    # there's no setting to turn it off, offline it's skipped. BypassNRO lets
    # OOBE carry on without a network.
    qemu-system-x86_64 \
      -name defenestrate \
      -machine q35,smm=on,accel=kvm \
      -global driver=cfi.pflash01,property=secure,value=on \
      -global ICH9-LPC.disable_s3=1 \
      -cpu host,hv-passthrough -smp 4 -m 8G \
      -drive if=pflash,format=raw,unit=0,readonly=on,file=${OVMFFull.fd}/FV/OVMF_CODE.ms.fd \
      -drive if=pflash,format=raw,unit=1,file="$state/OVMF_VARS.fd" \
      -chardev socket,id=chrtpm,path="$state/swtpm.sock" \
      -tpmdev emulator,id=tpm0,chardev=chrtpm \
      -device tpm-crb,tpmdev=tpm0 \
      -device ahci,id=ahci \
      -drive id=disk,file="$state/disk.qcow2",if=none,format=qcow2,cache=unsafe \
      -device ide-hd,drive=disk,bus=ahci.0,bootindex=0 \
      -drive id=cd,file=${iso}/defenestrate.iso,if=none,media=cdrom,readonly=on \
      -device ide-cd,drive=cd,bus=ahci.1,bootindex=1 \
      -nic user,model=e1000e,restrict=on \
      -device qemu-xhci -device usb-tablet \
      -qmp unix:"$state/qmp.sock",server=on,wait=off \
      "$@"
  '';
}
