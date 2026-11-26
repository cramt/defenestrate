# The boot queue's runner (cli/src/boot.rs has the design). Two tasks run it:
#   defenestrate-boot       -Phase Machine, as SYSTEM at every startup
#   defenestrate-boot-user  -Phase User, as the user (elevated) at every logon
# Each takes its phase's queue (machine\ or user\ next to this script) in
# name order, runs every action that has no .done marker, and writes the
# marker after it succeeds. The first failure stops the phase: it and what
# comes after it wait for the next boot, in order. Nothing pending is the
# usual case and costs one directory listing.
#
# -Register registers both tasks from machine.xml and user.xml: at first
# logon, and from linux-first.ps1 on an install from before the queue.
#
# Logs: %ProgramData%\defenestrate\boot-machine.log, boot-user.log.
param(
    [ValidateSet('Machine', 'User')][string]$Phase,
    [switch]$Register,
    # Tests only (nix/tests/boot-queue.nix): a script dot-sourced after the
    # Windows-only functions below, replacing them with fakes.
    [string]$Fakes
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$tasks = [ordered]@{ 'defenestrate-boot' = 'machine.xml'; 'defenestrate-boot-user' = 'user.xml' }

# Each phase's queue may only touch its own root: an HKCU import can't run
# as SYSTEM, and the user phase can't write HKLM, whatever the files say.
$roots = @{ Machine = 'HKEY_LOCAL_MACHINE\'; User = 'HKEY_CURRENT_USER\' }

function Invoke-RegImport([string]$file) {
    $ErrorActionPreference = 'Continue'
    $out = & reg.exe import $file 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    foreach ($line in $out) { if ($line.Trim()) { Log "  $line" } }
    if ($code -ne 0) { throw "reg import exited with $code" }
}

function Invoke-Spi([uint32]$action, [uint32]$uiParam, [object]$pvParam) {
    if (-not ('Defenestrate.Spi' -as [type])) {
        Add-Type -Namespace Defenestrate -Name Spi -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true)]
public static extern bool SystemParametersInfo(uint action, uint uiParam, int[] pvParam, uint winIni);
[DllImport("user32.dll", SetLastError = true, EntryPoint = "SystemParametersInfoW")]
public static extern bool SystemParametersInfoPtr(uint action, uint uiParam, System.IntPtr pvParam, uint winIni);
'@
    }
    # SPIF_UPDATEINIFILE | SPIF_SENDCHANGE: written back as imported, and
    # WM_SETTINGCHANGE broadcast.
    $flags = 3
    $ok = if ($pvParam -is [int[]]) {
        [Defenestrate.Spi]::SystemParametersInfo($action, $uiParam, $pvParam, $flags)
    } else {
        [Defenestrate.Spi]::SystemParametersInfoPtr($action, $uiParam, [IntPtr][int]$pvParam, $flags)
    }
    if (-not $ok) { throw "SystemParametersInfo(0x{0:x}) failed: {1}" -f $action, [Runtime.InteropServices.Marshal]::GetLastWin32Error() }
}

function Get-UserValue([string]$key, [string]$name) {
    [int](Get-ItemProperty -LiteralPath "HKCU:\$key" -Name $name).$name
}

# What runs as SYSTEM (or as an elevated admin) must only be writable by
# SYSTEM, Administrators and TrustedInstaller: the queue's directory, so no
# one else can add an action, and each file the run reads.
function Assert-AdminOnly([string]$path) {
    $trusted = @('S-1-5-18', 'S-1-5-32-544', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464', 'S-1-3-0')
    # WriteData, AppendData, WRITE_DAC, WRITE_OWNER, GENERIC_ALL, GENERIC_WRITE
    $write = 0x2 -bor 0x4 -bor 0x40000 -bor 0x80000 -bor 0x10000000 -bor 0x40000000
    $acl = Get-Acl -LiteralPath $path
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($trusted -notcontains $owner) { throw "$path is owned by $owner, not SYSTEM or Administrators" }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -ne 'Allow') { continue }
        if (([int64]$rule.FileSystemRights -band $write) -and $trusted -notcontains $rule.IdentityReference.Value) {
            throw "$path lets $($rule.IdentityReference.Value) write it ($($rule.FileSystemRights))"
        }
    }
}

function Register-Tasks {
    foreach ($name in $tasks.Keys) {
        $ErrorActionPreference = 'Continue'
        $out = & schtasks.exe /create /tn $name /xml (Join-Path $here $tasks[$name]) /f 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        if ($code -ne 0) { throw "schtasks /create $name exited with $code`: $out" }
        Write-Output "registered $name"
    }
}

if ($Fakes) { . $Fakes }

if ($Register) {
    Register-Tasks
    exit 0
}
if (-not $Phase) { throw 'boot.ps1 takes -Phase Machine, -Phase User or -Register' }

$state = Join-Path $env:ProgramData 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state "boot-$($Phase.ToLowerInvariant()).log"
$lines = @(Get-Content -LiteralPath $log -ErrorAction SilentlyContinue)
if ($lines.Count -gt 1000) { Set-Content -LiteralPath $log -Value $lines[-500..-1] }
function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

function Get-Sha256([string]$text) {
    $sha = [Security.Cryptography.SHA256]::Create()
    -join ($sha.ComputeHash([Text.Encoding]::ASCII.GetBytes($text)) | ForEach-Object { $_.ToString('x2') })
}

# The SPIs, each with what it takes from HKCU after the import.
function Invoke-Refresh([string]$refresh) {
    $mouse = 'Control Panel\Mouse'
    $keyboard = 'Control Panel\Keyboard'
    switch ($refresh) {
        'mouse' {
            $v = [int[]]@((Get-UserValue $mouse 'MouseThreshold1'), (Get-UserValue $mouse 'MouseThreshold2'), (Get-UserValue $mouse 'MouseSpeed'))
            Invoke-Spi 0x0004 0 $v  # SPI_SETMOUSE
            Log "  SPI_SETMOUSE thresholds $($v[0]),$($v[1]) acceleration $($v[2]): in effect now"
        }
        'mouseSpeed' {
            $v = Get-UserValue $mouse 'MouseSensitivity'
            Invoke-Spi 0x0071 0 $v  # SPI_SETMOUSESPEED
            Log "  SPI_SETMOUSESPEED $($v): in effect now"
        }
        'mouseCurve' {
            # No SPI for the curve; setting the speed and acceleration again
            # is when Windows recomputes from it, as libpointing does
            # (boot.rs, Refresh::MouseCurve).
            $v = Get-UserValue $mouse 'MouseSensitivity'
            Invoke-Spi 0x0071 0 $v
            $m = [int[]]@((Get-UserValue $mouse 'MouseThreshold1'), (Get-UserValue $mouse 'MouseThreshold2'), (Get-UserValue $mouse 'MouseSpeed'))
            Invoke-Spi 0x0004 0 $m
            Log "  SmoothMouseXCurve/YCurve: no SPI of their own; SPI_SETMOUSESPEED $v and SPI_SETMOUSE set again for Windows to recompute from them (unconfirmed on Windows 11; certain from the next logon)"
        }
        'keyboardDelay' {
            $v = Get-UserValue $keyboard 'KeyboardDelay'
            Invoke-Spi 0x0017 $v $null  # SPI_SETKEYBOARDDELAY
            Log "  SPI_SETKEYBOARDDELAY $($v): in effect now"
        }
        'keyboardSpeed' {
            $v = Get-UserValue $keyboard 'KeyboardSpeed'
            Invoke-Spi 0x000B $v $null  # SPI_SETKEYBOARDSPEED
            Log "  SPI_SETKEYBOARDSPEED $($v): in effect now"
        }
        default { throw "unknown refresh '$refresh'" }
    }
}

function Invoke-Action([System.IO.FileInfo]$file) {
    $a = Get-Content -Raw -LiteralPath $file.FullName | ConvertFrom-Json
    switch ($a.action) {
        'regImport' {
            $reg = [IO.Path]::ChangeExtension($file.FullName, '.reg')
            Assert-AdminOnly $reg
            # Written as UTF-16 of the text dfnstrate hashed; a mismatch is a
            # torn write or a stale payload, not something to import.
            $text = [IO.File]::ReadAllText($reg, [Text.Encoding]::Unicode)
            $got = Get-Sha256 $text
            if ($got -ne $a.sha256) { throw "$($file.Name): the .reg's sha256 is $got, the action says $($a.sha256)" }
            $root = $roots[$Phase]
            foreach ($line in $text -split "`r?`n") {
                if ($line -match '^\[-?(.*)\]$' -and -not $Matches[1].StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
                    throw "$($file.Name) has [$($Matches[1])], outside $root, which the $Phase phase doesn't touch"
                }
            }
            Invoke-RegImport $reg
            Log "  imported $(Split-Path -Leaf $reg)"
            if ($Phase -eq 'User') {
                foreach ($r in @($a.refresh)) { if ($r) { Invoke-Refresh $r } }
                foreach ($v in @($a.atNextLogon)) { if ($v) { Log "  $v has no refresh: in effect from the next logon" } }
            }
        }
        default { throw "$($file.Name): unknown action '$($a.action)'" }
    }
}

try {
    $queue = Join-Path $here $Phase.ToLowerInvariant()
    $pending = @(Get-ChildItem -LiteralPath $queue -Filter '*.json' -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d{6}-.+\.json$' -and -not (Test-Path -LiteralPath ([IO.Path]::ChangeExtension($_.FullName, '.done'))) } |
        Sort-Object Name)
    if ($pending.Count -eq 0) {
        Log 'nothing pending'
        exit 0
    }
    Assert-AdminOnly $queue
    Log "$($pending.Count) pending: $(($pending | ForEach-Object { $_.BaseName }) -join ', ')"
    foreach ($f in $pending) {
        Log "running $($f.BaseName)"
        Assert-AdminOnly $f.FullName
        Invoke-Action $f
        Set-Content -LiteralPath ([IO.Path]::ChangeExtension($f.FullName, '.done')) -Value ('{0:yyyy-MM-dd HH:mm:ss}' -f (Get-Date))
        Log "done $($f.BaseName)"
    }
} catch {
    Log "FAILED: $_; it and what's after it run at the next boot"
    exit 1
}
