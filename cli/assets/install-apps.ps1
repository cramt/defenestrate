# Installs the config's winget apps (apps.json, IDs only, no versions) at
# each logon of the configured user until all are in, then deletes its own
# scheduled task (defenestrate-apps, registered at first logon). On an image,
# that first logon is the build's offline VM, so the first time this runs is
# the machine's first real boot. Runs elevated in the user's session.
# Log: %LOCALAPPDATA%\defenestrate\apps.log
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'apps.log'
# One ID per line: a rerun after a failure skips what already worked.
$doneFile = Join-Path $state 'apps-done.txt'
$taskName = 'defenestrate-apps'

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
    $ids = @(Get-Content -Raw -LiteralPath (Join-Path $here 'apps.json') | ConvertFrom-Json)
    $done = @()
    if (Test-Path -LiteralPath $doneFile) { $done = @(Get-Content -LiteralPath $doneFile) }
    $pending = @($ids | Where-Object { $done -notcontains $_ })
    Log "starting, $($pending.Count) of $($ids.Count) apps to install"

    # winget needs the network and its own registration, and neither is a
    # given right at logon on a fresh machine, so keep trying for a while.
    $deadline = (Get-Date).AddMinutes(60)
    while ($pending.Count -gt 0 -and (Get-Date) -lt $deadline) {
        $winget = Find-Winget
        if (-not $winget) {
            Log 'winget not available yet, waiting'
            Start-Sleep -Seconds 30
            continue
        }
        $left = @()
        foreach ($id in $pending) {
            Log "$id"
            try {
                Invoke-Winget $winget "install --id `"$id`" --exact --source winget --silent --accept-package-agreements --accept-source-agreements --disable-interactivity"
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
        Log 'all apps installed, task removed'
    } else {
        Log "not installed: $($pending -join ', '); trying again at the next logon"
    }
} catch {
    Log "FAILED: $_"
}
