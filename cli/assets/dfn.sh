#!/bin/sh
# Inside the defenestrate WSL distro (busybox and this script, nothing else).
# Runs programs from the machine's Linux install: its disks are attached to
# WSL by the Windows logon task (`wsl --mount --bare`), mounted here like its
# own fileSystems, and the user's login shell runs chrooted in it as them,
# with the same home and tools as when Linux is booted. Terminal programs
# only: WSLg's windows never show under our shell (README).
#
#   dfn boot           wsl.conf's boot command: share mounts, then mount
#   dfn mount          mount the install at /linux (idempotent)
#   dfn release        unmount it, flushed, before Windows reboots
#   dfn pack <path>    a tar of the install's <path> on stdout, symlinks followed
#   dfn shell          the user's own login shell (the "Linux terminal")
#
# Two OSes never run at once on a dual boot machine, so sharing the home
# directory and its profiles is safe as long as Linux was shut down, not
# hibernated.
set -eu

ROOT=/linux
# _session runs inside the install, where the distro's files aren't.
[ "${1:-}" = _session ] || . /defenestrate/linux.conf # USER_NAME, and mounts(), a list of `m` calls

bb=/bin/busybox

is_mounted() { $bb grep -q " $1 " /proc/mounts; }

# m DEVICE FSTYPE OPTIONS TARGET
m() {
	dir=$ROOT${4%/}
	is_mounted "${dir:-/}" && return 0
	dev=$($bb findfs "$1") || {
		echo "dfn: no filesystem $1; is its disk attached (wsl --mount --bare)?" >&2
		return 1
	}
	# ext4 is built into WSL's kernel; btrfs and others are modules.
	$bb grep -qw "$2" /proc/filesystems || $bb modprobe "$2" || true
	$bb mkdir -p "$dir"
	$bb mount -t "$2" ${3:+-o "$3"} "$dev" "$dir"
}

bind() {
	is_mounted "$2" && return 0
	if [ -d "$1" ]; then $bb mkdir -p "$2"; else $bb touch "$2"; fi
	$bb mount --bind "$1" "$2"
}

# One at a time: the logon task's first wsl call starts the instance, whose
# boot command (dfn boot) then mounts at the same moment as the task's own
# dfn mount. Both saw /linux unmounted, and the loser's mount failed with
# EBUSY (saturn, 2026-11-17). The lock goes before anything long-lived (dfn
# shell) is exec'd with the descriptor still open.
do_mount() {
	exec 9>/tmp/dfn-mount.lock
	$bb flock 9
	mount_all
	exec 9>&-
}

mount_all() {
	mounts
	bind /proc "$ROOT/proc"
	bind /sys "$ROOT/sys"
	bind /dev "$ROOT/dev"
	bind /dev/pts "$ROOT/dev/pts"
	bind /dev/shm "$ROOT/dev/shm"

	# The install's /run is a tmpfs when it's booted; give it a fresh one with
	# what a booted system would have there.
	if ! is_mounted "$ROOT/run"; then
		$bb mount -t tmpfs -o mode=755 tmpfs "$ROOT/run"
		$bb ln -s "$(in_root /nix/var/nix/profiles/system)" "$ROOT/run/current-system"
		$bb mkdir -p "$ROOT/run/defenestrate"
		resolv
	fi
	# The Linux user, by the install's uid, so wsl.conf can make them the
	# distro's default user: what Windows writes over \\wsl$ is then theirs.
	# WSL picks \\wsl$'s user once, as the instance starts, so this instance
	# serves it as root until it's restarted (launches look the default user
	# up each time, so `id -u` can't tell). The marker is how the logon task
	# knows to restart it; on the rootfs, so a task that died before
	# restarting still does it at the next logon.
	if ! $bb grep -q "^$USER_NAME:" /etc/passwd; then
		echo "$USER_NAME:x:$(user_field 3):$(user_field 4)::/:/bin/sh" >>/etc/passwd
		$bb touch /defenestrate/user-added
	fi
	# What _session runs on inside the install. Copied every time so an
	# updated dfn takes effect without a remount.
	$bb cp $bb "$ROOT/run/defenestrate/busybox"
	$bb cp /defenestrate/dfn "$ROOT/run/defenestrate/dfn"
}

# Follows symlinks the way the install sees them: its profile links are
# absolute paths into its own store.
in_root() {
	p=$1
	while [ -L "$ROOT$p" ]; do
		t=$($bb readlink "$ROOT$p")
		case $t in
		/*) p=$t ;;
		*) p=$($bb dirname "$p")/$t ;;
		esac
	done
	echo "$p"
}

# The install's resolv.conf points at its own resolver, which isn't running.
# Use WSL's. NixOS often makes it a symlink into /run, which is ours now.
resolv() {
	conf=$ROOT/etc/resolv.conf
	if [ -L "$conf" ]; then
		target=$($bb readlink "$conf")
		case $target in
		/run/*)
			$bb mkdir -p "$ROOT$($bb dirname "$target")"
			$bb cp /etc/resolv.conf "$ROOT$target"
			return
			;;
		esac
	fi
	# A link elsewhere (into the store) can't be bound over from outside the
	# chroot; such an install keeps its own, which may still work. One with
	# none at all (never booted) gets an empty file to bind over, which its
	# own boot would replace anyway.
	[ -e "$conf" ] || [ -L "$conf" ] || $bb touch "$conf"
	[ -L "$conf" ] || $bb mount --bind /etc/resolv.conf "$conf" ||
		echo "dfn: couldn't give the install WSL's resolv.conf; DNS may not work" >&2
}

# Field N of the user's line in the install's passwd.
user_field() {
	$bb awk -F: -v u="$USER_NAME" -v n="$1" '$1 == u { print $n; found = 1 } END { exit !found }' "$ROOT/etc/passwd" ||
		{ echo "dfn: no user $USER_NAME in the install's /etc/passwd" >&2; exit 1; }
}

# as_user COMMAND: runs a bash -lc COMMAND (with "$@" after it) as the user,
# inside the install, with the environment a login session would give it.
#
# Not a plain chroot: the kernel refuses user namespaces to a chrooted
# process (EPERM from clone(CLONE_NEWUSER), "CanCreateUserNamespace()
# clone() failure"), and plenty of tools (Firefox-family sandboxes, bwrap,
# rootless containers) need them. In a private
# mount namespace the install is moved onto / and then chrooted into, which
# the kernel counts as the namespace's root: WSL's own init enters a distro
# this way (src/linux/init/main.cpp).
as_user() {
	exec $bb unshare -m /defenestrate/dfn _enter "$@"
}

# The second half of as_user, in its own mount namespace: switch root to
# the install the way WSL's init does.
enter() {
	uid=$(user_field 3)
	gid=$(user_field 4)
	home=$(user_field 6)
	$bb mount --make-rprivate /
	cd "$ROOT"
	$bb mount --move . /
	# Absolute paths still resolve from the old root until the chroot (the
	# kernel doesn't follow a mount on a lookup's starting point); the
	# working directory is the install, so go through it.
	exec ./run/defenestrate/busybox chroot . /run/defenestrate/busybox sh /run/defenestrate/dfn _session "$USER_NAME" "$uid" "$gid" "$home" "$@"
}

# Inside the install, as root: drop to the user and run COMMAND (with "$@"
# after it) through a login shell.
session() {
	user=$1 uid=$2 gid=$3 home=$4 cmd=$5
	shift 5
	bb=/run/defenestrate/busybox
	# What logind would make: the user's own runtime dir, on /run's tmpfs.
	$bb mkdir -p "/run/user/$uid"
	$bb chown "$uid:$gid" "/run/user/$uid"
	$bb chmod 700 "/run/user/$uid"
	# setpriv --init-groups gives the user their supplementary groups too
	# (secrets are often group-readable: onepassword-secrets for op-secrets);
	# busybox chpst only sets the primary one, so it's just the fallback.
	# Word-split on purpose: user names and gids are checked to be plain.
	if [ -x /run/current-system/sw/bin/setpriv ]; then
		drop="/run/current-system/sw/bin/setpriv --reuid=$user --regid=$gid --init-groups"
	else
		drop="$bb chpst -u $user"
	fi
	# shellcheck disable=SC2086
	exec $drop $bb env -i \
		HOME="$home" USER="$user" LOGNAME="$user" \
		XDG_RUNTIME_DIR="/run/user/$uid" \
		/run/current-system/sw/bin/bash -lc "cd ~; $cmd" dfn "$@"
}

case ${1:-} in
boot)
	# WSL gives elevated and non-elevated Windows clients separate mount
	# namespaces, the second unshared from the first the first time it's
	# needed (src/linux/init/config.cpp), and \\wsl$ is served from whichever
	# client started the instance. The logon task mounts as an elevated one,
	# so on a non-elevated start \\wsl$ showed an empty /linux. Shared before
	# that split, a mount made in either namespace shows in both.
	$bb mount --make-rshared /
	# An instance WSL stopped and started again mounts by itself, if the
	# disks are still attached. Its own process, so a missing disk stops it
	# at the first mount (set -e) instead of binding into an empty /linux.
	$bb sh /defenestrate/dfn mount >/tmp/dfn-boot.log 2>&1 || true
	;;
_enter)
	shift
	enter "$@"
	;;
_session)
	shift
	session "$@"
	;;
mount)
	do_mount
	;;
release)
	# Windows killing WSL's VM (a reboot) leaves the install's filesystems
	# mounted, and btrfs writes back every 30 s: whatever Windows wrote in
	# the last half minute was lost (saturn, 2026-11-20: a test report's
	# last items and a rename, seen done and then missing on Linux). So the
	# reboot task asks for this first. Programs still using the install
	# (a Linux terminal, say) get asked to quit, then made to.
	$bb sync
	is_mounted "$ROOT" || exit 0
	# Deepest first: binds and the install's own mounts under it.
	under() { $bb awk -v r="$ROOT" "$1"' && ($2 == r || index($2, r "/") == 1) { print length($2), $2 }' /proc/mounts | $bb sort -rn | $bb awk '{ print $2 }'; }
	mounts=$(under 1)
	# Who's using it: only the install's own filesystems, the ones on a
	# disk. Each btrfs subvolume is a device of its own to fuser -m, so
	# each is asked; the /proc, /dev and /sys binds are used by everything,
	# init included (asking those killed the test VM's).
	disks=$(under '$1 ~ /^\/dev\//')
	for m in $disks; do $bb fuser -m -k -TERM "$m" >/dev/null 2>&1 || true; done
	$bb sleep 2
	for m in $disks; do $bb fuser -m -k "$m" >/dev/null 2>&1 || true; done
	failed=
	for m in $mounts; do
		$bb umount "$m" || failed="$failed $m"
	done
	if [ -n "$failed" ]; then
		# Read-only still forces everything out to disk.
		for m in $failed; do $bb mount -o remount,ro "$m" 2>/dev/null || true; done
		echo "dfn: couldn't unmount$failed; remounted read-only" >&2
		exit 1
	fi
	;;
pack)
	[ $# -eq 2 ] || { echo "usage: dfn pack <path>" >&2; exit 2; }
	do_mount
	# A path the Windows config names but the install doesn't have: the
	# config is newer than the Linux generation last booted.
	$bb chroot "$ROOT" /run/defenestrate/busybox test -e "$2" ||
		{ echo "dfn: no $2 in the install; was Linux switched to the config that makes it before booting Windows?" >&2; exit 1; }
	# Chrooted, so absolute symlinks (home-manager's into the store) resolve
	# inside the install; -h follows them. As root: rendered secrets aren't
	# the user's to read.
	exec $bb chroot "$ROOT" /run/defenestrate/busybox tar -ch -C "$($bb dirname "$2")" "$($bb basename "$2")"
	;;
shell)
	do_mount
	as_user 'exec "$1" -l' "$(user_field 7)"
	;;
*)
	echo "usage: dfn boot | mount | release | pack <path> | shell" >&2
	exit 2
	;;
esac
