# dfnstrate persist against NTFS volumes, in a VM (ntfs-3g needs root and
# FUSE): what save copies off an install and who owns it, that restore
# writes it back without compatibility.ini and with the permissions the
# profile directory hands down (not ntfs-3g's Everyone: full control), that
# a seed is taken from a live WAL database consistently and only when
# nothing was kept, and that a browser holding its databases fails the seed
# before anything is written.
{ pkgs, dfnstrate }:

pkgs.vmTools.runInLinuxVM (pkgs.runCommand "dfnstrate-persist-test"
  {
    memSize = 1024;
    nativeBuildInputs = [ pkgs.ntfs3g pkgs.util-linux pkgs.coreutils pkgs.kmod pkgs.attr pkgs.sqlite pkgs.gnugrep ];
  }
  ''
    set -x
    modprobe fuse || true
    dfn() { SUDO_UID=1000 SUDO_GID=100 ${pkgs.lib.getExe dfnstrate} persist "$@"; }
    here=$PWD

    # An install that has run Firefox.
    truncate -s 128M old.img
    mkntfs -q -F -f old.img
    mkdir m
    ntfs-3g old.img m
    ff=m/Users/user/AppData/Roaming/Mozilla/Firefox
    p=$ff/Profiles/abc.default-release
    mkdir -p $p/storage/default
    printf '[Profile0]\r\nPath=Profiles/abc.default-release\r\n' > $ff/profiles.ini
    printf '[308046B0AF4A39CB]\r\nDefault=Profiles/abc.default-release\r\n' > $ff/installs.ini
    sqlite3 $p/cookies.sqlite "CREATE TABLE moz_cookies (name TEXT, originAttributes TEXT); INSERT INTO moz_cookies VALUES ('SID', ''');"
    printf '[Compatibility]\r\nLastVersion=999.0_20991231000000/20991231000000\r\n' > $p/compatibility.ini
    : > $p/parent.lock
    echo idb > $p/storage/default/data
    umount m
    before=$(sha256sum < old.img)

    cfg() {
      cat > layer.toml <<EOF
    [persist.firefox]
    $1
    EOF
      echo "-c ${../../configs/vm.toml} -c $here/layer.toml"
    }

    # Save: read-only, the copies the caller's, the lock left behind.
    dfn save $(cfg "") --volume-image old.img --store $here/state/store
    [ "$(sha256sum < old.img)" = "$before" ] || { echo "save wrote to the volume"; exit 1; }
    k=state/store/firefox
    [ "$(sqlite3 $k/Profiles/abc.default-release/cookies.sqlite 'SELECT name FROM moz_cookies')" = SID ]
    [ "$(cat $k/Profiles/abc.default-release/storage/default/data)" = idb ]
    [ -e $k/Profiles/abc.default-release/compatibility.ini ]
    [ ! -e $k/Profiles/abc.default-release/parent.lock ] || { echo "kept the lock"; exit 1; }
    [ "$(stat -c %u:%g:%a state/store)" = 1000:100:700 ]
    [ "$(stat -c %u:%g $k/Profiles/abc.default-release/cookies.sqlite)" = 1000:100 ]

    # A new install: a profile directory handing down SYSTEM: full control
    # (object and container inherit), owned by Administrators. Built by
    # hand: header, DACL (one ACE), owner, group.
    system=010100000000000512000000
    admins=01020000000000052000000020020000
    header=0100048430000000400000000000000014000000 # self-relative, DACL present and auto-inherited, as a profile's is; owner 0x30, group 0x40, DACL 0x14
    dacl=02001c0001000000                           # one ACE, 28 bytes
    ace=00031400ff011f00$system                     # allow, object+container inherit, full control
    newvol() {
      # Past restore's 1 GiB of headroom for Windows (sparse).
      truncate -s 2G $1
      mkntfs -q -F -f $1
      ntfs-3g $1 m
      mkdir -p m/Users/user/AppData/Roaming
      setfattr -n system.ntfs_acl -v 0x$header$dacl$ace$admins$admins m/Users/user/AppData/Roaming
      umount m
    }
    # SYSTEM's SID, after an inherited (0x10) allow ACE for everything.
    inherited=00101400ff011f00010100000000000512000000
    everyone=010100000000000100000000

    newvol new.img
    dfn restore $(cfg "seed = { profile = \"$here/unused\" }") --volume-image new.img --store $here/state/store
    ntfs-3g -o ro new.img m
    r=$ff/Profiles/abc.default-release
    [ "$(sqlite3 -readonly $r/cookies.sqlite 'SELECT name FROM moz_cookies')" = SID ]
    [ "$(cat $r/storage/default/data)" = idb ]
    grep -q abc.default-release $ff/installs.ini
    [ ! -e $r/compatibility.ini ] || { echo "restored compatibility.ini, which an older Firefox refuses"; exit 1; }
    [ ! -e $ff/Profiles/defenestrate.default-release ] || { echo "seeded over a kept profile"; exit 1; }
    a=$(getfattr -e hex -n system.ntfs_acl $r/cookies.sqlite | sed -n 's/^system.ntfs_acl=0x//p')
    case $a in *$inherited*) ;; *) echo "cookies.sqlite didn't inherit the profile's ACL: $a"; exit 1 ;; esac
    case $a in *$everyone*) echo "cookies.sqlite grants Everyone: $a"; exit 1 ;; esac
    umount m
    # An install that already has a Firefox is left alone.
    if dfn restore $(cfg "") --volume-image new.img --store $here/state/store; then echo "restored over an existing Firefox"; exit 1; fi

    # The seed: a profile a browser is using, its newest cookies only in
    # the write-ahead log (no checkpoint on close).
    mkdir zen
    sqlite3 zen/cookies.sqlite \
      "PRAGMA journal_mode=WAL;" \
      "CREATE TABLE moz_cookies (name TEXT, originAttributes TEXT);" \
      ".dbconfig no_ckpt_on_close on" \
      "INSERT INTO moz_cookies VALUES ('yt', '''),('work', '^userContextId=2'), ('lost', '^userContextId=9');"
    [ -s zen/cookies.sqlite-wal ] || { echo "test setup: no WAL to read"; exit 1; }
    sqlite3 zen/key4.db "CREATE TABLE metaData (id TEXT);"
    echo '{"version":8,"identities":[{"userContextId":1},{"userContextId":2}]}' > zen/containers.json
    sqlite3 zen/places.sqlite "CREATE TABLE moz_places (url TEXT);"
    echo junk > zen/prefs.js

    newvol seed.img
    dfn restore $(cfg "seed = { profile = \"$here/zen\" }") --volume-image seed.img --store $here/state/empty 2> seed.err
    cat seed.err
    grep -qF "define ({9})" seed.err
    ntfs-3g -o ro seed.img m
    s=$ff/Profiles/defenestrate.default-release
    # Off the read-only mount: a WAL database wants a writable directory.
    cp $s/cookies.sqlite seeded.sqlite
    [ "$(sqlite3 seeded.sqlite 'SELECT count(*) FROM moz_cookies')" = 3 ]
    [ "$(ls $s | sort | tr '\n' ' ')" = "containers.json cookies.sqlite key4.db " ] || { ls -la $s; exit 1; }
    grep -q 'Default=Profiles/defenestrate.default-release' $ff/installs.ini
    grep -q '\[Install308046B0AF4A39CB\]' $ff/profiles.ini
    umount m

    # A browser holding its database: the seed fails, nothing is written.
    newvol held.img
    held=$(sha256sum < held.img)
    (echo 'PRAGMA locking_mode=EXCLUSIVE;'; echo 'BEGIN EXCLUSIVE;'; echo 'SELECT 1;'; sleep 60) | sqlite3 zen/cookies.sqlite > /dev/null &
    sleep 1
    if dfn restore $(cfg "seed = { profile = \"$here/zen\" }") --volume-image held.img --store $here/state/empty 2> held.err; then echo "seeded from a locked database"; exit 1; fi
    cat held.err
    grep -q 'locked' held.err
    [ "$(sha256sum < held.img)" = "$held" ] || { echo "wrote to the volume after the seed failed"; exit 1; }
    kill %1 || true

    mkdir $out
  '')
