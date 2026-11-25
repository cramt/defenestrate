# The shell: cross-compiled to a Windows executable (exe), and its tests run
# natively (tests). Off Windows the crate compiles only its pure modules
# (tiling tree, classification, settings), which is what the tests cover.
{ pkgs, crane, src, mainProgram }:

let
  toolchain = pkgs.rust-bin.fromRustupToolchainFile "${src}/rust-toolchain.toml";
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
  mingw = pkgs.pkgsCross.mingwW64;
  common = {
    src = craneLib.cleanCargoSource src;
    strictDeps = true;
  };
in
{
  exe = craneLib.buildPackage (common // {
    # Run natively by `tests` below; the Windows build would need Wine.
    doCheck = false;

    CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc}/bin/${mingw.stdenv.cc.targetPrefix}cc";
    # The mingw gcc wants libpthread for Rust's std even though std itself
    # doesn't use pthreads.
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${mingw.windows.pthreads}/lib";
    depsBuildBuild = [ mingw.stdenv.cc ];

    meta = { inherit mainProgram; };
  });

  tests = craneLib.cargoTest (common // {
    cargoArtifacts = craneLib.buildDepsOnly common;
    # settings.rs checks it reads what dfnstrate renders: the golden file,
    # which lives beside the crate, not in it.
    preBuild = ''
      mkdir -p ../configs
      cp ${../configs/shell-settings.json} ../configs/shell-settings.json
    '';
  });
}
