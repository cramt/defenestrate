# Boots the firmware's Linux entry once, then reboots. Run elevated, in
# session 0, by the on-demand task defenestrate-reboot-to-linux, which the
# Start Menu's "Reboot to Linux" starts. Sets UEFI BootNext ({fwbootmgr}'s
# bootsequence) rather than touching BootOrder, so the boot after that goes
# wherever the firmware's order says, which is the same thing
# `dfnstrate reboot` does from the Linux side.
#
# Which entry is Linux: the first match of linux-entry.ps1, in the
# firmware's boot order. If nothing matches, nothing happens: rebooting
# anyway would land back in Windows, which is worse than not rebooting.
# Log: %LOCALAPPDATA%\defenestrate\reboot.log
$ErrorActionPreference = 'Stop'
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'reboot.log'
function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

. (Join-Path $PSScriptRoot 'linux-entry.ps1')

try {
    $entries = Get-FirmwareEntries
    foreach ($e in $entries) { Log "firmware entry $($e.Id) '$($e.Description)' path '$($e.Path)'" }

    $found = Find-LinuxEntries $entries
    $want = $found.Want
    # The first in boot order: a wrong pick here costs one boot, unlike
    # linux-first.ps1's, so it doesn't refuse on two.
    $linux = $found.Matches | Select-Object -First 1
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
