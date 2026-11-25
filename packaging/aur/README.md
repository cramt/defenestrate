# AUR packaging

`dfnstrate-git/` is the AUR package: PKGBUILD plus the `.SRCINFO` the AUR
reads. It builds the latest `main` from source: the Windows shell with
rustup's `x86_64-pc-windows-gnu` target and `mingw-w64-gcc`, then dfnstrate
with the shell compiled in.

## Testing it

`check.sh` builds and installs the package in a throwaway
`archlinux:base-devel` container (rootless podman), then runs
`dfnstrate --help` and `dfnstrate render` on `configs/vm.toml`, plus
namcap:

```sh
packaging/aur/check.sh
```

It builds what's pushed to GitHub, not the working tree, since that's what
the AUR package will build.

After changing the PKGBUILD, regenerate `.SRCINFO` (the script prints it):

```sh
makepkg --printsrcinfo > .SRCINFO   # on Arch, in dfnstrate-git/
```

## Publishing

Once, with an AUR account that has your SSH key:

```sh
git clone ssh://aur@aur.archlinux.org/dfnstrate-git.git /tmp/aur-dfnstrate-git
cp packaging/aur/dfnstrate-git/{PKGBUILD,.SRCINFO} /tmp/aur-dfnstrate-git/
cd /tmp/aur-dfnstrate-git && git add PKGBUILD .SRCINFO && git commit -m "Initial import" && git push
```

Later updates are the same copy, commit and push. A `-git` package only
needs that when the PKGBUILD changes; new commits on `main` get picked up
when users rebuild.

## A tagged release package

Version tags exist (`v0.1.0`, matching `cli/Cargo.toml`). A `dfnstrate`
(non-git) package next to this one would need:

- A PKGBUILD whose `source` is the tag's tarball,
  `$url/archive/refs/tags/v$pkgver.tar.gz`, with a real `sha256sums`
  (`updpkgsums`) in place of `SKIP`, no `pkgver()`, and no
  `provides`/`conflicts` dance beyond conflicting with `dfnstrate-git`.
- A bump per release: new `pkgver`, `pkgrel=1`, `updpkgsums`, regenerate
  `.SRCINFO`, push to the AUR. A GitHub Action could do this on tag push,
  but that needs an AUR SSH key in the repo's secrets.

`dfnstrate-git`'s version starts with the Cargo version
(`0.1.0.r<commits>.g<hash>`), so it sorts above the matching release.
