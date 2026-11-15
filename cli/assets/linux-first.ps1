# Puts the firmware's Linux entry first in UEFI BootOrder ({fwbootmgr}'s
# displayorder), so Linux stays the default and Windows boots only when
# asked to (`dfnstrate reboot`, BootNext). Run as SYSTEM at every startup by
# the task defenestrate-linux-first: Windows' first boot adds its own
# "Windows Boot Manager" entry and puts it first, and boot manager updates
# can do the same later.
#
# A wrong pick here changes every boot after it, not just the next one as
# in reboot-to-linux.ps1, so this takes linux-entry.ps1's match only when
# there's exactly one. Two or more (two distros, a duplicated entry, an
# OEM tool on the ESP alongside Linux) and it changes nothing and says so:
# pin the entry with linuxBootEntry = { description = "..." }.
# Log: %ProgramData%\defenestrate\linux-first.log
$ErrorActionPreference = 'Stop'
$state = Join-Path $env:ProgramData 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'linux-first.log'
# One line every boot adds up; the newest few hundred are plenty.
$lines = @(Get-Content -LiteralPath $log -ErrorAction SilentlyContinue)
if ($lines.Count -gt 1000) { Set-Content -LiteralPath $log -Value $lines[-500..-1] }
function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

. (Join-Path $PSScriptRoot 'linux-entry.ps1')

try {
    $entries = @(Get-FirmwareEntries)
    $found = Find-LinuxEntries $entries
    $linux = $found.Matches
    if ($linux.Count -eq 0) {
        Log "no Linux entry ($($found.Want)); boot order left alone"
        exit 0
    }
    if ($linux.Count -gt 1) {
        $fix = if ($found.Pinned) { 'remove the duplicate (efibootmgr -B on Linux)' } else { 'pin one with linuxBootEntry = { description = ... }' }
        Log "$($linux.Count) entries could be Linux ($($found.Want)); not guessing, boot order left alone. To fix, $($fix):"
        foreach ($e in $linux) { Log "  $($e.Id) '$($e.Description)' path '$($e.Path)'" }
        exit 1
    }
    $linux = $linux[0]
    if ($entries[0].Id -eq $linux.Id) {
        Log "'$($linux.Description)' ($($linux.Id)) is already first"
        exit 0
    }

    Log "boot order was:"
    foreach ($e in $entries) { Log "  $($e.Id) '$($e.Description)' path '$($e.Path)'" }
    Log "putting '$($linux.Description)' ($($linux.Id)) first ($($found.Want))"
    $ErrorActionPreference = 'Continue'
    $out = & bcdedit.exe /set $fwbootmgr displayorder $linux.Id /addfirst 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    foreach ($line in $out) { if ($line.Trim()) { Log "  $line" } }
    if ($code -ne 0) { throw "bcdedit exited with $code" }
    $first = @(Get-FirmwareEntries)[0]
    if ($first.Id -ne $linux.Id) { throw "bcdedit succeeded but '$($first.Description)' ($($first.Id)) is still first" }
    Log 'done'
} catch {
    Log "FAILED: $_"
    exit 1
}
