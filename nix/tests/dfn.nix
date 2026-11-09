# dfn, the Linux half of wsl.linux, without Windows: a VM unpacks the
# defenestrate distro that `dfnstrate render` made, stands in for what WSL
# gives a distro (API filesystems, WSLg's sockets, resolv.conf), attaches
# the test disk, and drives dfn the way the logon task and shortcuts do.
{ pkgs, rendered, linuxDisk }:

pkgs.vmTools.runInLinuxVM (pkgs.runCommand "dfn-test"
  {
    # snapshot=on: writes (dfn's mounts are read-write) go to a scratch
    # overlay, not the shared test disk.
    QEMU_OPTS = "-drive file=${linuxDisk}/disk.raw,if=virtio,format=raw,snapshot=on";
    memSize = 2048;
    nativeBuildInputs = [ pkgs.gnutar pkgs.kmod pkgs.util-linux pkgs.python3 ];
  }
  ''
    set -x
    # WSL's kernel has btrfs as a module that dfn loads; this kernel's
    # modules aren't in the distro, so load it from here.
    modprobe btrfs

    d=/distro
    mkdir $d
    tar -C $d -xf ${rendered}/wsl/rootfs.tar
    for fs in proc sys dev; do mount --rbind /$fs $d/$fs; done
    mkdir -p $d/dev/pts $d/dev/shm $d/tmp/.X11-unix $d/mnt/wslg/runtime-dir/pulse
    # dfn waits for WSLg's compositor socket before starting an app.
    python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' $d/mnt/wslg/runtime-dir/wayland-0
    echo "nameserver 192.0.2.1" > $d/etc/resolv.conf

    dfn() { chroot $d /defenestrate/dfn "$@"; }
    dfn mount
    dfn mount # again: idempotent, as every logon runs it

    uid=$(dfn run id -u)
    [ "$uid" = 1000 ] || { echo "ran as uid $uid, not the user's 1000"; exit 1; }
    dfn run sh -c 'touch ~/written-from-wsl'
    [ "$(stat -c %u $d/linux/home/user/written-from-wsl)" = 1000 ]

    dfn apps | tee apps
    grep -q '^firefox.desktop	Firefox' apps || grep -qi 'firefox' apps

    dfn run firefox --version | tee version
    grep -q 'Mozilla Firefox' version

    # The session environment a desktop would give it.
    dfn run sh -c 'echo "$XDG_RUNTIME_DIR $WAYLAND_DISPLAY $PULSE_SERVER"; ls -l "$XDG_RUNTIME_DIR"' | tee env
    grep -q '/run/wslg/runtime-dir wayland-0 unix:/run/wslg/runtime-dir/pulse/native' env
    grep -q 'wayland-0' env
    [ "$(cat $d/linux/etc/resolv.conf)" = "nameserver 192.0.2.1" ] || readlink $d/linux/etc/resolv.conf

    mkdir $out
    cp apps version env $out/
  '')
