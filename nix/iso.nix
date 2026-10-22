{ lib, runCommand, _7zz, wimlib, hivex, cdrtools, source, edition, bootPrompt, regFiles, unattend }:

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
  checkRegistry = lib.concatStrings (lib.mapAttrsToList (hive: path: ''
    hivexregedit --merge --prefix ${lib.escapeShellArg regFiles.roots.${hive}} \
      hives/${baseNameOf path} ${regFiles}/ascii/${hive}.reg
  '') hivePaths);
in
runCommand "defenestrate.iso"
  {
    nativeBuildInputs = [ _7zz wimlib hivex cdrtools ];
    # ~25 GB of scratch space and nothing a substituter would have.
    preferLocalBuild = true;
    allowSubstitutes = false;
  }
  ''
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
    wimlib-imagex update install.wim 1 --command="add payload /Windows/Setup/defenestrate" > /dev/null
    mv install.wim iso/sources/install.wim

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

    # A single-edition install.wim is still ~7.5 GB. ISO9660 can't hold a
    # file over 4 GiB, so the image needs UDF, which xorriso can't write and
    # cdrtools' mkisofs can.
    mkdir $out
    mkisofs -quiet -iso-level 3 -UDF -J -joliet-long \
      -V DEFENESTRATE_X64 \
      -c boot.catalog -hide boot.catalog -hide-joliet boot.catalog -hide-udf boot.catalog \
      -b boot/etfsboot.com -no-emul-boot -boot-load-size 8 \
      -eltorito-alt-boot -eltorito-platform efi -no-emul-boot \
      -b efi/microsoft/boot/${efiBoot} \
      -o $out/defenestrate.iso iso
  ''
