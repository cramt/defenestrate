# Runs at each logon of the configured user, elevated, from the scheduled
# task defenestrate-wsl (registered at first logon). Puts the machine's Linux
# install within reach of WSL:
#   - imports the defenestrate distro (busybox and dfn) the first time,
#   - attaches the install's disks to WSL (`wsl --mount --bare` needs admin,
#     which is why this is a task rather than something the shell does),
#   - mounts the install inside WSL (dfn mount),
#   - keeps one Start Menu shortcut per Linux app, under Programs\Linux.
# Log: %LOCALAPPDATA%\defenestrate\wsl.log
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$state = Join-Path $env:LOCALAPPDATA 'defenestrate'
New-Item -ItemType Directory -Force -Path $state | Out-Null
$log = Join-Path $state 'wsl.log'
function Log([string]$msg) {
    Add-Content -LiteralPath $log -Value ('{0:yyyy-MM-dd HH:mm:ss} {1}' -f (Get-Date), $msg)
}

# wsl.exe writes UTF-16 unless told otherwise; dfn's output is UTF-8.
$env:WSL_UTF8 = '1'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$wsl = Join-Path $env:ProgramFiles 'WSL\wsl.exe'
$wslg = Join-Path $env:ProgramFiles 'WSL\wslg.exe'
$distro = 'defenestrate'
$config = Get-Content -Raw (Join-Path $here 'wsl.json') | ConvertFrom-Json

# Runs wsl.exe, logging what it says; returns its output lines. Its exit
# code decides failure: Windows PowerShell turns any stderr line from a
# native command into a terminating error under "Stop", and WSL warns there.
function Wsl([string[]]$arguments, [switch]$AllowFailure) {
    $ErrorActionPreference = 'Continue'
    $out = & $wsl @arguments 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    foreach ($line in $out) { if ($line.Trim()) { Log "  $line" } }
    if ($code -ne 0 -and -not $AllowFailure) { throw "wsl $($arguments -join ' ') exited with $code" }
    return $out
}

try {
    Log 'logon'
    $registered = Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' -ErrorAction SilentlyContinue |
        Where-Object { (Get-ItemProperty $_.PSPath).DistributionName -eq $distro }
    if (-not $registered) {
        Log "importing the $distro distro"
        $dir = Join-Path $state 'wsl'
        Wsl @('--import', $distro, $dir, (Join-Path $here 'rootfs.tar'), '--version', '2') | Out-Null
    }

    foreach ($guid in $config.disks) {
        $disk = Get-Disk | Where-Object { $_.Guid -and $_.Guid.Trim('{}') -eq $guid }
        if (-not $disk) {
            Log "no disk with GPT GUID $guid"
            continue
        }
        Log "attaching disk $($disk.Number) ($($disk.FriendlyName), $guid)"
        # Fails when it's already attached (a second logon without a reboot);
        # dfn mount below says whether everything's there.
        Wsl @('--mount', "\\.\PHYSICALDRIVE$($disk.Number)", '--bare') -AllowFailure | Out-Null
    }

    Log 'mounting'
    # Only a check that the install is reachable: WSL stops an idle distro
    # within seconds, mounts and all, so every launch mounts again.
    Wsl @('-d', $distro, '--cd', '/', '--exec', '/defenestrate/dfn', 'mount') | Out-Null

    # One shortcut per desktop entry the Linux user would see. The shell's
    # launcher lists the Start Menu, so they're launchable from there.
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Linux'
    New-Item -ItemType Directory -Force -Path $menu | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $wanted = @{}
    foreach ($line in (Wsl @('-d', $distro, '--cd', '/', '--exec', '/defenestrate/dfn', 'apps'))) {
        $id, $name = "$line" -split "`t", 2
        if (-not $name) { continue }
        $file = ($name -replace '[<>:"/\\|?*]', '_') + '.lnk'
        if ($wanted.ContainsKey($file)) { continue }
        $wanted[$file] = $true
        $lnk = $shell.CreateShortcut((Join-Path $menu $file))
        # wslg.exe rather than wsl.exe: no console window. It has no --exec;
        # --shell-type none is the same.
        $lnk.TargetPath = $wslg
        $lnk.Arguments = "-d $distro --cd / --shell-type none -- /defenestrate/dfn launch $id"
        $lnk.Description = "$name (Linux)"
        $lnk.Save()
    }
    # The user's login shell in a console, for terminal programs (which have
    # no desktop entry to show up as).
    $file = 'Linux terminal.lnk'
    $wanted[$file] = $true
    $lnk = $shell.CreateShortcut((Join-Path $menu $file))
    $lnk.TargetPath = $wsl
    $lnk.Arguments = "-d $distro --cd / --exec /defenestrate/dfn shell"
    $lnk.Description = 'Login shell of the Linux install'
    $lnk.Save()
    Get-ChildItem $menu -Filter *.lnk | Where-Object { -not $wanted.ContainsKey($_.Name) } | Remove-Item
    Log "$($wanted.Count) Linux apps in the Start Menu"
} catch {
    Log "failed: $_"
    exit 1
}
