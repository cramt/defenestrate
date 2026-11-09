{
  description = "defenestrate: a declarative Windows distro built from the stock Microsoft ISO";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      inherit (pkgs) lib;

      windowsExe = src: mainProgram: import ./nix/shell.nix { inherit pkgs crane src mainProgram; };
      shell = windowsExe ./shell "defenestrate-shell.exe";

      # The Linux-side tool everything goes through, Nix builds included.
      dfnstrate = pkgs.callPackage ./nix/dfnstrate.nix { inherit crane shell; };

      # mkDistro [ ./my-machine.toml ] or [ ./my-machine.nix ]; see nix/distro.nix.
      mkDistro = import ./nix/distro.nix { inherit lib pkgs dfnstrate; };

      vm = mkDistro [ ./configs/vm.toml ];
      testLinuxDisk = import ./nix/tests/linux-disk.nix { inherit pkgs lib nixpkgs; };
    in
    {
      lib.${system} = { inherit mkDistro; };

      packages.${system} = {
        inherit shell dfnstrate;
        cosmic-bar-prototype = windowsExe ./prototypes/cosmic-bar "cosmic-bar.exe";
        vm-iso = vm.build.iso;
        vm = vm.build.vm;
        vm-image = vm.build.image;
        # A Linux install to attach to the VM, for wsl.linux in configs/vm.toml.
        test-linux-disk = testLinuxDisk;
        default = dfnstrate;
      };

      apps.${system} = {
        default = { type = "app"; program = lib.getExe dfnstrate; };
        vm = { type = "app"; program = lib.getExe vm.build.vm; };
      };

      checks.${system} = {
        # Renders the VM config, which fetches only fonts and Firefox, not the
        # ISO: catches config and generator breakage without a 25 GB build.
        vm-rendered = vm.build.rendered;
        dfnstrate-tests = dfnstrate.passthru.tests;
        # dfn against a Linux install, in a VM; no Windows needed.
        dfn = import ./nix/tests/dfn.nix { inherit pkgs; rendered = vm.build.rendered; linuxDisk = testLinuxDisk; };
      };

      # Same tools, firmware and shell the package wraps in, so `cargo run`
      # behaves like the installed dfnstrate.
      devShells.${system}.default = pkgs.mkShell (dfnstrate.passthru.env // {
        inputsFrom = [ dfnstrate.passthru.unwrapped ];
        packages = [ pkgs.clippy pkgs.rustfmt ] ++ dfnstrate.passthru.runtime;
        DEFENESTRATE_SHELL_EXE = "${shell}/bin/defenestrate-shell.exe";
      });
    };
}
