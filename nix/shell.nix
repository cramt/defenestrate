# Cross-compiles a Rust crate to a Windows executable.
{ pkgs, crane, src, mainProgram }:

let
  toolchain = pkgs.rust-bin.fromRustupToolchainFile "${src}/rust-toolchain.toml";
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
  mingw = pkgs.pkgsCross.mingwW64;
in
craneLib.buildPackage {
  src = craneLib.cleanCargoSource src;
  strictDeps = true;
  # Tests would have to run on Windows.
  doCheck = false;

  CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw.stdenv.cc}/bin/${mingw.stdenv.cc.targetPrefix}cc";
  # The mingw gcc wants libpthread for Rust's std even though std itself
  # doesn't use pthreads.
  CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${mingw.windows.pthreads}/lib";
  depsBuildBuild = [ mingw.stdenv.cc ];

  meta = { inherit mainProgram; };
}
