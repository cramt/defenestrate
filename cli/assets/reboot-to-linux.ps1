# Boots the firmware's Linux entry once, then reboots. Run elevated, in
# session 0, by the on-demand task defenestrate-reboot-to-linux, which the
# Start Menu's "Reboot to Linux" starts. Sets UEFI BootNext ({fwbootmgr}'s
# bootsequence) rather than touching BootOrder, so the boot after that goes
# wherever the firmware's order says, which is the same thing
# `dfnstrate reboot` does from the Linux side.
#
# Which entry is Linux comes from reboot.json: {"description": null} picks
# the first entry in the firmware's boot order that boots an .efi file other
# than Windows Boot Manager or the removable-media fallback; a description
# picks the entry with exactly that description. If nothing matches, nothing
# happens: rebooting anyway would land back in Windows, which is worse than
# not rebooting.
# Log: %LOCALAPPDATA%\defenestrate\reboot.log
$ErrorActionPreference = 'Stop'
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'reboot.log'
function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

# Well-known BCD object IDs and element types (BCD WMI provider docs).
$fwbootmgr = '{a5a30fa2-3d06-4e9f-b5f4-a01df9d1fcba}'
$bootmgr = '{9dea862c-5cdd-4e70-acc1-f32b344d4795}'
$DisplayOrder = 0x24000001
$AppPath = 0x12000002
$Description = 0x12000004

# Through WMI rather than parsing `bcdedit /enum`, whose labels are
# translated on non-English installs. Objects are opened by WMI path:
# BcdStore.OpenObject hands back an embedded object, which PowerShell gives
# no methods, so GetElement couldn't be called on it.
function Open([string]$id) {
    [wmi]"root\wmi:BcdObject.Id=`"$id`",StoreFilePath=`"`""
}
function Element($object, [uint32]$type) {
    try {
        $r = $object.GetElement($type)
        if ($r.ReturnValue) { return $r.Element }
    } catch {
        # A missing element (no path on a firmware-UI entry, say) throws,
        # with an empty message, rather than returning ReturnValue false.
    }
    return $null
}

try {
    $config = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'reboot.json') | ConvertFrom-Json
    $order = (Element (Open $fwbootmgr) $DisplayOrder).Ids
    if (-not $order) { throw "no displayorder in {fwbootmgr}; is this a UEFI boot?" }
    $entries = foreach ($id in $order) {
        $o = Open $id
        [pscustomobject]@{
            Id = $id
            Description = "$((Element $o $Description).String)"
            Path = "$((Element $o $AppPath).String)"
        }
    }
    foreach ($e in $entries) { Log "firmware entry $($e.Id) '$($e.Description)' path '$($e.Path)'" }

    if ($config.description) {
        $want = "description '$($config.description)'"
        $linux = $entries | Where-Object { $_.Description -ceq $config.description } | Select-Object -First 1
    } else {
        $want = 'auto: first entry booting an .efi file that is not Windows or the removable-media fallback'
        # Shell, network (PXE/HTTP) and setup-menu entries have no file path.
        # A USB stick's entry, if the firmware made one, boots the fallback
        # path, and so does a disk's "UEFI OS" entry; a Linux install that
        # only boots that way needs linuxBootEntry set.
        $linux = $entries | Where-Object {
            $_.Id -ne $bootmgr -and
            $_.Path -like '*.efi' -and
            $_.Path -notlike '\EFI\Microsoft\*' -and
            $_.Path -ne '\EFI\BOOT\BOOTX64.EFI'
        } | Select-Object -First 1
    }
    if (-not $linux) {
        Log "no Linux entry found ($want); not rebooting"
        exit 1
    }

    Log "booting '$($linux.Description)' ($($linux.Id)) once ($want)"
    $ErrorActionPreference = 'Continue'
    $out = & bcdedit.exe /set $fwbootmgr bootsequence $linux.Id 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    foreach ($line in $out) { if ($line.Trim()) { Log "  $line" } }
    if ($code -ne 0) { throw "bcdedit exited with $code" }
    Log 'rebooting'
    & shutdown.exe /r /t 0
} catch {
    Log "FAILED, not rebooting: $_"
    exit 1
}
