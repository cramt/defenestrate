# The distro itself: everything here is something stock Windows does that we
# don't. Machine-specific settings (user, disk) belong in a per-machine config.
{ packages, pkgs, ... }:

{
  # Explorer is still around as a program (`explorer .` opens a file
  # window); it's just not the shell anymore.
  shell.package = packages.shell;

  # The Mono variant: conhost only offers fixed-pitch fonts, and the plain
  # Nerd Font has double-width icons that aren't. The shell's bar draws with
  # this family too (FONT_FACE in shell/src/main.rs).
  fonts = map (style: "${pkgs.nerd-fonts.iosevka}/share/fonts/truetype/NerdFonts/Iosevka/IosevkaNerdFontMono-${style}.ttf")
    [ "Regular" "Bold" "Italic" "BoldItalic" ];

  registry.system = {
    # 24H2+ silently BitLockers the system drive on first sign-in and escrows
    # the recovery key to a Microsoft account. We don't have one.
    "CurrentControlSet\\Control\\BitLocker".PreventDeviceEncryption.dword = 1;

    # Keep the RTC in UTC like every other OS, so dual-booting Linux doesn't
    # shift the clock by the UTC offset every boot.
    "CurrentControlSet\\Control\\TimeZoneInformation".RealTimeIsUniversal.dword = 1;

    # Fast Startup is hibernation in disguise: it leaves NTFS marked in use,
    # and Linux then refuses to mount it read-write.
    "CurrentControlSet\\Control\\Session Manager\\Power".HiberbootEnabled.dword = 0;
  };

  registry.software = {
    # Conhost's font picker only lists TrueType faces named here.
    "Microsoft\\Windows NT\\CurrentVersion\\Console\\TrueTypeFont"."000".sz = "Iosevka NFM";

    # Pro clamps 0 ("Security") up to 1 ("Required"); only Enterprise/Education
    # honour 0. Still the lowest this edition goes.
    "Policies\\Microsoft\\Windows\\DataCollection".AllowTelemetry.dword = 0;

    "Policies\\Microsoft\\Windows\\CloudContent".DisableWindowsConsumerFeatures.dword = 1;

    # Recall.
    "Policies\\Microsoft\\Windows\\WindowsAI".DisableAIDataAnalysis.dword = 1;

    "Policies\\Microsoft\\Windows\\WindowsUpdate\\AU".NoAutoRebootWithLoggedOnUsers.dword = 1;
  };

  registry.defaultUser = {
    # The Default profile ships with this Run entry, so every new account
    # installs OneDrive on first sign-in.
    "Software\\Microsoft\\Windows\\CurrentVersion\\Run".OneDriveSetup = null;

    # Silently installed "suggested" apps and the ads in Start / lock screen.
    "Software\\Microsoft\\Windows\\CurrentVersion\\ContentDeliveryManager" = {
      OemPreInstalledAppsEnabled.dword = 0;
      PreInstalledAppsEnabled.dword = 0;
      SilentInstalledAppsEnabled.dword = 0;
      SoftLandingEnabled.dword = 0;
      SystemPaneSuggestionsEnabled.dword = 0;
      RotatingLockScreenOverlayEnabled.dword = 0;
    };

    # Bing results in local search.
    "Software\\Policies\\Microsoft\\Windows\\Explorer".DisableSearchBoxSuggestions.dword = 1;

    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced".HideFileExt.dword = 0;

    # Every console window's defaults. FontFamily 0x36 is TMPF_TRUETYPE |
    # FF_MODERN; FontSize is the cell height in the high word.
    "Console" = {
      FaceName.sz = "Iosevka NFM";
      FontFamily.dword = 54;
      FontWeight.dword = 400;
      FontSize.dword = 18 * 65536;
    };
    # Windows 11 hands every new console to Windows Terminal once its
    # package is registered for the user ("Let Windows decide"), and
    # Terminal ignores all of the above (it came up in Cascadia Mono). This
    # GUID is conhost, the "Windows Console Host" choice in that setting.
    "Console\\%%Startup" = {
      DelegationConsole.sz = "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}";
      DelegationTerminal.sz = "{B23D10C0-E52E-411E-9D5B-C09FDF709C7D}";
    };
    # powershell.exe's own subkeys pin Lucida Console over the defaults above.
    "Console\\%SystemRoot%_System32_WindowsPowerShell_v1.0_powershell.exe".FaceName = null;
    "Console\\%SystemRoot%_SysWOW64_WindowsPowerShell_v1.0_powershell.exe".FaceName = null;
  };
}
