# Everything the first real boot needs to install config's apps: the
# installers from the lock, the plan, install-apps.ps1, and the scheduled
# tasks that run it. Copied into the image under
# C:\Windows\Setup\defenestrate\apps.
{ lib, runCommand, fetchurl, writeText, python3, libxml2, locked, wanted, userName }:

let
  # wanted is { baked = [ id ]; winget = [ id ]; msstore = [ id ]; }, one
  # list per config option; each ID must be locked as that same kind.
  relock = {
    baked = id: id;
    winget = id: "--winget ${id}";
    msstore = id: "--msstore ${id}";
  };
  entry = kind: id:
    let
      app = locked.${id} or (throw ''
        ${id} isn't in apps.lock.json. Lock it with:
          nix run .#lock-apps -- ${relock.${kind} id}'');
      lockedAs = lib.head (lib.attrNames app);
    in
    if lockedAs == kind then { inherit id; ${kind} = app.${kind}; }
    else throw ''
      ${id} is configured as ${kind} but locked as ${lockedAs}. Relock it with:
        nix run .#lock-apps -- ${relock.${kind} id}'';

  # Baked first, in config order; install-apps.ps1 keeps that order.
  plan = lib.concatMap (kind: map (entry kind) wanted.${kind}) [ "baked" "winget" "msstore" ];

  installers = map
    (app: fetchurl { inherit (app.baked) url hash; name = app.baked.file; })
    (lib.filter (app: app ? baked) plan);

  # Logon tasks rather than RunOnce entries: explorer is what runs those,
  # and explorer isn't the shell. Both run alongside the shell, without a
  # UAC prompt:
  # - SYSTEM, in session 0, for machine-wide installers. Not elevated in
  #   the user's session: a non-elevated keyboard hook gets no keys while
  #   an elevated window has focus, and the task's hidden console and
  #   installers held it, so the shell's keys were dead until they finished.
  # - The plain user (install-apps.ps1 -User) for per-user installers, which
  #   break when elevated (Discord's Squirrel Update.exe crashes with
  #   0xC0000005) and belong in the user's profile, and for winget, which
  #   doesn't run as SYSTEM.
  # schtasks' defaults would skip a task on battery and stop it after 72
  # hours.
  mkTask = { principal, args, log }: writeText "task.xml" ''
    <?xml version="1.0" encoding="UTF-16"?>
    <Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
      <RegistrationInfo>
        <Description>Installs defenestrate's apps, then deletes itself. Log: ${log}</Description>
      </RegistrationInfo>
      <Triggers>
        <LogonTrigger>
          <Enabled>true</Enabled>
          <UserId>${userName}</UserId>
        </LogonTrigger>
      </Triggers>
      <Principals>
        <Principal id="Author">
          ${principal}
        </Principal>
      </Principals>
      <Settings>
        <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
        <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
        <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
        <ExecutionTimeLimit>PT4H</ExecutionTimeLimit>
        <Priority>5</Priority>
      </Settings>
      <Actions Context="Author">
        <Exec>
          <Command>powershell.exe</Command>
          <Arguments>-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\Windows\Setup\defenestrate\apps\install-apps.ps1${args}</Arguments>
        </Exec>
      </Actions>
    </Task>
  '';
  tasks = {
    "task.xml" = mkTask {
      # S-1-5-18 is LocalSystem.
      principal = "<UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel>";
      args = "";
      log = "C:\\Windows\\Setup\\defenestrate\\apps.log";
    };
    "task-user.xml" = mkTask {
      principal = "<UserId>${userName}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>";
      args = " -User";
      log = "%LOCALAPPDATA%\\defenestrate\\apps.log";
    };
  };
in
if plan == [ ] then null else
runCommand "defenestrate-apps" { nativeBuildInputs = [ python3 libxml2 ]; } ''
  mkdir -p $out/installers
  ${lib.concatMapStrings (i: "cp ${i} $out/installers/${i.name}\n") installers}
  cp ${writeText "apps.json" (builtins.toJSON plan)} $out/apps.json
  cp ${./install-apps.ps1} $out/install-apps.ps1
  # schtasks only takes task XML as UTF-16.
  ${lib.concatStrings (lib.mapAttrsToList (name: xml: ''
    python3 -I -c 'import sys; open(sys.argv[2], "wb").write(b"\xff\xfe" + open(sys.argv[1], "rb").read().decode("ascii").replace("\n", "\r\n").encode("utf-16-le"))' \
      ${xml} $out/${name}
    xmllint --noout $out/${name}
  '') tasks)}
''
