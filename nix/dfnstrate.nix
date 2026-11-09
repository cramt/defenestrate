# dfnstrate with everything it drives on PATH, the Secure Boot firmware and
# a Chromium for `fetch`, and the shell compiled in. This is what the
# derivations in distro.nix run, and what `nix run .#dfnstrate` gives you.
{ lib, pkgs, crane, shell, makeWrapper
, _7zz, wimlib, hivex, xorriso, qemu_kvm, swtpm, util-linux, mtools, sleuthkit, ntfs3g, efibootmgr, systemd, chromium, OVMFFull
}:

let
  craneLib = crane.mkLib pkgs;
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../cli ../configs/default.toml ];
  };
  common = {
    inherit src;
    inherit (craneLib.crateNameFromCargoToml { cargoToml = ../cli/Cargo.toml; }) pname version;
    strictDeps = true;
    cargoExtraArgs = "-p dfnstrate";
    # headless_chrome's build script runs rustfmt over the code it generates,
    # purely cosmetically, and fails without one.
    DO_NOT_FORMAT = "1";
  };
  cargoArtifacts = craneLib.buildDepsOnly common;
  unwrapped = craneLib.buildPackage (common // {
    inherit cargoArtifacts;
    # Tests run as their own check (passthru.tests), not on every build.
    doCheck = false;
    DEFENESTRATE_SHELL_EXE = "${shell}/bin/${shell.meta.mainProgram}";
  });

  runtime = [ _7zz wimlib hivex xorriso qemu_kvm swtpm util-linux mtools sleuthkit ntfs3g efibootmgr systemd ];
  # Defaults only: the caller's own firmware or browser still wins.
  env = {
    DFNSTRATE_OVMF_CODE = "${OVMFFull.fd}/FV/OVMF_CODE.ms.fd";
    DFNSTRATE_OVMF_VARS = "${OVMFFull.fd}/FV/OVMF_VARS.ms.fd";
    DFNSTRATE_BROWSER = lib.getExe chromium;
  };
in
pkgs.runCommand "dfnstrate-${unwrapped.version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    meta.mainProgram = "dfnstrate";
    passthru = {
      inherit unwrapped runtime env;
      tests = craneLib.cargoTest (common // { inherit cargoArtifacts; });
    };
  }
  ''
    # Prefixed, so sudo and the rest of the caller's PATH still resolve.
    makeWrapper ${unwrapped}/bin/dfnstrate $out/bin/dfnstrate \
      --prefix PATH : ${lib.makeBinPath runtime} \
      ${lib.concatStringsSep " " (lib.mapAttrsToList (k: v: "--set-default ${k} ${v}") env)}
  ''
