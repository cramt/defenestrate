#!/usr/bin/env bash
# sign.sh IN_DIR OUT_DIR: takes the shell's two binaries from IN_DIR (cargo's
# release dir) to OUT_DIR: defenestrate-shell.exe, the launcher Winlogon
# starts, as it is, and defenestrate-shell-uiaccess.exe Authenticode-signed
# with a code signing certificate made for this one build, after which the
# private key is destroyed.
#
# The uiAccess binary's manifest asks for uiAccess (build.rs), which Windows
# only grants to an exe signed by a certificate chaining to a root in the
# machine's Trusted Root store, installed under Program Files:
# https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-only-elevate-uiaccess-applications-that-are-installed-in-secure-locations
# dfnstrate reads the certificate back out of the signature and puts it in
# that store (cli/src/cert.rs). With the key gone, nothing exists that can
# sign anything else that Windows would then trust.
#
# Needs openssl and osslsigncode. The Nix build runs this (nix/sign.nix), and
# so do the builds without it (README, the AUR package).
set -euo pipefail
in_dir=$1 out_dir=$2
launcher=defenestrate-shell.exe uiaccess=defenestrate-shell-uiaccess.exe
for f in "$launcher" "$uiaccess"; do
  [ -f "$in_dir/$f" ] || { echo "sign.sh: no $f in $in_dir" >&2; exit 1; }
done

key_dir=$(mktemp -d)
# Even on failure: a kept-failed Nix build dir or a stray /tmp must not end
# up holding the key.
trap 'rm -rf "$key_dir"' EXIT
umask 077

# The subject is how dfnstrate tells our certificates apart from everyone
# else's in the Root store (cli/src/cert.rs: COMMON_NAME), so it's fixed.
# - A leaf, not a CA (CA:FALSE), and good for code signing only: if the key
#   had leaked anyway, it couldn't issue certificates or sign TLS. Windows
#   takes a self-signed leaf in Root as its own trust anchor (seen in the VM:
#   Get-AuthenticodeSignature says Valid, and the shell gets uiAccess).
# - Valid until 9999-12-31 (RFC 5280's "no expiry"), and from 2000: there's
#   no timestamp, so Windows checks the dates against its own clock, which
#   on a dual boot machine may sit hours off the build machine's.
openssl req -x509 -new -noenc -quiet \
  -newkey rsa:3072 \
  -keyout "$key_dir/key.pem" -out "$key_dir/cert.pem" \
  -subj "/CN=defenestrate shell" \
  -not_before 20000101000000Z -not_after 99991231235959Z \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" \
  -addext "subjectKeyIdentifier=hash"

umask 022
mkdir -p "$out_dir"
rm -f "$out_dir/$launcher" "$out_dir/$uiaccess"
install -m 755 "$in_dir/$launcher" "$out_dir/$launcher"
osslsigncode sign -h sha256 -certs "$key_dir/cert.pem" -key "$key_dir/key.pem" -n "defenestrate shell" \
  -in "$in_dir/$uiaccess" -out "$out_dir/$uiaccess" >/dev/null
chmod 755 "$out_dir/$uiaccess"
osslsigncode verify -CAfile "$key_dir/cert.pem" -in "$out_dir/$uiaccess" >/dev/null
