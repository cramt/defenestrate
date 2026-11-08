{ lib, config, productKey
  # Register apps.nix's scheduled task, which installs the apps at the
  # next logon: the machine's first real boot, for image.nix.
, armApps ? false
  # How first logon ends once the shell is switched. image.nix powers off
  # instead, which is how its VM knows setup is done.
, firstLogonEnd ? "reboot"
}:

assert lib.elem firstLogonEnd [ "reboot" "poweroff" ];


let
  x = lib.escapeXML;

  component = name: body: ''
    <component name="${name}" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
    ${body}
    </component>
  '';

  locales = ''
    <InputLocale>${x config.inputLocale}</InputLocale>
    <SystemLocale>${x config.language}</SystemLocale>
    <UILanguage>${x config.language}</UILanguage>
    <UserLocale>${x config.language}</UserLocale>
  '';

  password = ''
    <Password>
      <Value>${x config.user.password}</Value>
      <PlainText>true</PlainText>
    </Password>
  '';

  wipe = config.disk.wipe or null;

  # EFI + MSR + Windows. No recovery partition: WinRE stays inside the
  # Windows partition, and it's one less partition for Linux to step around.
  diskConfiguration = lib.optionalString (wipe != null) ''
    <DiskConfiguration>
      <Disk wcm:action="add">
        <DiskID>${toString wipe.diskId}</DiskID>
        <WillWipeDisk>true</WillWipeDisk>
        <CreatePartitions>
          <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>300</Size></CreatePartition>
          <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
          <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
        </CreatePartitions>
        <ModifyPartitions>
          <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>EFI</Label></ModifyPartition>
          <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>2</PartitionID></ModifyPartition>
          <ModifyPartition wcm:action="add"><Order>3</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
        </ModifyPartitions>
      </Disk>
      <WillShowUI>Never</WillShowUI>
    </DiskConfiguration>
  '';

  installTo = lib.optionalString (wipe != null) ''
    <InstallTo>
      <DiskID>${toString wipe.diskId}</DiskID>
      <PartitionID>3</PartitionID>
    </InstallTo>
    <InstallToAvailablePartition>false</InstallToAvailablePartition>
    <WillShowUI>Never</WillShowUI>
  '';

  setupDir = "%SystemRoot%\\Setup\\defenestrate";
in
''
  <?xml version="1.0" encoding="utf-8"?>
  <unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
    <settings pass="windowsPE">
      ${component "Microsoft-Windows-International-Core-WinPE" ''
        <SetupUILanguage><UILanguage>${x config.language}</UILanguage></SetupUILanguage>
        ${locales}
      ''}
      ${component "Microsoft-Windows-Setup" ''
        ${diskConfiguration}
        <!-- Setup otherwise phones home for updated setup binaries and drivers mid-install. -->
        <DynamicUpdate><Enable>false</Enable></DynamicUpdate>
        <Diagnostics><OptIn>false</OptIn></Diagnostics>
        <ImageInstall>
          <OSImage>
            ${installTo}
            <!-- iso.nix exports the chosen edition alone, so it's always image 1. -->
            <InstallFrom>
              <MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>1</Value></MetaData>
            </InstallFrom>
          </OSImage>
        </ImageInstall>
        <UserData>
          <AcceptEula>true</AcceptEula>
          <ProductKey>
            <Key>${productKey}</Key>
            <WillShowUI>Never</WillShowUI>
          </ProductKey>
        </UserData>
      ''}
    </settings>

    <settings pass="specialize">
      ${component "Microsoft-Windows-Shell-Setup" ''
        <ComputerName>${x config.computerName}</ComputerName>
        <TimeZone>${x config.timeZone}</TimeZone>
      ''}
      ${component "Microsoft-Windows-Deployment" ''
        <RunSynchronous>
          <RunSynchronousCommand wcm:action="add">
            <Order>1</Order>
            <Path>${x ''cmd.exe /c "${setupDir}\specialize.cmd > ${setupDir}\specialize.log 2>&1"''}</Path>
          </RunSynchronousCommand>
        </RunSynchronous>
      ''}
    </settings>

    <settings pass="oobeSystem">
      ${component "Microsoft-Windows-International-Core" locales}
      ${component "Microsoft-Windows-Shell-Setup" ''
        ${lib.optionalString config.user.autoLogon ''
          <AutoLogon>
            <Enabled>true</Enabled>
            <Username>${x config.user.name}</Username>
            ${password}
            <!-- Without a count, Windows turns autologon off after the first one. -->
            <LogonCount>9999999</LogonCount>
          </AutoLogon>
        ''}
        <!-- Runs elevated in the first real user session, after OOBE is done.
             That session still gets explorer; the reboot makes the next one ours. -->
        <FirstLogonCommands>
          <SynchronousCommand wcm:action="add">
            <Order>1</Order>
            <CommandLine>${x ''cmd.exe /c "reg import ${setupDir}\shell.reg > ${setupDir}\firstlogon.log 2>&1"''}</CommandLine>
            <Description>Replace explorer.exe as the shell</Description>
          </SynchronousCommand>
          ${lib.optionalString armApps ''
            <SynchronousCommand wcm:action="add">
              <Order>2</Order>
              <CommandLine>${x ''cmd.exe /c "schtasks /create /tn defenestrate-apps /xml ${setupDir}\apps\task.xml >> ${setupDir}\firstlogon.log 2>&1"''}</CommandLine>
              <Description>Install the apps at the next logon</Description>
            </SynchronousCommand>
          ''}
          <SynchronousCommand wcm:action="add">
            <Order>${if armApps then "3" else "2"}</Order>
            <CommandLine>shutdown.exe ${if firstLogonEnd == "reboot" then "/r" else "/s"} /t 0</CommandLine>
            <Description>${if firstLogonEnd == "reboot" then "Log in again under the new shell" else "Done; the next logon gets the new shell"}</Description>
          </SynchronousCommand>
        </FirstLogonCommands>
        <OOBE>
          <HideEULAPage>true</HideEULAPage>
          <HideOEMRegistrationScreen>true</HideOEMRegistrationScreen>
          <HideOnlineAccountScreens>true</HideOnlineAccountScreens>
          <HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE>
          <ProtectYourPC>3</ProtectYourPC>
        </OOBE>
        <UserAccounts>
          <LocalAccounts>
            <LocalAccount wcm:action="add">
              <Name>${x config.user.name}</Name>
              <Group>Administrators</Group>
              ${password}
            </LocalAccount>
          </LocalAccounts>
        </UserAccounts>
      ''}
    </settings>
  </unattend>
''
