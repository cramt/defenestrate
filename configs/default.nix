# The distro itself: everything here is something stock Windows does that we
# don't. Machine-specific settings (user, disk) belong in a per-machine config.
{ ... }:

{
  # Placeholder until the Rust shell exists. Explorer is still around as a
  # program (`explorer .` opens a file window); it's just not the shell.
  shell = "cmd.exe";

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
  };
}
