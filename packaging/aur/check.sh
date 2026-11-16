#!/usr/bin/env bash
# Builds dfnstrate-git in a clean Arch container and smoke-tests it: what an
# AUR user's `makepkg -si` does, minus their machine's leftovers. Rootless
# podman, so it runs on hosts without Arch or root.
#
# Builds this checkout's HEAD (committed, not the working tree) instead of
# GitHub's main, so a change can be tested before it's pushed.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
podman=${PODMAN:-podman}
command -v "$podman" >/dev/null || podman="nix run nixpkgs#podman --"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# A bundle, because a worktree's .git is only a pointer into the main
# checkout and can't be cloned from inside the container. --branches
# brings the history pkgver() counts.
git -C "$repo" bundle create "$tmp/src.bundle" HEAD --branches 2>/dev/null

$podman run --rm -i \
  -v "$here/dfnstrate-git:/pkg:ro" \
  -v "$repo/configs:/configs:ro" \
  -v "$tmp/src.bundle:/src.bundle:ro" \
  docker.io/archlinux/archlinux:base-devel bash -euo pipefail <<'EOF'
pacman -Syu --noconfirm --needed git namcap >/dev/null
# makepkg refuses to run as root.
useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
install -d -o builder /build
# osslsigncode, a makedepend, is only in the AUR, so pacman can't resolve
# it; build it first as an AUR helper (yay, paru) would.
su builder -c 'cd ~ && git clone -q https://aur.archlinux.org/osslsigncode.git && cd osslsigncode && makepkg -si --noconfirm' >/dev/null
# makepkg's git clone --mirror can't read a bundle directly.
git clone -q --bare /src.bundle /src.git
chown -R builder /src.git
sed 's|^source=.*|source=("$_pkgname::git+file:///src.git")|' /pkg/PKGBUILD > /build/PKGBUILD
# Writable by makepkg, so pkgver() can record the version it computed.
chown builder /build/PKGBUILD
cd /build

su builder -c 'makepkg -s --noconfirm'

pacman -U --noconfirm /build/dfnstrate-git-*.pkg.tar.zst
echo '--- namcap'
namcap /pkg/PKGBUILD /build/dfnstrate-git-*.pkg.tar.zst || true

echo '--- .SRCINFO'
# The real source URL, with the pkgver this build computed.
sed "s|^pkgver=.*|$(grep '^pkgver=' /build/PKGBUILD)|" /pkg/PKGBUILD > /build/upstream.PKGBUILD
su builder -c 'makepkg --printsrcinfo -p upstream.PKGBUILD' > /build/.SRCINFO
cat /build/.SRCINFO
diff -u /pkg/.SRCINFO /build/.SRCINFO && echo '.SRCINFO up to date' || echo '.SRCINFO STALE: replace it with the above'

echo '--- dfnstrate --help'
dfnstrate --help
echo '--- dfnstrate render'
# Fails with "built without the shell" unless the Windows exe made it in:
# configs default to shell = "builtin".
su builder -c 'cd ~ && dfnstrate render -c /configs/vm.toml -o render-out'
find ~builder/render-out -maxdepth 2 | sort
EOF
