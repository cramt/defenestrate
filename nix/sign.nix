# The shell's two binaries as they're installed: the launcher as built, and
# the uiAccess one Authenticode-signed by shell/sign.sh with a certificate
# made for this build. The private key is created, used and deleted inside
# this one derivation's build dir, so it never reaches the store; only the
# signed exe does, with the certificate inside its signature.
#
# Not reproducible by design: a rebuild makes a new key. The store path is
# still input-addressed, so it changes only when the shell does.
{ runCommand, openssl, osslsigncode, unsigned }:

runCommand "${unsigned.name}-signed"
  {
    nativeBuildInputs = [ openssl osslsigncode ];
    # What Winlogon starts (render.rs).
    meta.mainProgram = unsigned.meta.mainProgram;
  }
  ''
    bash ${../shell/sign.sh} ${unsigned}/bin $out/bin
  ''
