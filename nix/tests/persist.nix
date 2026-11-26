# dfnstrate persist against NTFS volumes, in a VM (ntfs-3g needs root and
# FUSE): what save copies off an install and who owns it, that restore
# writes it back without compatibility.ini and with the permissions the
# profile directory hands down (not ntfs-3g's Everyone: full control), that
# a seed is taken from a live WAL database consistently and only when
# nothing was kept, also while a browser holds it the way Firefox and Zen
# do (locking_mode=EXCLUSIVE) and writes to it, and that a database held in
# rollback-journal mode fails the seed before anything is written. Then
# `dfnstrate update` seeding an install's existing profile: once, moving
# the profile's own files aside, the rest of the update done either way,
# and `persist seed --force` again.
{ pkgs, dfnstrate, cache }:

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
    [ "$(ls $s | sort | tr '\n' ' ')" = "containers.json cookies.sqlite defenestrate-seed.txt key4.db " ] || { ls -la $s; exit 1; }
    grep -q 'Default=Profiles/defenestrate.default-release' $ff/installs.ini
    grep -q '\[Install308046B0AF4A39CB\]' $ff/profiles.ini
    umount m

    # A browser holding its databases as Firefox and Zen do: WAL with
    # locking_mode=EXCLUSIVE, so sqlite's backup can't open them, and
    # writing (a checkpoint every few pages, so the database file and the
    # log both change under the copy). The copy goes around the lock.
    browser() {
      if [ -n "''${browser_pid:-}" ]; then kill $browser_pid 2>/dev/null || true; wait $browser_pid || true; fi
      (
        echo 'PRAGMA locking_mode=EXCLUSIVE;'
        echo 'PRAGMA wal_autocheckpoint=8;'
        echo ".dbconfig no_ckpt_on_close on"
        echo "INSERT INTO moz_cookies VALUES ('held', ''');"
        for i in $(seq 1 12000); do echo "INSERT INTO moz_cookies VALUES (hex(randomblob(400)), ''');"; sleep 0.05; done
      ) | sqlite3 zen/cookies.sqlite > /dev/null &
      browser_pid=$!
      sleep 1
      # Held: an ordinary reader is refused.
      if sqlite3 -readonly -cmd '.timeout 200' zen/cookies.sqlite 'SELECT 1;' 2>/dev/null; then echo "test setup: the database isn't held"; exit 1; fi
    }
    browser
    newvol held.img
    dfn restore $(cfg "seed = { profile = \"$here/zen\" }") --volume-image held.img --store $here/state/empty | tee held.out
    grep -q 'cookies.sqlite: held by the browser, so copied with its write-ahead log and checked' held.out
    ntfs-3g -o ro held.img m
    cp $s/cookies.sqlite held.sqlite
    umount m
    [ "$(sqlite3 held.sqlite 'PRAGMA integrity_check')" = ok ]
    [ "$(sqlite3 held.sqlite "SELECT count(*) FROM moz_cookies WHERE name IN ('yt', 'work', 'lost', 'held')")" = 4 ]

    # Held in rollback-journal mode, a copy of the file can be half a
    # transaction: the seed fails, nothing is written.
    mkdir journal
    sqlite3 journal/cookies.sqlite "CREATE TABLE moz_cookies (name TEXT, originAttributes TEXT);"
    (echo 'PRAGMA locking_mode=EXCLUSIVE;'; echo 'BEGIN EXCLUSIVE;'; echo 'SELECT 1;'; sleep 3600) | sqlite3 journal/cookies.sqlite > /dev/null &
    journal_pid=$!
    sleep 1
    newvol journal.img
    unwritten=$(sha256sum < journal.img)
    if dfn restore $(cfg "seed = { profile = \"$here/journal\" }") --volume-image journal.img --store $here/state/empty 2> journal.err; then echo "seeded from a held rollback-journal database"; exit 1; fi
    cat journal.err
    grep -q 'locked by the browser' journal.err
    grep -q "isn't in WAL mode" journal.err
    [ "$(sha256sum < journal.img)" = "$unwritten" ] || { echo "wrote to the volume after the seed failed"; exit 1; }

    # update into an install's existing Firefox: the profile its first
    # start made (saturn's from 2026-11-20), with cookies of its own and a
    # log a crash could have left.
    newvol up.img
    ntfs-3g up.img m
    mkdir -p m/Windows/Setup/defenestrate
    mkdir -p $ff/Profiles/kiq5zat0.default-release
    w=$ff/Profiles/kiq5zat0.default-release
    setfattr -n system.ntfs_acl -v 0x$header$dacl$ace$admins$admins $w
    printf '[Install308046B0AF4A39CB]\r\nDefault=Profiles/kiq5zat0.default-release\r\nLocked=1\r\n\r\n[Profile0]\r\nName=default-release\r\nIsRelative=1\r\nPath=Profiles/kiq5zat0.default-release\r\nDefault=1\r\n' > $ff/profiles.ini
    printf '[308046B0AF4A39CB]\r\nDefault=Profiles/kiq5zat0.default-release\r\nLocked=1\r\n' > $ff/installs.ini
    sqlite3 $w/cookies.sqlite "CREATE TABLE moz_cookies (name TEXT, originAttributes TEXT); INSERT INTO moz_cookies VALUES ('windows', ''');"
    echo stale > $w/cookies.sqlite-wal
    echo '{"logins":[]}' > $w/logins.json
    echo 'user_pref("x", 1);' > $w/prefs.js
    acl() { getfattr -e hex -n system.ntfs_acl $1 | sed -n 's/^system.ntfs_acl=0x//p'; }
    own_acl=$(acl $w/cookies.sqlite)
    umount m

    up() {
      cat > up.toml <<EOF
    shell = { command = "cmd.exe" }   # no certificate, so no SOFTWARE hive needed
    [persist.firefox]
    seed = { profile = "$here/$1" }
    EOF
      local status=0
      ${pkgs.lib.getExe dfnstrate} update -c ${../../configs/vm.toml} -c up.toml --cache ${cache} --offline --volume-image up.img > up.out 2> up.err || status=$?
      cat up.out up.err
      return $status
    }
    seeded() { [ "$(sqlite3 $1 "SELECT group_concat(name) FROM (SELECT DISTINCT name FROM moz_cookies WHERE name IN ('yt', 'work', 'lost', 'held', 'windows') ORDER BY name)")" = "$2" ]; }

    browser
    up zen
    grep -qF 'queued the user registry import' up.out
    grep -qF 'seeded kiq5zat0.default-release with cookies.sqlite, key4.db, containers.json' up.out
    ntfs-3g -o ro up.img m
    cp $w/cookies.sqlite up.sqlite
    seeded up.sqlite held,lost,work,yt
    [ ! -e $w/cookies.sqlite-wal ] || { echo "left a stale log next to the seeded database"; exit 1; }
    [ ! -e $w/logins.json ] || { echo "kept logins.json, encrypted with the replaced key4.db"; exit 1; }
    [ ! -e $w/places.sqlite ] || { echo "seeded history without history"; exit 1; }
    grep -q 'user_pref' $w/prefs.js
    grep -q "from $here/zen" $w/defenestrate-seed.txt
    r=$w/defenestrate-seed-replaced
    [ "$(ls $r | sort | tr '\n' ' ')" = "cookies.sqlite cookies.sqlite-wal logins.json " ] || { ls -la $r; exit 1; }
    cp $r/cookies.sqlite replaced.sqlite
    seeded replaced.sqlite windows
    grep -q kiq5zat0 $ff/installs.ini
    [ ! -e $ff/Profiles/defenestrate.default-release ] || { echo "made a profile of its own next to the install's"; exit 1; }
    # Moved aside, a file keeps what it had.
    [ "$(acl $r/cookies.sqlite)" = "$own_acl" ] || { echo "the profile's own cookies.sqlite changed its ACL"; exit 1; }
    # A directory inherits the ACE with its inheritance kept (0x13).
    case $(acl $r) in *00131400ff011f00$system*) ;; *) echo "$r didn't inherit the profile's ACL"; exit 1 ;; esac
    for f in $w/cookies.sqlite $w/defenestrate-seed.txt; do
      a=$(acl $f)
      case $a in *$inherited*) ;; *) echo "$f didn't inherit the profile's ACL: $a"; exit 1 ;; esac
      case $a in *$everyone*) echo "$f grants Everyone: $a"; exit 1 ;; esac
    done
    seed1=$(sha256sum < $w/cookies.sqlite)
    umount m

    # Once per profile: from now on Windows' logins are its own.
    up zen
    grep -qF 'was seeded before' up.out
    ntfs-3g -o ro up.img m
    [ "$(sha256sum < $w/cookies.sqlite)" = "$seed1" ] || { echo "seeded a seeded profile again"; exit 1; }
    umount m

    # A seed that fails is a warning, not a failed update.
    ntfs-3g up.img m
    rm $w/defenestrate-seed.txt
    umount m
    up journal
    grep -qF 'registry is as last queued' up.out
    grep -qF "warning: Windows' Firefox not seeded (the rest of the update is done)" up.err
    ntfs-3g -o ro up.img m
    [ "$(sha256sum < $w/cookies.sqlite)" = "$seed1" ] || { echo "a failed seed changed the profile"; exit 1; }
    umount m

    # --force seeds again, keeping the profile's own, not the last seed's.
    ntfs-3g up.img m
    echo 'user_pref' > $w/defenestrate-seed.txt
    umount m
    kill $browser_pid 2>/dev/null || true
    wait $browser_pid 2>/dev/null || true
    sqlite3 zen/cookies.sqlite "DELETE FROM moz_cookies WHERE name = 'lost';"
    dfn seed $(cfg "seed = { profile = \"$here/zen\" }") --volume-image up.img | tee seed.out
    grep -qF 'was seeded before' seed.out
    dfn seed --force $(cfg "seed = { profile = \"$here/zen\" }") --volume-image up.img
    ntfs-3g -o ro up.img m
    cp $w/cookies.sqlite forced.sqlite
    seeded forced.sqlite held,work,yt
    cp $r/cookies.sqlite replaced.sqlite
    seeded replaced.sqlite windows
    umount m
    kill $journal_pid 2>/dev/null || true

    mkdir $out
  '')
