{ lib, runCommand, _7zz, wimlib, hivex, xorriso, libxml2, source, edition, bootPrompt, regFiles, unattend, shellPackage, fonts, apps }:

let
  efiBoot = if bootPrompt then "efisys.bin" else "efisys_noprompt.bin";

  hivePaths = {
    software = "/Windows/System32/config/SOFTWARE";
    system = "/Windows/System32/config/SYSTEM";
    defaultUser = "/Users/Default/NTUSER.DAT";
  };

  # Merge each .reg into a scratch copy of the hive it targets. The copies are
  # thrown away (specialize.cmd applies the real thing on Windows), this just
  # makes a malformed .reg fail the build instead of a VM install 20 minutes in.
  checkRegistry = lib.concatStrings (lib.mapAttrsToList (name: hive: ''
    hivexregedit --merge --prefix ${lib.escapeShellArg regFiles.roots.${hive}} \
      hives/${baseNameOf hivePaths.${hive}} ${regFiles}/ascii/${name}.reg
  '') regFiles.hives);
in
runCommand "defenestrate.iso"
  {
    nativeBuildInputs = [ _7zz wimlib hivex xorriso libxml2 ];
    # ~25 GB of scratch space and nothing a substituter would have.
    preferLocalBuild = true;
    allowSubstitutes = false;
  }
  ''
    # Setup stops on a malformed answer file before it writes anything,
    # which a headless build only notices hours later.
    xmllint --noout ${unattend}

    # Microsoft's ISO is UDF-only (the ISO9660 side is just a README), which
    # rules out xorriso for reading it.
    7zz x -oiso ${source} -x!sources/install.wim > /dev/null
    7zz x -owim ${source} sources/install.wim > /dev/null
    chmod -R u+w iso

    # Keeping one edition drops ~0.5 GB per extra edition and pins image index 1.
    wimlib-imagex export wim/sources/install.wim ${lib.escapeShellArg edition} install.wim
    rm -r wim

    mkdir hives
    wimlib-imagex extract install.wim 1 ${lib.concatStringsSep " " (builtins.attrValues hivePaths)} \
      --dest-dir=hives --no-acls > /dev/null
    ${checkRegistry}
    rm -r hives

    mkdir payload
    cp ${regFiles}/*.reg payload/
    sed 's/$/\r/' ${./specialize.cmd} > payload/specialize.cmd
    # Only adds new files. Replacing an existing file this way drops its
    # security descriptor, which is why the registry isn't edited offline.
    wimlib-imagex update install.wim 1 > /dev/null <<EOF
    add payload /Windows/Setup/defenestrate
    ${lib.optionalString (apps != null) "add ${apps} /Windows/Setup/defenestrate/apps"}
    ${lib.optionalString (shellPackage != null) "add ${shellPackage}/bin '/Program Files/defenestrate'"}
    ${lib.concatMapStrings (f: "add ${f} '/Windows/Fonts/${baseNameOf f}'\n") fonts}
    EOF

    # A single-edition install.wim is still ~7.5 GB, over ISO9660's 4 GiB
    # file limit. Setup takes split images natively, which keeps the ISO
    # plain ISO9660 and lets it be copied straight onto a FAT32 stick. (A
    # mkisofs UDF image held the whole file, and Windows setup stalled at 0%
    # without ever reading past its start.)
    wimlib-imagex split install.wim iso/sources/install.swm 3800 > /dev/null
    rm install.wim

    cp ${unattend} iso/autounattend.xml

    # Builds >= 26000 boot into the new "ConX" setup, which drops the
    # specialize and oobeSystem passes of an answer file. WinPE runs whatever
    # winpeshl.ini says instead of its default, so point it at legacy setup
    # with the answer file baked into boot.wim (image 2 is the setup image).
    # Lesson from nixconf's saturn-windows-image.sh.
    printf '[LaunchApps]\r\n%%SYSTEMDRIVE%%\\sources\\setup.exe, /legacy /unattend:%%SYSTEMDRIVE%%\\autounattend.xml\r\n' > winpeshl.ini
    wimlib-imagex update iso/sources/boot.wim 2 > /dev/null <<EOF
    add ${unattend} /autounattend.xml
    add winpeshl.ini /Windows/System32/winpeshl.ini
    EOF

    mkdir $out
    xorriso -as mkisofs -quiet -iso-level 3 -J -joliet-long \
      -V DEFENESTRATE_X64 \
      -c boot.catalog -hide boot.catalog -hide-joliet boot.catalog \
      -b boot/etfsboot.com -no-emul-boot -boot-load-size 8 \
      -eltorito-alt-boot -e efi/microsoft/boot/${efiBoot} -no-emul-boot \
      -o $out/defenestrate.iso iso
  ''
