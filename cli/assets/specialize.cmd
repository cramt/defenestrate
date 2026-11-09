@echo off
rem Runs once in the specialize pass, before OOBE and before any user profile
rem exists. Windows applies our registry config itself, so the hives keep
rem their ACLs (editing them offline from Linux would strip those).
setlocal
set here=%~dp0

reg import "%here%software.reg" || exit /b 1
reg import "%here%system.reg" || exit /b 1

reg load HKU\DefenestrateDefault "%SystemDrive%\Users\Default\NTUSER.DAT" || exit /b 1
reg import "%here%defaultUser.reg"
set rc=%errorlevel%
reg unload HKU\DefenestrateDefault
if %rc% neq 0 exit /b %rc%

rem Last, so a package that won't go can't keep the registry config from applying.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%here%remove-appx.ps1"
