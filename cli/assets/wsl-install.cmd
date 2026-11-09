@echo off
rem Runs at first logon (the image build's, in its VM), elevated. Installs WSL
rem from the MSI baked into the image, so the machine needs no network for
rem it, and registers the logon task that sets up the Linux install, if the
rem config has one. The distro itself is imported by that task: that needs
rem WSL's VM running, which the image build's VM can't be relied on for.
setlocal
set here=%~dp0
start /wait msiexec /i "%here%wsl.msi" /qn /norestart /l*v "%here%wsl-install.log"
echo WSL msiexec exited with %errorlevel%
if exist "%here%task.xml" schtasks /create /tn defenestrate-wsl /xml "%here%task.xml"
exit /b 0
