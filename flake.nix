{
  description = "defenestrate: a declarative Windows distro built from the stock Microsoft ISO";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      # requireFile marks the ISO unfree, which, fair. Allow exactly that.
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfreePredicate = p: nixpkgs.lib.hasPrefix "Windows11_" (nixpkgs.lib.getName p);
      };

      mkDistro = modules:
        (nixpkgs.lib.evalModules {
          modules = [
            ./nix/module.nix
            {
              _module.args = { inherit pkgs; };
              source = pkgs.callPackage ./nix/source.nix { };
            }
          ] ++ modules;
        }).config;

      vm = mkDistro [ ./configs/default.nix ./configs/vm.nix ];
    in
    {
      lib.${system} = { inherit mkDistro; };

      packages.${system} = {
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
