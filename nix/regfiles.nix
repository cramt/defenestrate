# One .reg file per entry of `files`, each a { hive, keys } pair: which hive
# its keys are relative to, and the registry config itself.
{ lib, runCommand, python3, writeText, files }:

let
  # Where each hive lives at install time. defaultUser is the Default
  # profile's NTUSER.DAT, which specialize.cmd loads under this name.
  roots = {
    software = "HKEY_LOCAL_MACHINE\\SOFTWARE";
    system = "HKEY_LOCAL_MACHINE\\SYSTEM";
    defaultUser = "HKEY_USERS\\DefenestrateDefault";
  };

  render = name: file: ''
    python3 -I ${../tools/regfile.py} \
      ${writeText "${name}.json" (builtins.toJSON file.keys)} \
      ${lib.escapeShellArg roots.${file.hive}} ascii/${name}.reg
    # reg.exe wants UTF-16LE with a BOM.
    python3 -I -c 'import sys; open(sys.argv[2], "wb").write(b"\xff\xfe" + open(sys.argv[1], "rb").read().decode("ascii").encode("utf-16-le"))' \
      ascii/${name}.reg ${name}.reg
  '';
in
runCommand "defenestrate-regfiles"
  {
    nativeBuildInputs = [ python3 ];
    passthru = {
      inherit roots;
      hives = lib.mapAttrs (_: file: file.hive) files;
    };
  }
  ''
    mkdir -p $out/ascii
    cd $out
    ${lib.concatStrings (lib.mapAttrsToList render files)}
  ''
