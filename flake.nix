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
      # requireFile marks the ISO unfree, which, fair. Allow exactly that.
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        config.allowUnfreePredicate = p: nixpkgs.lib.hasPrefix "Windows11_" (nixpkgs.lib.getName p);
      };

      mkDistro = modules:
        (nixpkgs.lib.evalModules {
          modules = [
            ./nix/module.nix
            {
              # `packages` is what configs/ reach for: things this flake builds.
              _module.args = { inherit pkgs; packages = { inherit shell; }; };
              source = pkgs.callPackage ./nix/source.nix { };
            }
          ] ++ modules;
        }).config;

      windowsExe = src: mainProgram: import ./nix/shell.nix { inherit pkgs crane src mainProgram; };
      shell = windowsExe ./shell "defenestrate-shell.exe";

      vm = mkDistro [ ./configs/default.nix ./configs/vm.nix ];
    in
    {
      lib.${system} = { inherit mkDistro; };

      packages.${system} = {
        inherit shell;
        cosmic-bar-prototype = windowsExe ./prototypes/cosmic-bar "cosmic-bar.exe";
        vm-iso = vm.build.iso;
        vm = vm.build.vm;
        default = vm.build.iso;
      };

      apps.${system}.vm = {
        type = "app";
        program = "${vm.build.vm}/bin/defenestrate-vm";
      };
    };
}
