{ lib, runCommand, python3, writeText, registry }:

let
  # Where each hive lives at install time. defaultUser is the Default
  # profile's NTUSER.DAT, which specialize.cmd loads under this name.
  roots = {
    software = "HKEY_LOCAL_MACHINE\\SOFTWARE";
    system = "HKEY_LOCAL_MACHINE\\SYSTEM";
    defaultUser = "HKEY_USERS\\DefenestrateDefault";
  };

  render = hive: ''
    python3 -I ${../tools/regfile.py} \
      ${writeText "${hive}.json" (builtins.toJSON registry.${hive})} \
      ${lib.escapeShellArg roots.${hive}} ascii/${hive}.reg
    # reg.exe wants UTF-16LE with a BOM.
    python3 -I -c 'import sys; open(sys.argv[2], "wb").write(b"\xff\xfe" + open(sys.argv[1], "rb").read().decode("ascii").encode("utf-16-le"))' \
      ascii/${hive}.reg ${hive}.reg
  '';
in
runCommand "defenestrate-regfiles"
  {
    nativeBuildInputs = [ python3 ];
    passthru = { inherit roots; };
  }
  ''
    mkdir -p $out/ascii
    cd $out
    ${lib.concatMapStrings render (builtins.attrNames roots)}
  ''
