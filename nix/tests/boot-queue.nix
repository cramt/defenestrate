# The boot queue (cli/src/boot.rs) end to end, short of Windows:
#
# 1. In a VM (ntfs-3g needs root and FUSE): `dfnstrate update` onto an NTFS
#    volume shaped like an install from before the queue. It creates the
#    runner and its tasks' XML with the ACL C:\Windows\Setup\defenestrate
#    hands down (not ntfs-3g's Everyone: full control), queues the four
#    registry imports, queues nothing on a second run with the same config,
#    and with only the keyboard changed queues only the user import, in
#    place of the pending one.
# 2. Under pwsh on Linux, the runner update wrote, against that queue, with
#    the Windows calls faked (boot-fakes.ps1): each action once and in
#    order, a failure leaving it and what follows for the next boot without
#    redoing what finished, nothing on a second boot, the settings refreshes
#    after the user import, and refusals of a payload outside its phase's
#    root or not matching its hash.
{ pkgs, dfnstrate, cache, config }:

let
  update = pkgs.vmTools.runInLinuxVM (pkgs.runCommand "dfnstrate-boot-queue-update"
    {
      memSize = 1024;
      nativeBuildInputs = [ pkgs.ntfs3g pkgs.util-linux pkgs.coreutils pkgs.kmod pkgs.attr pkgs.gnugrep pkgs.gnused ];
    }
    ''
      set -x
      modprobe fuse || true
      truncate -s 256M v.img
      mkntfs -q -F -f v.img
      mkdir m
      ntfs-3g v.img m
      # C:\Windows\Setup\defenestrate as setup leaves it, but for the ACL:
      # SYSTEM full control, object and container inherit, owned by
      # Administrators (header, DACL with one ACE, owner, group).
      mkdir -p m/Windows/Setup/defenestrate
      system=010100000000000512000000
      admins=01020000000000052000000020020000
      header=0100048430000000400000000000000014000000
      dacl=02001c0001000000
      ace=00031400ff011f00$system
      setfattr -n system.ntfs_acl -v 0x$header$dacl$ace$admins$admins m/Windows/Setup/defenestrate
      umount m

      layer() {
        cat > layer.toml <<EOF
      shell = { command = "cmd.exe" }   # no certificate, so no SOFTWARE hive needed
      displayScale = 1.0
      [cosmic.compositor.input_default]
      state = "Enabled"
      acceleration = { profile = "Flat", speed = 0.6 }
      [cosmic.compositor.xkb_config]
      repeat_delay = $1
      repeat_rate = 25
      EOF
      }
      up() { ${pkgs.lib.getExe dfnstrate} update -c ${config} -c layer.toml --cache ${cache} --offline --volume-image v.img | tee out.txt; }

      layer 600
      up
      for t in "software registry import (000001-software)" "system registry import (000002-system)" "shell registry import (000003-shell)" "user registry import (000004-user)"; do
        grep -qF "queued the $t" out.txt
      done
      grep -qF 'created C:\Windows\Setup\defenestrate\boot\boot.ps1' out.txt

      ntfs-3g -o ro v.img m
      b=m/Windows/Setup/defenestrate/boot
      # Inherited (0x10) SYSTEM: full control, and no Everyone, on both a
      # file update made and one in a directory it made.
      for f in $b/boot.ps1 $b/machine/000001-software.json $b/user/000004-user.reg; do
        a=$(getfattr -e hex -n system.ntfs_acl $f | sed -n 's/^system.ntfs_acl=0x//p')
        case $a in *00101400ff011f00$system*) ;; *) echo "$f didn't inherit: $a"; exit 1 ;; esac
        case $a in *010100000000000100000000*) echo "$f grants Everyone: $a"; exit 1 ;; esac
      done
      umount m

      up
      [ "$(grep -c 'registry is as last queued' out.txt)" = 4 ] || exit 1
      ! grep -q 'queued the' out.txt

      layer 250
      up
      grep -qF 'queued the user registry import (000005-user)' out.txt
      [ "$(grep -c 'queued the' out.txt)" = 1 ] || exit 1

      ntfs-3g -o ro v.img m
      [ ! -e $b/user/000004-user.json ] || { echo "the replaced user import is still queued"; exit 1; }
      mkdir $out
      cp -r $b $out/boot
      umount m
    '');
in
pkgs.runCommand "dfnstrate-boot-queue-test" { nativeBuildInputs = [ pkgs.powershell pkgs.coreutils pkgs.gnugrep pkgs.perl pkgs.gnused ]; } ''
  set -x
  export HOME=$TMPDIR POWERSHELL_TELEMETRY_OPTOUT=1 ProgramData=$TMPDIR/pd FAKE_CALLS=$TMPDIR/calls
  cp -r ${update}/boot q
  chmod -R u+w q
  run() { : > $FAKE_CALLS; pwsh -NoProfile -NonInteractive -File q/boot.ps1 -Fakes ${./boot-fakes.ps1} "$@"; }
  calls() { cat $FAKE_CALLS | tr '\n' ';'; }
  ls -R q

  run -Register
  [ "$(calls)" = "register defenestrate-boot machine.xml;register defenestrate-boot-user user.xml;" ]

  # The machine phase dies on its second import: the first is done, the
  # failed one and the third wait.
  if FAKE_FAIL=000002 run -Phase Machine; then echo "a failed import didn't fail the run"; exit 1; fi
  [ "$(calls)" = "reg import 000001-software.reg;reg import 000002-system.reg;" ]
  [ -e q/machine/000001-software.done ] && [ ! -e q/machine/000002-system.done ] && [ ! -e q/machine/000003-shell.done ]
  # The next boot carries on from it, not from the start.
  run -Phase Machine
  [ "$(calls)" = "reg import 000002-system.reg;reg import 000003-shell.reg;" ]
  run -Phase Machine
  [ "$(calls)" = "" ]

  # The user import, then the refreshes for what it changed. The pointer has
  # a curve, which only loads at logon, so none of it is set now.
  run -Phase User
  [ "$(calls)" = "reg import 000005-user.reg;spi 0x17 7 ;spi 0xb 7 ;" ]
  grep -q 'Control Panel\\Desktop\\LogPixels has no refresh: in effect from the next logon' pd/defenestrate/boot-user.log
  grep -q 'Control Panel\\Mouse\\MouseSpeed has no refresh: in effect from the next logon' pd/defenestrate/boot-user.log
  run -Phase User
  [ "$(calls)" = "" ]
  grep -q 'nothing pending' pd/defenestrate/boot-user.log

  # A payload outside the phase's root: an HKLM key in the user queue, and
  # HKCU in the machine queue (SYSTEM's HKCU isn't the user's).
  queue() {
    # ASCII as UTF-16LE with a BOM, as dfnstrate writes it for reg.exe.
    perl -e 'print "\xff\xfe", map { "$_\0" } split //, $ARGV[0]' "$3" > q/$1/$2.reg
    sha=$(printf '%s' "$3" | sha256sum | cut -d' ' -f1)
    echo "$4" | sed "s/SHA/$sha/" > q/$1/$2.json
  }
  hdr=$'Windows Registry Editor Version 5.00\r\n\r\n'
  queue user 000006-user "$hdr"$'[HKEY_LOCAL_MACHINE\\SOFTWARE\\X]\r\n\r\n' '{"action":"regImport","sha256":"SHA","refresh":[],"atNextLogon":[]}'
  if run -Phase User; then echo "ran an HKLM key as the user"; exit 1; fi
  [ "$(calls)" = "" ]
  grep -q 'outside HKEY_CURRENT_USER' pd/defenestrate/boot-user.log
  rm q/user/000006-user.*
  queue machine 000007-software "$hdr"$'[HKEY_CURRENT_USER\\X]\r\n\r\n' '{"action":"regImport","target":"software","sha256":"SHA"}'
  if run -Phase Machine; then echo "ran HKCU as SYSTEM"; exit 1; fi
  [ "$(calls)" = "" ]
  rm q/machine/000007-software.*

  # A payload that isn't what the action hashed.
  queue user 000008-user "$hdr"$'[HKEY_CURRENT_USER\\X]\r\n\r\n' '{"action":"regImport","sha256":"SHA","refresh":[],"atNextLogon":[]}'
  printf 'x\0' >> q/user/000008-user.reg
  if run -Phase User; then echo "imported a .reg that doesn't match its hash"; exit 1; fi
  [ "$(calls)" = "" ]
  grep -q "the .reg's sha256 is" pd/defenestrate/boot-user.log

  cat pd/defenestrate/boot-machine.log pd/defenestrate/boot-user.log
  mkdir $out
''
