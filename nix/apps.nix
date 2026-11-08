# Everything the first real boot needs to install config's apps: the
# installers from the lock, the plan, install-apps.ps1, and the scheduled
# task that runs it. Copied into the image under
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

  # A logon task rather than a RunOnce entry: explorer is what runs those,
  # and explorer isn't the shell. The task runs elevated without a prompt,
  # alongside the shell, as the user (so winget works and per-user
  # installers land in their profile). schtasks' defaults would skip it on
  # battery and stop it after 72 hours.
  task = writeText "task.xml" ''
    <?xml version="1.0" encoding="UTF-16"?>
    <Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
      <RegistrationInfo>
        <Description>Installs defenestrate's apps, then deletes itself. Log: C:\Windows\Setup\defenestrate\apps.log</Description>
      </RegistrationInfo>
      <Triggers>
        <LogonTrigger>
          <Enabled>true</Enabled>
          <UserId>${userName}</UserId>
        </LogonTrigger>
      </Triggers>
      <Principals>
        <Principal id="Author">
          <UserId>${userName}</UserId>
          <LogonType>InteractiveToken</LogonType>
          <RunLevel>HighestAvailable</RunLevel>
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
          <Arguments>-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\Windows\Setup\defenestrate\apps\install-apps.ps1</Arguments>
        </Exec>
      </Actions>
    </Task>
  '';
in
if plan == [ ] then null else
runCommand "defenestrate-apps" { nativeBuildInputs = [ python3 libxml2 ]; } ''
  mkdir -p $out/installers
  ${lib.concatMapStrings (i: "cp ${i} $out/installers/${i.name}\n") installers}
  cp ${writeText "apps.json" (builtins.toJSON plan)} $out/apps.json
  cp ${./install-apps.ps1} $out/install-apps.ps1
  # schtasks only takes task XML as UTF-16.
  python3 -I -c 'import sys; open(sys.argv[2], "wb").write(b"\xff\xfe" + open(sys.argv[1], "rb").read().decode("ascii").replace("\n", "\r\n").encode("utf-16-le"))' \
    ${task} $out/task.xml
  xmllint --noout $out/task.xml
''
