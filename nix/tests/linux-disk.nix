# A Linux install shaped like saturn's 980 (one btrfs pool with @root, @nix
# and @home subvolumes), with Firefox for user "user", as a raw disk image to
# attach to a defenestrate VM. configs/vm.toml's wsl.linux describes it.
#
#   nix build .#test-linux-disk   →   result/disk.raw
{ pkgs, lib, nixpkgs }:

let
  # configs/vm.toml names the disk by this.
  diskGuid = "d3f3e5a7-5c4e-4b7a-9f1e-6a0d2c7b8e10";
  label = "linux-pool";
  subvol = name: {
    device = "/dev/disk/by-label/${label}";
    fsType = "btrfs";
    options = [ "subvol=${name}" "compress=zstd:1" "noatime" ];
  };

  nixos = import "${nixpkgs}/nixos/lib/eval-config.nix" {
    system = "x86_64-linux";
    modules = [{
      # Never booted itself: it only has to be a real install to chroot into.
      boot.loader.grub.enable = false;
      fileSystems."/" = subvol "@root";
      fileSystems."/nix" = subvol "@nix";
      fileSystems."/home" = subvol "@home";
      users.users.user = {
        isNormalUser = true;
        uid = 1000;
        packages = [ pkgs.firefox ];
      };
      documentation.enable = false;
      system.stateVersion = "26.05";
    }];
  };
  toplevel = nixos.config.system.build.toplevel;
in
pkgs.vmTools.runInLinuxVM (pkgs.runCommand "test-linux-disk"
  {
    nativeBuildInputs = [ pkgs.btrfs-progs pkgs.util-linux pkgs.nix nixos.config.system.build.nixos-install pkgs.nixos-enter ];
    # Built outside $out: $out is shared into the VM, and QEMU can't lock a
    # disk there.
    preVM = ''
      truncate -s 12G disk.raw
    '';
    QEMU_OPTS = "-drive file=$NIX_BUILD_TOP/disk.raw,if=virtio,format=raw,cache=unsafe,werror=report";
    postVM = ''
      mkdir -p $out
      mv $NIX_BUILD_TOP/disk.raw $out/disk.raw
    '';
    memSize = 4096;
    passthru = { inherit diskGuid toplevel; };
  }
  ''
    sfdisk /dev/vda <<EOF
    label: gpt
    label-id: ${diskGuid}
    size=1G, type=uefi
    type=linux
    EOF
    mkfs.btrfs -q -L ${label} /dev/vda2
    mkdir /pool /mnt
    mount /dev/vda2 /pool
    for s in @root @nix @home; do btrfs subvolume create /pool/$s; done
    mount -o subvol=@root /dev/vda2 /mnt
    mkdir -p /mnt/nix /mnt/home
    mount -o subvol=@nix /dev/vda2 /mnt/nix
    mount -o subvol=@home /dev/vda2 /mnt/home

    export NIX_STATE_DIR=$TMPDIR/state
    nix-store --load-db < ${pkgs.closureInfo { rootPaths = [ toplevel ]; }}/registration
    nixos-install --root /mnt --system ${toplevel} --no-bootloader --no-root-passwd --no-channel-copy --substituters ""

    # A real install gets /etc, its users and their homes from activation at
    # first boot. This one never boots, so activate it here.
    nixos-enter --root /mnt -c /nix/var/nix/profiles/system/activate
    grep '^user:' /mnt/etc/passwd
    # dfn runs Firefox as uid 1000, which needs a home it can write.
    [ "$(stat -c %u /mnt/home/user)" = 1000 ]
    umount /mnt/home /mnt/nix /mnt /pool
  '')
