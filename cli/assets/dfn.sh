#!/bin/sh
# Inside the defenestrate WSL distro (busybox and this script, nothing else).
# Runs programs from the machine's Linux install: its disks are attached to
# WSL by the Windows logon task (`wsl --mount --bare`), mounted here like its
# own fileSystems, and programs run chrooted in it as its user, so Firefox is
# the same binary with the same profile as when Linux is booted.
#
#   dfn mount          mount the install at /linux (idempotent)
#   dfn apps           list its desktop entries: <id> TAB <name>
#   dfn launch <id>    run a desktop entry
#   dfn run <cmd>...   run a command as the user, through a login shell
#
# Two OSes never run at once on a dual boot machine, so sharing the home
# directory and its profiles is safe as long as Linux was shut down, not
# hibernated.
set -eu

ROOT=/linux
. /defenestrate/linux.conf # USER_NAME, and mounts(), a list of `m` calls

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

do_mount() {
	mounts
	bind /proc "$ROOT/proc"
	bind /sys "$ROOT/sys"
	bind /dev "$ROOT/dev"
	bind /dev/pts "$ROOT/dev/pts"
	bind /dev/shm "$ROOT/dev/shm"
	# X11's socket; a booted install makes this directory itself.
	bind /tmp/.X11-unix "$ROOT/tmp/.X11-unix"

	# The install's /run is a tmpfs when it's booted; give it a fresh one with
	# what a booted system would have there.
	if ! is_mounted "$ROOT/run"; then
		$bb mount -t tmpfs -o mode=755 tmpfs "$ROOT/run"
		$bb ln -s "$(in_root /nix/var/nix/profiles/system)" "$ROOT/run/current-system"
		# WSLg's Wayland and PulseAudio sockets, here rather than at
		# /mnt/wslg so nothing is created on the install's own disk.
		bind /mnt/wslg "$ROOT/run/wslg"
		$bb mkdir -p "$ROOT/run/defenestrate"
		# chpst for dropping to the user once inside the chroot.
		$bb cp $bb "$ROOT/run/defenestrate/busybox"
		resolv
	fi
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

# WSLg starts with WSL's VM, which stops a minute after the last distro
# does, so a launch often comes before its compositor is up.
wait_for_wslg() {
	i=0
	while [ ! -S /mnt/wslg/runtime-dir/wayland-0 ] && [ $i -lt 60 ]; do
		$bb sleep 0.5
		i=$((i + 1))
	done
}

# as_user COMMAND: runs a bash -lc COMMAND (with "$@" after it) as the user,
# chrooted, with the environment a desktop session would give it. The
# runtime dir is WSLg's own (world-writable, its sockets made by its uid
# 1000, which is also the usual first Linux user).
as_user() {
	cmd=$1
	shift
	home=$(user_field 6)
	exec $bb chroot "$ROOT" /run/defenestrate/busybox chpst -u "$USER_NAME" \
		/run/defenestrate/busybox env -i \
		HOME="$home" USER="$USER_NAME" LOGNAME="$USER_NAME" \
		XDG_RUNTIME_DIR=/run/wslg/runtime-dir WAYLAND_DISPLAY=wayland-0 DISPLAY=:0 \
		PULSE_SERVER=unix:/run/wslg/runtime-dir/pulse/native \
		/run/current-system/sw/bin/bash -lc "cd ~; $cmd" dfn "$@"
}

# Desktop entries the user would see in a launcher: everything on
# XDG_DATA_DIRS (which the login shell sets from the profiles), first one of
# each id wins, minus hidden ones and terminal programs.
APPS='
for dir in ${XDG_DATA_HOME:-$HOME/.local/share} $(echo "${XDG_DATA_DIRS:-}" | tr : " "); do
	for f in "$dir"/applications/*.desktop; do
		[ -f "$f" ] && echo "$(basename "$f")	$f"
	done
done | awk -F "\t" '"'"'!seen[$1]++ { print $2 }'"'"' | while read -r f; do
	awk -F= -v f="$f" '"'"'
		/^\[/ { main = ($0 == "[Desktop Entry]") }
		main && $1 == "Name" && name == "" { name = substr($0, 6) }
		main && $1 == "Type" { type = $2 }
		main && ($1 == "NoDisplay" || $1 == "Hidden") && $2 == "true" { hide = 1 }
		main && $1 == "Terminal" && $2 == "true" { hide = 1 }
		END { if (type == "Application" && !hide && name != "") { n = split(f, p, "/"); print p[n] "\t" name } }
	'"'"' "$f"
done
'

# Exec= of desktop entry $1, field codes (%u, %F, …) dropped, run through the
# shell, which takes care of its quoting.
LAUNCH='
id=$1
for dir in ${XDG_DATA_HOME:-$HOME/.local/share} $(echo "${XDG_DATA_DIRS:-}" | tr : " "); do
	f=$dir/applications/$id
	[ -f "$f" ] && break
done
[ -f "$f" ] || { echo "dfn: no desktop entry $id" >&2; exit 1; }
exec=$(awk -F= '"'"'/^\[/ { main = ($0 == "[Desktop Entry]") } main && $1 == "Exec" { print substr($0, 6); exit }'"'"' "$f" | sed "s/ *%[fFuUdDnNickvm]//g")
eval "exec $exec"
'

case ${1:-} in
mount)
	do_mount
	;;
apps)
	do_mount
	as_user "$APPS"
	;;
launch)
	[ $# -eq 2 ] || { echo "usage: dfn launch <desktop id>" >&2; exit 2; }
	do_mount
	wait_for_wslg
	as_user "$LAUNCH" "$2"
	;;
run)
	shift
	[ $# -ge 1 ] || { echo "usage: dfn run <command> [args]" >&2; exit 2; }
	do_mount
	wait_for_wslg
	as_user 'exec "$@"' "$@"
	;;
*)
	echo "usage: dfn mount | apps | launch <desktop id> | run <command> [args]" >&2
	exit 2
	;;
esac
