# Installs the config's GPU drivers and winget apps (apps.json, IDs only, no
# versions) at each logon of the configured user until all are in, then
# deletes its own
# scheduled task (defenestrate-apps, registered at first logon). On an image,
# that first logon is the build's offline VM, so the first time this runs is
# the machine's first real boot. Runs elevated in the user's session.
# Log: %LOCALAPPDATA%\defenestrate\apps.log
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'apps.log'
# One item per line, a winget ID or $wuItem: a rerun after a failure skips
# what already worked.
$doneFile = Join-Path $state 'apps-done.txt'
$taskName = 'defenestrate-apps'
# Can't clash with a winget ID, which has no colon.
$wuItem = 'windows-update:display-drivers'

function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

# Start-Process -Wait has no timeout, and an installer stuck on a dialog
# nobody sees would hold up everything after it forever.
function Invoke-Winget([string]$winget, [string]$arguments) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $winget
    $psi.Arguments = $arguments
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(30 * 60 * 1000)) {
        $p.Kill()
        throw 'still running after 30 minutes, killed it'
    }
    # 0x8A15002B: already installed, e.g. a rerun after the install worked
    # but the done file didn't get written.
    if (@(0, -1978335189) -notcontains $p.ExitCode) { throw ('exit code 0x{0:X8}' -f $p.ExitCode) }
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
    Log "starting, $($pending.Count) of $($ids.Count) to install (GPU vendors: $($gpus -join ', '))"

    # winget needs the network and its own registration, and neither is a
    # given right at logon on a fresh machine, so keep trying for a while.
    $deadline = (Get-Date).AddMinutes(60)
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
                if ($id -eq $wuItem) { Install-DisplayDrivers } else { Invoke-Winget $winget "install --id `"$id`" --exact --source winget --silent --accept-package-agreements --accept-source-agreements --disable-interactivity" }
                Add-Content -LiteralPath $doneFile -Value $id
                Log "${id}: installed"
            } catch {
                Log "${id}: failed ($_), retrying"
                $left += $id
            }
        }
        $pending = $left
        if ($pending.Count -gt 0) { Start-Sleep -Seconds 60 }
    }

    if ($pending.Count -eq 0) {
        schtasks.exe /delete /tn $taskName /f | Out-Null
        Log 'all installed, task removed'
    } else {
        Log "not installed: $($pending -join ', '); trying again at the next logon"
    }
} catch {
    Log "FAILED: $_"
}
