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
    nativeBuildInputs = [ pkgs.gnutar pkgs.kmod pkgs.util-linux pkgs.python3 pkgs.coreutils ];
  }
  ''
    set -x
    # WSL's kernel has btrfs as a module that dfn loads; this kernel's
    # modules aren't in the distro, so load it from here.
    modprobe btrfs

    d=/distro
    mkdir $d
    tar -C $d -xf ${rendered}/wsl/rootfs.tar
    # A WSL distro's root is a mount (its moved onto / below), so bind it
    # onto itself first: binding over it later would hide what's under it.
    mount --bind $d $d
    for fs in proc sys dev; do mount --rbind /$fs $d/$fs; done
    mkdir -p $d/dev/pts $d/dev/shm $d/tmp/.X11-unix $d/mnt/wslg/runtime-dir/pulse
    # dfn waits for WSLg's compositor socket before starting an app.
    python3 -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' $d/mnt/wslg/runtime-dir/wayland-0
    echo "nameserver 192.0.2.1" > $d/etc/resolv.conf

    # Enter the distro the way WSL does: a mount namespace with the distro
    # moved onto / (so it is that namespace's root, which user namespaces
    # need), held open by a process every dfn call joins, as a running WSL
    # instance's mounts are shared by everything run in it.
    unshare -m --propagation private sh -c "cd $d && mount --move . / && exec chroot . /bin/busybox sleep 100000" &
    holder=$!
    inside() { nsenter -t $holder -m -r -w -- "$@"; }
    for _ in $(seq 50); do inside /bin/busybox true 2>/dev/null && break; sleep 0.2; done
    dfn() { inside /defenestrate/dfn "$@"; }

    dfn mount
    dfn mount # again: idempotent, as every logon runs it

    uid=$(dfn run id -u)
    [ "$uid" = 1000 ] || { echo "ran as uid $uid, not the user's 1000"; exit 1; }
    dfn run sh -c 'touch ~/written-from-wsl'
    [ "$(inside /bin/busybox stat -c %u /linux/home/user/written-from-wsl)" = 1000 ]

    # Supplementary groups, as op-secrets' group-readable secrets need.
    [ "$(dfn run cat /var/lib/test-secret/key)" = s3cret ]
    # User namespaces, which Firefox-family sandboxes need and the kernel
    # refuses to a chrooted process.
    dfn run unshare -U true

    # The "Linux terminal": the user's login shell, here fed a command.
    [ "$(echo 'id -u' | dfn shell)" = 1000 ]

    dfn apps | tee apps
    grep -q '^firefox.desktop	Firefox' apps || grep -qi 'firefox' apps

    dfn run firefox --version | tee version
    grep -q 'Mozilla Firefox' version

    # The session environment a desktop would give it.
    dfn run sh -c 'echo "$XDG_RUNTIME_DIR $WAYLAND_DISPLAY $PULSE_SERVER"; ls -l "$XDG_RUNTIME_DIR"' | tee env
    grep -q '/run/wslg/runtime-dir wayland-0 unix:/run/wslg/runtime-dir/pulse/native' env
    grep -q 'wayland-0' env
    [ "$(inside /bin/busybox cat /linux/etc/resolv.conf)" = "nameserver 192.0.2.1" ]

    mkdir $out
    cp apps version env $out/
  '')
