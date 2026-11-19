# Runs at each logon of the configured user, elevated, from the scheduled
# task defenestrate-wsl (registered at first logon). Puts the machine's Linux
# install within reach of WSL:
#   - keeps WSL from stopping the distro when it's idle (.wslconfig),
#   - imports the defenestrate distro (busybox and dfn) the first time, and
#     again whenever rootfs.tar has changed,
#   - attaches the install's disks to WSL (`wsl --mount --bare` needs admin,
#     which is why this is a task rather than something the shell does),
#   - mounts the install inside WSL (dfn mount),
#   - copies wsl.linux.files from the install into the user's profile,
#   - keeps the "Linux terminal" Start Menu shortcut, under Programs\Linux.
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

# The install's $from into the profile at $to, replacing what's there. As a
# tar stream (dfn pack): PowerShell decodes a native command's output as
# text, which would mangle bytes and can't carry a directory anyway.
function Copy-FromLinux([string]$from, [string]$to) {
    $tmp = Join-Path $state ('copy-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $psi = New-Object System.Diagnostics.ProcessStartInfo $wsl
        $psi.Arguments = "-d $distro -u root --cd / --exec /defenestrate/dfn pack `"$from`""
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $p = [System.Diagnostics.Process]::Start($psi)
        $stderr = $p.StandardError.ReadToEndAsync()
        $tar = [System.IO.File]::Create("$tmp.tar")
        try { $p.StandardOutput.BaseStream.CopyTo($tar) } finally { $tar.Close() }
        $p.WaitForExit()
        if ($p.ExitCode -ne 0) { throw "dfn pack exited with $($p.ExitCode): $($stderr.Result.Trim())" }
        & (Join-Path $env:SystemRoot 'System32\tar.exe') -xf "$tmp.tar" -C $tmp
        if ($LASTEXITCODE -ne 0) { throw "tar.exe couldn't unpack it ($LASTEXITCODE)" }
        $dest = Join-Path $env:USERPROFILE $to
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest) | Out-Null
        if (Test-Path -LiteralPath $dest) { Remove-Item -LiteralPath $dest -Recurse -Force }
        Move-Item -LiteralPath (Join-Path $tmp ($from -split '/')[-1]) -Destination $dest
    } finally {
        Remove-Item -LiteralPath $tmp, "$tmp.tar" -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# Sets one key in the user's .wslconfig, keeping the rest of the file.
function Set-WslConfig([string]$section, [string]$key, [string]$value) {
    $path = Join-Path $env:USERPROFILE '.wslconfig'
    $lines = [System.Collections.Generic.List[string]]::new()
    if (Test-Path -LiteralPath $path) { $lines.AddRange([string[]](Get-Content -LiteralPath $path)) }
    $in = $false; $start = -1; $end = $lines.Count
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^\s*\[(.+)\]\s*$') {
            if ($in) { $end = $i; break }
            $in = $Matches[1].Trim() -eq $section
            if ($in) { $start = $i }
        } elseif ($in -and $lines[$i] -match "^\s*$([regex]::Escape($key))\s*=") {
            if ($lines[$i].Trim() -eq "$key=$value") { return }
            $lines[$i] = "$key=$value"
            Set-Content -LiteralPath $path -Value $lines
            Log ".wslconfig: $section.$key=$value"
            return
        }
    }
    if ($start -lt 0) {
        $lines.Add("[$section]")
        $lines.Add("$key=$value")
    } else {
        $lines.Insert($end, "$key=$value")
    }
    Set-Content -LiteralPath $path -Value $lines
    Log ".wslconfig: $section.$key=$value"
}

try {
    Log 'logon'
    # WSL stops a distro seconds after its last process exits, mounts and
    # all, and the VM a minute later, which detaches the disks `wsl --mount`
    # attached: the install would vanish from \\wsl$ until the next logon.
    # Before any wsl call, since WSL reads it when it starts.
    Set-WslConfig 'general' 'instanceIdleTimeout' '-1'
    $registered = Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' -ErrorAction SilentlyContinue |
        Where-Object { (Get-ItemProperty $_.PSPath).DistributionName -eq $distro }
    # rootfs.tar is the distro's source of truth: `dfnstrate update` can
    # replace it from Linux, and nothing in the distro is worth keeping (it's
    # busybox, dfn and the mount list), so a changed one is imported afresh.
    $rootfs = Join-Path $here 'rootfs.tar'
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $rootfs).Hash
    $stamp = Join-Path $state 'rootfs.sha256'
    $imported = if (Test-Path -LiteralPath $stamp) { (Get-Content -Raw -LiteralPath $stamp).Trim() } else { '' }
    if ($registered -and $imported -ne $hash) {
        Log "rootfs.tar changed, re-importing the $distro distro"
        Wsl @('--unregister', $distro) | Out-Null
        $registered = $null
    }
    if (-not $registered) {
        Log "importing the $distro distro"
        $dir = Join-Path $state 'wsl'
        Wsl @('--import', $distro, $dir, $rootfs, '--version', '2') | Out-Null
        Set-Content -LiteralPath $stamp -Value $hash -NoNewline
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
    # The distro's boot command (dfn boot) mounted already if the disks were
    # attached when it started; this covers the first logon, where they
    # weren't. Every launch mounts again too, in case WSL restarted it.
    Wsl @('-d', $distro, '-u', 'root', '--cd', '/', '--exec', '/defenestrate/dfn', 'mount') | Out-Null
    # The distro's default user is the Linux user, so what Windows writes over
    # \\wsl$ is theirs rather than root's. Its uid is the install's, which dfn
    # mount copies into the distro's passwd, leaving /defenestrate/user-added
    # when it does. \\wsl$ takes its user once, as the instance starts, so an
    # instance that started before that (a fresh import) serves it as root
    # until restarted. Not `id -u`: a launch looks the default user up anew,
    # so it said 1000 while \\wsl$ wrote as root (saturn, 2026-11-19).
    $added = Wsl @('-d', $distro, '-u', 'root', '--cd', '/', '--exec', '/bin/busybox', 'sh', '-c', 'test -e /defenestrate/user-added && echo added || true')
    if ($added | Where-Object { "$_".Trim() -eq 'added' }) {
        Log "restarting the $distro distro so \\wsl$ runs as the Linux user"
        Wsl @('-d', $distro, '-u', 'root', '--cd', '/', '--exec', '/bin/busybox', 'rm', '/defenestrate/user-added') | Out-Null
        Wsl @('--terminate', $distro) -AllowFailure | Out-Null
        Wsl @('-d', $distro, '-u', 'root', '--cd', '/', '--exec', '/defenestrate/dfn', 'mount') | Out-Null
    }

    # Only the terminal: WSLg's windows never show under our shell (README,
    # "Linux GUI windows are a non-goal"), so GUI apps would be dead entries.
    # Anything else in the folder, like the per-app shortcuts older versions
    # wrote, is removed below.
    $menu = Join-Path ([Environment]::GetFolderPath('Programs')) 'Linux'
    New-Item -ItemType Directory -Force -Path $menu | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $wanted = @{}
    # The user's login shell in a console, for terminal programs (which have
    # no desktop entry to show up as).
    $file = 'Linux terminal.lnk'
    $wanted[$file] = $true
    $lnk = $shell.CreateShortcut((Join-Path $menu $file))
    $lnk.TargetPath = $wsl
    $lnk.Arguments = "-d $distro -u root --cd / --exec /defenestrate/dfn shell"
    $lnk.Description = 'Login shell of the Linux install'
    $lnk.Save()
    Get-ChildItem $menu -Filter *.lnk | Where-Object { -not $wanted.ContainsKey($_.Name) } | Remove-Item
    Log 'Linux terminal in the Start Menu'

    # One failing (a secret Linux hasn't rendered yet) leaves its last copy
    # and doesn't hold up the rest.
    $copied = Join-Path $state 'linux-files.txt'
    $wantedFiles = @($config.files | ForEach-Object { $_.to })
    foreach ($f in @($config.files)) {
        try {
            Copy-FromLinux $f.from $f.to
            Log "copied $($f.from) to ~\$($f.to)"
        } catch {
            Log "couldn't copy $($f.from) to ~\$($f.to): $_"
        }
    }
    # What earlier logons copied that the config no longer names.
    if (Test-Path -LiteralPath $copied) {
        foreach ($old in (Get-Content -LiteralPath $copied)) {
            if ($old -and $wantedFiles -notcontains $old) {
                Remove-Item -LiteralPath (Join-Path $env:USERPROFILE $old) -Recurse -Force -ErrorAction SilentlyContinue
                Log "removed ~\$old, no longer configured"
            }
        }
    }
    Set-Content -LiteralPath $copied -Value $wantedFiles
} catch {
    Log "failed: $_"
    exit 1
}
