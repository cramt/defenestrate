# Installs the config's GPU drivers and winget apps (apps.json, IDs only, no
# versions) at each logon of the configured user until all are in, then
# deletes its own
# scheduled task (defenestrate-apps, registered at first logon). On an image,
# that first logon is the build's offline VM, so the first time this runs is
# the machine's first real boot. Runs elevated in the user's session.
# Log: %LOCALAPPDATA%\defenestrate\apps.log
#
# -Limited: what defenestrate-apps-limited runs, unelevated, for an ID that
# failed elevated (Discord's Squirrel installer crashes when elevated):
# installs the one ID in limited-request.txt and writes how it went to
# limited-result.txt.
param([switch]$Limited)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'apps.log'
# One item per line, a winget ID or $wuItem: a rerun after a failure skips
# what already worked.
$doneFile = Join-Path $state 'apps-done.txt'
$taskName = 'defenestrate-apps'
$limitedTask = 'defenestrate-apps-limited'
$request = Join-Path $state 'limited-request.txt'
$result = Join-Path $state 'limited-result.txt'
# Can't clash with a winget ID, which has no colon.
$wuItem = 'windows-update:display-drivers'

function Log([string]$msg) {
    $who = if ($Limited) { ' (unelevated)' } else { '' }
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss}{1} {2}' -f (Get-Date), $who, $msg)
}

# Start-Process -Wait has no timeout, and an installer stuck on a dialog
# nobody sees would hold up everything after it forever. winget's output goes
# to winget\<id>.out.log and its own log to winget\<id>.log: the exit code
# alone says nothing about why an installer failed. Unelevated, user scope
# only: a machine-wide installer would stop on a UAC prompt.
function Invoke-Winget([string]$winget, [string]$id) {
    $logs = Join-Path $state 'winget'
    New-Item -ItemType Directory -Force -Path $logs | Out-Null
    $name = if ($Limited) { "$id.unelevated" } else { $id }
    $scope = if ($Limited) { ' --scope user' } else { '' }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $winget
    $psi.Arguments = "install --id `"$id`" --exact --source winget --silent --accept-package-agreements --accept-source-agreements --disable-interactivity --verbose-logs$scope"
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $started = Get-Date
    $p = [System.Diagnostics.Process]::Start($psi)
    # Both read at once: a full pipe on the one not being read would stall winget.
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    $finished = $p.WaitForExit(30 * 60 * 1000)
    if (-not $finished) { $p.Kill() }
    Set-Content -LiteralPath (Join-Path $logs "$name.out.log") -Value ($out.Result + $err.Result)
    Copy-WingetLog $started (Join-Path $logs "$name.log")
    if (-not $finished) { throw 'still running after 30 minutes, killed it' }
    # 0x8A15002B: already installed, e.g. a rerun after the install worked
    # but the done file didn't get written.
    if (@(0, -1978335189) -notcontains $p.ExitCode) { throw ('exit code 0x{0:X8}, see winget\{1}.log' -f $p.ExitCode, $name) }
}

# winget ignores --log here (1.26.509, saturn): its log only ever lands in
# its own DiagOutputDir, so the newest one since $since is copied out.
function Copy-WingetLog([datetime]$since, [string]$to) {
    $dir = Join-Path $env:LOCALAPPDATA 'Packages\Microsoft.DesktopAppInstaller_8wekyb3d8bbwe\LocalState\DiagOutputDir'
    $newest = Get-ChildItem -LiteralPath $dir -Filter 'WinGet-*.log' -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -ge $since } | Sort-Object LastWriteTime | Select-Object -Last 1
    if ($newest) { Copy-Item -LiteralPath $newest.FullName -Destination $to -Force }
}

# Runs $id through defenestrate-apps-limited, unelevated, and waits for it.
function Invoke-Limited([string]$id) {
    # /f: an install from before the task existed gets it, and a changed
    # limited-task.xml replaces the old one.
    schtasks.exe /create /tn $limitedTask /xml (Join-Path $here 'limited-task.xml') /f | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "registering $limitedTask failed ($LASTEXITCODE)" }
    Remove-Item -LiteralPath $result -ErrorAction SilentlyContinue
    Set-Content -LiteralPath $request -Value $id
    schtasks.exe /run /tn $limitedTask | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "starting $limitedTask failed ($LASTEXITCODE)" }
    # Invoke-Winget's own limit, and a little.
    $deadline = (Get-Date).AddMinutes(32)
    while (-not (Test-Path -LiteralPath $result)) {
        if ((Get-Date) -gt $deadline) { throw "$limitedTask didn't finish" }
        Start-Sleep -Seconds 5
    }
    $outcome = (Get-Content -Raw -LiteralPath $result).Trim()
    if ($outcome -ne 'installed') { throw "unelevated: $outcome" }
}

# What explorer does with a PATH an installer changed, and the shell does
# too (shell/src/env.rs): it reloads the environment on this broadcast.
# Installers mostly send it themselves; this covers the ones that don't.
function Send-EnvironmentChanged {
    if (-not ('Defenestrate.Broadcast' -as [type])) {
        Add-Type -Namespace Defenestrate -Name Broadcast -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint msg, System.UIntPtr wParam, string lParam, uint flags, uint timeout, out System.UIntPtr result);
'@
    }
    $result = [System.UIntPtr]::Zero
    # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG: a hung window
    # mustn't hold up the installs.
    [void][Defenestrate.Broadcast]::SendMessageTimeout([System.IntPtr]0xffff, 0x1A, [System.UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
}

# The PCI vendor IDs of the machine's display controllers (PCI class 03),
# from every PnP device rather than Win32_VideoController so a GPU without a
# driver yet still counts.
function Get-GpuVendors {
    $vendors = foreach ($dev in Get-CimInstance Win32_PnPEntity) {
        if ($dev.DeviceID -match '^PCI\\VEN_([0-9A-F]{4})' -and ($dev.CompatibleID -match '^PCI\\CC_03')) {
            $Matches[1]
            Log "GPU: $($dev.Name) ($($dev.DeviceID))"
        }
    }
    @($vendors | Sort-Object -Unique)
}

# What Windows Update would install for the GPUs by itself sooner or later,
# now. BrowseOnly ones are the "optional updates" it wouldn't.
function Install-DisplayDrivers {
    $session = New-Object -ComObject Microsoft.Update.Session
    $found = $session.CreateUpdateSearcher().Search("IsInstalled=0 and Type='Driver'").Updates
    $updates = New-Object -ComObject Microsoft.Update.UpdateColl
    foreach ($u in $found) {
        if ($u.DriverClass -eq 'Display' -and -not $u.BrowseOnly) {
            if (-not $u.EulaAccepted) { $u.AcceptEula() }
            [void]$updates.Add($u)
            Log "  $($u.Title)"
        }
    }
    if ($updates.Count -eq 0) {
        Log '  Windows Update has no display driver for this machine'
        return
    }
    $downloader = $session.CreateUpdateDownloader()
    $downloader.Updates = $updates
    [void]$downloader.Download()
    $installer = $session.CreateUpdateInstaller()
    $installer.Updates = $updates
    $result = $installer.Install()
    # OperationResultCode: 2 is succeeded; 3, succeeded with errors, retries.
    if ($result.ResultCode -ne 2) { throw "Windows Update result code $($result.ResultCode)" }
    if ($result.RebootRequired) { Log '  needs a reboot to finish' }
}

function Find-Winget {
    $cmd = Get-Command winget.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    # App Installer is provisioned for every account, but a new account only
    # gets it registered a while after its first logon. This is Microsoft's
    # documented way to hurry that along.
    try { Add-AppxPackage -RegisterByFamilyName -MainPackage Microsoft.DesktopAppInstaller_8wekyb3d8bbwe } catch { }
    $alias = Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\winget.exe'
    if (Test-Path -LiteralPath $alias) { return $alias }
    return $null
}

if ($Limited) {
    try {
        $id = (Get-Content -Raw -LiteralPath $request).Trim()
        Log "$id"
        $winget = Find-Winget
        if (-not $winget) { throw 'winget not available' }
        Invoke-Winget $winget $id
        Log "${id}: installed"
        Set-Content -LiteralPath $result -Value 'installed'
    } catch {
        Log "failed: $_"
        Set-Content -LiteralPath $result -Value "$_"
    }
    exit
}

try {
    $plan = Get-Content -Raw -LiteralPath (Join-Path $here 'apps.json') | ConvertFrom-Json
    # Drivers first: the vendors' apps and the games want them in.
    $ids = @()
    if ($plan.windowsUpdateDisplayDrivers) { $ids += $wuItem }
    $gpus = Get-GpuVendors
    foreach ($v in @($plan.gpuVendors)) {
        if ($gpus -contains $v.pciVendor) { $ids += @($v.apps) }
    }
    $ids += @($plan.apps)
    $ids = @($ids | Select-Object -Unique)
    $done = @()
    if (Test-Path -LiteralPath $doneFile) { $done = @(Get-Content -LiteralPath $doneFile) }
    $pending = @($ids | Where-Object { $done -notcontains $_ })
    $elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    Log "starting as $([Security.Principal.WindowsIdentity]::GetCurrent().Name) (elevated: $elevated), $($pending.Count) of $($ids.Count) to install (GPU vendors: $($gpus -join ', '))"

    # winget needs the network and its own registration, and neither is a
    # given right at logon on a fresh machine, so keep trying for a while.
    $deadline = (Get-Date).AddMinutes(60)
    # Waiting covers a network or winget that isn't up yet; an installer
    # that failed three times will fail the same way a fourth, and retrying
    # it every minute only fills the log. The next logon tries again.
    $maxFailures = 3
    $failures = @{}
    $givenUp = @()
    while ($pending.Count -gt 0 -and (Get-Date) -lt $deadline) {
        $winget = Find-Winget
        if (-not $winget -and @($pending | Where-Object { $_ -ne $wuItem }).Count -gt 0) {
            Log 'winget not available yet, waiting'
            Start-Sleep -Seconds 30
            continue
        }
        $left = @()
        foreach ($id in $pending) {
            Log "$id"
            try {
                if ($id -eq $wuItem) {
                    Install-DisplayDrivers
                } else {
                    try {
                        Invoke-Winget $winget $id
                    } catch {
                        # Per-user installers can fail just for being elevated.
                        Log "${id}: failed elevated ($_), trying unelevated"
                        Invoke-Limited $id
                    }
                }
                Add-Content -LiteralPath $doneFile -Value $id
                Log "${id}: installed"
                Send-EnvironmentChanged
            } catch {
                $failures[$id] = 1 + $failures[$id]
                if ($failures[$id] -lt $maxFailures) {
                    Log "${id}: failed ($_), retrying"
                    $left += $id
                } else {
                    Log "${id}: failed ($_), $maxFailures times; giving up until the next logon"
                    $givenUp += $id
                }
            }
        }
        $pending = $left
        if ($pending.Count -gt 0) { Start-Sleep -Seconds 60 }
    }

    if ($pending.Count -eq 0 -and $givenUp.Count -eq 0) {
        schtasks.exe /delete /tn $taskName /f | Out-Null
        Log 'all installed, task removed'
    } else {
        Log "not installed: $((@($pending) + @($givenUp)) -join ', '); trying again at the next logon"
    }
} catch {
    Log "FAILED: $_"
}
