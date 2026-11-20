# dfnstrate logs against an NTFS volume shaped like an install's, in a VM
# (ntfs-3g needs root and FUSE): what it copies, who owns the copies when
# sudo ran it, that it writes nothing to the volume, and that --also can't
# reach outside C:\.
{ pkgs, dfnstrate }:

pkgs.vmTools.runInLinuxVM (pkgs.runCommand "dfnstrate-logs-test"
  {
    memSize = 1024;
    nativeBuildInputs = [ pkgs.ntfs3g pkgs.util-linux pkgs.coreutils pkgs.kmod ];
  }
  ''
    set -x
    modprobe fuse || true
    truncate -s 128M v.img
    mkntfs -q -F -f v.img
    mkdir m
    ntfs-3g v.img m
    u=m/Users/user
    mkdir -p $u/AppData/Local/defenestrate/winget m/ProgramData/defenestrate m/Windows/Setup/defenestrate $u/.claude/projects/C--Users-user
    echo shell > $u/AppData/Local/defenestrate/shell.log
    echo winget > $u/AppData/Local/defenestrate/winget/Discord.Discord.log
    echo first > m/ProgramData/defenestrate/linux-first.log
    echo updates > m/Windows/Setup/defenestrate/updates.log
    truncate -s 60M m/Windows/Setup/defenestrate/big.vhdx
    echo '{}' > $u/.claude/projects/C--Users-user/run.jsonl
    umount m
    before=$(sha256sum < v.img)

    logs() { SUDO_UID=1000 SUDO_GID=100 ${pkgs.lib.getExe dfnstrate} logs -c ${../../configs/vm.toml} --volume-image v.img "$@"; }
    logs --out "$PWD/out/nested" --also Users/user/.claude/projects --also Users/user/missing
    o=out/nested
    [ "$(cat $o/Users/user/AppData/Local/defenestrate/shell.log)" = shell ]
    [ "$(cat $o/Users/user/AppData/Local/defenestrate/winget/Discord.Discord.log)" = winget ]
    [ "$(cat $o/ProgramData/defenestrate/linux-first.log)" = first ]
    [ "$(cat $o/Windows/Setup/defenestrate/updates.log)" = updates ]
    [ "$(cat $o/Users/user/.claude/projects/C--Users-user/run.jsonl)" = '{}' ]
    # The caller's: copies, and the directories it made, never one that was
    # there before (the build directory stays the builder's).
    [ "$(stat -c %u:%g $o/Users/user/AppData/Local/defenestrate/shell.log)" = 1000:100 ]
    [ "$(stat -c %u:%g out)" = 1000:100 ]
    [ "$(stat -c %u:%g out/nested)" = 1000:100 ]
    [ "$(stat -c %u:%g $o/ProgramData $o/Users $o/Users/user/AppData)" = "1000:100
    1000:100
    1000:100" ]
    [ "$(stat -c %u . )" != 1000 ]
    [ ! -e $o/Windows/Setup/defenestrate/big.vhdx ] || { echo "copied a 60 MB file"; exit 1; }
    [ "$(sha256sum < v.img)" = "$before" ] || { echo "logs wrote to the volume"; exit 1; }
    if logs --out "$PWD/out2" --also ../etc; then echo "--also ../ was let through"; exit 1; fi

    mkdir $out
  '')
