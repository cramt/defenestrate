# Runs once at first logon, elevated in the user's session (FirstLogonCommands).
# Registers the on-demand task defenestrate-reboot-to-linux, and puts a
# "Reboot to Linux" shortcut that starts it in the all-users Start Menu,
# where the shell's launcher finds it.
#
# The task is registered by the user it runs as, and Task Scheduler lets a
# task's own user start it without elevation even though it runs with
# highest privileges, so the shortcut needs no UAC prompt.
param([Parameter(Mandatory)][string]$task)
$ErrorActionPreference = 'Stop'
schtasks.exe /create /tn $task /xml (Join-Path $PSScriptRoot 'task.xml') /f
if ($LASTEXITCODE -ne 0) { throw "schtasks /create exited with $LASTEXITCODE" }

$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'Reboot to Linux.lnk'))
# schtasks is a console program; conhost --headless gives it a console that
# never shows, rather than one flashing up over whatever has focus.
$lnk.TargetPath = Join-Path $env:SystemRoot 'System32\conhost.exe'
$lnk.Arguments = "--headless $env:SystemRoot\System32\schtasks.exe /run /tn $task"
$lnk.Description = 'Boot the Linux entry once and reboot. Log: %LOCALAPPDATA%\defenestrate\reboot.log'
$lnk.Save()
Write-Output "registered $task and the Reboot to Linux shortcut"
