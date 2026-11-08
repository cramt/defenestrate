# Installs apps.json's apps, elevated, at each logon of the configured user
# until all of them are in, then deletes its own scheduled task. The image's
# first logon registers the task (nix/unattend.nix) and powers off, so the
# first time this runs is the machine's first real boot. It runs next to the
# shell, never in front of it.

$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$log = Join-Path (Split-Path $here) 'apps.log'
# One ID per line, so a rerun after a failure skips what already worked.
$doneFile = Join-Path $here 'done.txt'
$taskName = 'defenestrate-apps'

function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

# Start-Process -Wait has no timeout, and an installer stuck on a dialog
# nobody sees would hold up everything after it forever.
function Invoke-Installer([string]$file, [string]$arguments, [int[]]$okCodes) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $file
    $psi.Arguments = $arguments
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(30 * 60 * 1000)) {
        $p.Kill()
        throw 'still running after 30 minutes, killed it'
    }
    if ($okCodes -notcontains $p.ExitCode) { throw "exit code $($p.ExitCode)" }
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

# winget's "already installed": a rerun after the install worked but the
# done file didn't get written.
$wingetOk = @(0, -1978335189)

try {
    $plan = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $here 'apps.json') | ConvertFrom-Json
    $done = @()
    if (Test-Path -LiteralPath $doneFile) { $done = @(Get-Content -LiteralPath $doneFile) }
    $pending = @($plan | Where-Object { $done -notcontains $_.id })
    Log "starting, $($pending.Count) of $(@($plan).Count) apps to install"

    # Baked installers first: they need nothing but the disk.
    $online = @()
    foreach ($app in $pending) {
        if (-not $app.baked) { $online += $app; continue }
        $b = $app.baked
        $file = Join-Path $here "installers\$($b.file)"
        Log "$($app.id) $($b.version): $($b.installerType) installer"
        try {
            if ($b.installerType -eq 'msi' -or $b.installerType -eq 'wix') {
                $msiLog = Join-Path $here "$($app.id).msi.log"
                Invoke-Installer 'msiexec.exe' "/i `"$file`" $($b.args) /l*v `"$msiLog`"" $b.successCodes
            } else {
                Invoke-Installer $file $b.args $b.successCodes
            }
            Add-Content -LiteralPath $doneFile -Value $app.id
            Log "$($app.id): installed"
        } catch {
            Log "$($app.id): FAILED: $_"
        }
    }

    # winget needs the network and its own registration, and neither is a
    # given right at logon on a fresh machine, so keep trying for a while.
    $deadline = (Get-Date).AddMinutes(60)
    while ($online.Count -gt 0 -and (Get-Date) -lt $deadline) {
        $winget = Find-Winget
        if (-not $winget) {
            Log 'winget not available yet, waiting'
            Start-Sleep -Seconds 30
            continue
        }
        $left = @()
        foreach ($app in $online) {
            $common = '--silent --accept-package-agreements --accept-source-agreements --disable-interactivity'
            if ($app.winget) {
                $what = "$($app.id) $($app.winget.version) from winget"
                $arguments = "install --id `"$($app.id)`" --exact --source winget --version `"$($app.winget.version)`" $common"
            } else {
                $what = "$($app.id) from the Microsoft Store"
                $arguments = "install --id `"$($app.id)`" --exact --source msstore $common"
            }
            Log $what
            try {
                Invoke-Installer $winget $arguments $wingetOk
                Add-Content -LiteralPath $doneFile -Value $app.id
                Log "$($app.id): installed"
            } catch {
                Log "$($app.id): failed ($_), retrying"
                $left += $app
            }
        }
        $online = $left
        if ($online.Count -gt 0) { Start-Sleep -Seconds 60 }
    }

    $done = @(Get-Content -LiteralPath $doneFile -ErrorAction SilentlyContinue)
    $missing = @($plan | Where-Object { $done -notcontains $_.id } | ForEach-Object { $_.id })
    if ($missing.Count -eq 0) {
        schtasks.exe /delete /tn $taskName /f | Out-Null
        Log 'all apps installed, task removed'
    } else {
        Log "not installed: $($missing -join ', '); trying again at the next logon"
    }
} catch {
    Log "FAILED: $_"
}
