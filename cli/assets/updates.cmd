@echo off
rem In setup's WinPE, after winpeshl.ini's `setup.exe /noreboot` has applied
rem the image: adds the config's `updates` to the still offline Windows with
rem DISM, the way an updated image is serviced, then reboots into specialize.
rem On a failure it shuts down instead, which `build image` reads as a failed
rem install; updates.log next to firstlogon.log says why. See iso.rs.
setlocal enabledelayedexpansion

set media=
set target=
for %%d in (C D E F G H I J K L M N O P Q R S T U V W Y Z) do (
  if not defined media if exist %%d:\defenestrate\updates\ set media=%%d:
  rem Our payload, so never WinPE's own X:.
  if not defined target if exist %%d:\Windows\Setup\defenestrate\ set target=%%d:
)
if not defined target (
  echo defenestrate: setup left no Windows to update
  wpeutil shutdown
  exit /b 1
)
set log=%target%\Windows\Setup\defenestrate\updates.log
if not defined media (
  echo no defenestrate\updates on any drive>>"%log%"
  wpeutil shutdown
  exit /b 1
)

rem WinPE's own scratch space is a 32 MB RAM disk; a cumulative update
rem needs gigabytes.
set scratch=%target%\defenestrate-dism
mkdir "%scratch%"
for /f "delims=" %%f in ('dir /b /on "%media%\defenestrate\updates\*.msu"') do (
  echo %%f>>"%log%"
  dism /English /Image:%target%\ /Add-Package /PackagePath:"%media%\defenestrate\updates\%%f" /ScratchDir:"%scratch%" /LogPath:"%target%\Windows\Setup\defenestrate\dism-%%~nf.log" >>"%log%" 2>&1
  set code=!errorlevel!
  rem 0x800F081E: not applicable, e.g. a checkpoint the ISO is already past.
  if "!code!"=="-2146498530" (
    echo   not applicable to this build, skipped>>"%log%"
  ) else if not "!code!"=="0" if not "!code!"=="3010" (
    echo   failed: dism exited with !code!>>"%log%"
    wpeutil shutdown
    exit /b 1
  )
)
rmdir /s /q "%scratch%"
echo all installed>>"%log%"
wpeutil reboot
