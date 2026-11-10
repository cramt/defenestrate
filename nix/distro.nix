# mkDistro: configs in, derivations that run dfnstrate out. The building is
# all dfnstrate's; this file only does what Nix is for: turn configs into
# dfnstrate's input, and fetch every file it needs as a fixed-output
# derivation, so the build itself runs offline in the sandbox.
#
# Each entry of `modules` is a .toml file (same format `dfnstrate -c` takes)
# or a Nix module setting the same keys. TOML files are layered in order,
# then the Nix modules, merged together, as one last layer. All of it goes
# over configs/default.toml, which dfnstrate applies itself.
{ lib, pkgs, dfnstrate }:

modules:

let
  isToml = m: (builtins.isPath m || builtins.isString m) && lib.hasSuffix ".toml" (toString m);

  # Paths in a TOML file are relative to it. In the store it's alone, so
  # they become store paths here.
  resolveDeep = dir: v:
    if builtins.isAttrs v then lib.mapAttrs (k: x:
      if k == "path" && builtins.isString x && !lib.hasPrefix "/" x then dir + "/${x}"
      else resolveDeep dir x) v
    else if builtins.isList v then map (resolveDeep dir) v
    else v;
  tomlLayer = file: resolveDeep (dirOf file) (lib.importTOML file);

  nixModules = lib.filter (m: !isToml m) modules;
  nixLayer = (lib.evalModules {
    modules = [{ freeformType = (pkgs.formats.json { }).type; }] ++ nixModules;
  }).config;

  layers = map tomlLayer (lib.filter isToml modules) ++ lib.optional (nixModules != [ ]) (removeAttrs nixLayer [ "_module" ]);
  layerFiles = lib.imap0 (i: l: pkgs.writeText "dfnstrate-layer-${toString i}.json" (builtins.toJSON l)) layers;
  configArgs = lib.concatMapStringsSep " " (f: "-c ${f}") layerFiles;

  # Only for finding what to fetch; dfnstrate does the real merge.
  defaults = lib.importTOML ../configs/default.toml;
  all = [ defaults ] ++ layers;

  # Every { url, hash } anywhere in the config is a file to fetch.
  collect = v:
    if builtins.isAttrs v then
      (if v ? url && v ? hash then [{ inherit (v) url hash; }] else [ ]) ++ lib.concatMap collect (builtins.attrValues v)
    else if builtins.isList v then lib.concatMap collect v
    else [ ];


  iso = lib.foldl (acc: l: acc // (l.iso or { })) { } all;
  # Same name and hash as `nix-store --add-fixed sha256 <iso>`, so an ISO
  # added that way is used as is, without fetching.
  isoSource = pkgs.runCommand iso.name
    {
      nativeBuildInputs = [ dfnstrate ];
      outputHash = iso.hash;
      outputHashMode = "flat";
      pin = builtins.toJSON { inherit iso; };
      passAsFile = [ "pin" ];
      preferLocalBuild = true;
      # Chromium aborts at startup when fontconfig finds no fonts at all,
      # which is the sandbox's normal state.
      FONTCONFIG_FILE = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };
    }
    ''
      export HOME=$TMPDIR
      mv "$pinPath" pin.json
      dfnstrate fetch --no-defaults -c pin.json --cache "$TMPDIR/cache" --out "$out"
    '';

  cacheKey = hash: lib.replaceStrings [ "/" "+" ] [ "_" "-" ] hash;
  fetched = lib.listToAttrs (map (s: lib.nameValuePair (cacheKey s.hash) (pkgs.fetchurl s)) (collect all));
  cache = pkgs.linkFarm "dfnstrate-cache" (fetched // { ${cacheKey iso.hash} = isoSource; });
  # For render and update, which never read the ISO: fetching it takes a
  # browser and 7 GB.
  isoFreeCache = pkgs.linkFarm "dfnstrate-cache" fetched;

  runWith = c: args: "dfnstrate ${args} ${configArgs} --cache ${c} --offline";
  run = runWith cache;

  # ~25 GB of scratch space and nothing a substituter would have.
  heavy = {
    nativeBuildInputs = [ dfnstrate ];
    preferLocalBuild = true;
    allowSubstitutes = false;
  };

  local = {
    preferLocalBuild = true;
    allowSubstitutes = false;
  };

  mkIso = forImage: pkgs.runCommand "defenestrate.iso" heavy ''
    ${run "build iso${lib.optionalString forImage " --for-image"}"} --out $out --work-dir work
  '';
  isoBuild = mkIso false;

  image = pkgs.runCommand "defenestrate-image" (heavy // { requiredSystemFeatures = [ "kvm" ]; }) ''
    ${run "build image --from-iso ${mkIso true}"} --out $out --work-dir work
  '';
in
{
  build = {
    iso = isoBuild;
    inherit image cache isoSource;
    # For looking at what the config generates: `nix build .#…build.rendered`.
    rendered = pkgs.runCommand "defenestrate-rendered" { nativeBuildInputs = [ dfnstrate ]; } (runWith isoFreeCache "render" + " --out $out");
    # These scripts are tiny but reference the ISO, the image or the cache,
    # which makes those inputs: built on a remote builder, the 30 GB image
    # would be copied there for a two-line wrapper.
    vm = pkgs.writeShellApplication {
      derivationArgs = local;
      name = "defenestrate-vm";
      text = ''exec ${lib.getExe dfnstrate} vm --iso ${isoBuild}/defenestrate.iso "$@"'';
    };
    install = pkgs.writeShellApplication {
      derivationArgs = local;
      name = "defenestrate-install";
      text = ''exec ${lib.getExe dfnstrate} flash --image ${image} ${configArgs} "$@"'';
    };
    update = pkgs.writeShellApplication {
      derivationArgs = local;
      name = "defenestrate-update";
      text = ''exec ${lib.getExe dfnstrate} update ${configArgs} --cache ${isoFreeCache} --offline "$@"'';
    };
  };
}
