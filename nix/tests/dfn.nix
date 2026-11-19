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

    # wsl.conf's boot command, as the instance starts: the disks are
    # attached, so it mounts the install. The logon task's dfn mount runs at
    # the same moment (its wsl call is what started the instance); unlocked,
    # the second mount of /linux failed with EBUSY. Extra mounts make the
    # race likelier to show.
    dfn boot &
    pids=$!
    for _ in 1 2 3 4; do dfn mount & pids="$pids $!"; done
    for p in $pids; do wait $p || { echo "a concurrent dfn mount failed"; exit 1; }; done
    inside /bin/busybox grep -q ' /linux ' /proc/mounts || { inside /bin/busybox cat /tmp/dfn-boot.log; exit 1; }
    # WSL's second mount namespace (elevated or not, whichever came second):
    # a bare unshare(2) of the first, propagation untouched, as its init
    # makes it (busybox's --propagation unchanged still remounts, and fails).
    # What the logon task mounts in there has to show where \\wsl$ looks.
    # From inside the distro's root, as WSL's init is: unshare(2) moves the
    # root onto the new namespace's copy of it.
    python3 -c 'import os, sys, time; os.setns(os.open(f"/proc/{sys.argv[1]}/ns/mnt", os.O_RDONLY), os.CLONE_NEWNS); os.chroot(f"/proc/{sys.argv[1]}/root"); os.unshare(os.CLONE_NEWNS); time.sleep(100000)' $holder &
    second=$!
    for _ in $(seq 50); do [ -e /proc/$second/root/bin/busybox ] && break; sleep 0.2; done
    [ "$(readlink /proc/$second/ns/mnt)" != "$(readlink /proc/$holder/ns/mnt)" ]
    inside /bin/busybox mkdir -p /linux/tmp/probe
    nsenter -t $second -m -r -- /bin/busybox mount -t tmpfs probe /linux/tmp/probe
    inside /bin/busybox grep -q ' /linux/tmp/probe ' /proc/mounts || { echo "a mount in WSL's other namespace didn't propagate"; exit 1; }
    kill $second

    # Adding the user left the marker the logon task restarts the instance
    # on (\\wsl$ takes its user as the instance starts); the task removes it.
    inside /bin/busybox test -e /defenestrate/user-added || { echo "no user-added marker after the first mount"; exit 1; }
    inside /bin/busybox rm /defenestrate/user-added
    dfn mount
    dfn mount # again: idempotent, as every logon runs it
    # The Linux user, by the install's uid, once: wsl.conf's default user.
    [ "$(inside /bin/busybox grep -c '^user:x:1000:' /etc/passwd)" = 1 ]
    if inside /bin/busybox test -e /defenestrate/user-added; then echo "user-added again with the user already in passwd: every logon would restart"; exit 1; fi
    inside /bin/busybox grep -q '^default = user$' /etc/wsl.conf

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

    # What the logon task copies over: symlinks into the store resolved
    # inside the install (os-release is /etc/static/… -> /nix/store/…), and
    # a secret read as root.
    mkdir packed
    dfn pack /etc/os-release | tar -C packed -x
    [ -f packed/os-release ] && grep -q '^ID=nixos' packed/os-release
    dfn pack /var/lib/test-secret | tar -C packed -x
    [ "$(cat packed/test-secret/key)" = s3cret ]
    if dfn pack /no/such/path > /dev/null; then echo "pack of a missing path succeeded"; exit 1; fi

    dfn run firefox --version | tee version
    grep -q 'Mozilla Firefox' version

    # The session environment a desktop would give it.
    dfn run sh -c 'echo "$XDG_RUNTIME_DIR $WAYLAND_DISPLAY $PULSE_SERVER"; ls -l "$XDG_RUNTIME_DIR"' | tee env
    grep -q '/run/wslg/runtime-dir wayland-0 unix:/run/wslg/runtime-dir/pulse/native' env
    grep -q 'wayland-0' env
    [ "$(inside /bin/busybox cat /linux/etc/resolv.conf)" = "nameserver 192.0.2.1" ]

    mkdir $out
    cp version env $out/
  '')
