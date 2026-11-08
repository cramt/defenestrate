{ lib, config, pkgs, ... }:

let
  inherit (lib) mkOption types;

  # Microsoft's published generic install keys. They select an edition and
  # cannot activate anything; activation comes from the machine's digital
  # license. Editions without a known key aren't offered.
  genericKeys = {
    "Windows 11 Home" = "YTMG3-N6DKC-DKB77-7M9GH-8HVX7";
    "Windows 11 Pro" = "VK7JG-NPHTM-C97JM-9MPGT-3V66T";
    "Windows 11 Education" = "YNMGQ-8RYV3-4PGQ3-C8XTP-7CFBY";
    "Windows 11 Pro for Workstations" = "DXG7C-N36C4-C4HTG-X4T3X-2YV77";
  };

  regValue = types.attrTag {
    sz = mkOption { type = types.str; };
    expandSz = mkOption { type = types.str; };
    multiSz = mkOption { type = types.listOf types.str; };
    dword = mkOption { type = types.ints.u32; };
    qword = mkOption { type = types.ints.unsigned; };
    binary = mkOption { type = types.listOf types.ints.u8; };
  };

  # key path (backslash separated, relative to the hive root) -> value name
  # ("" is the default value) -> value, or null to delete the value.
  regHive = description: mkOption {
    type = types.attrsOf (types.attrsOf (types.nullOr regValue));
    default = { };
    inherit description;
  };

  windowsName = types.strMatching "[A-Za-z0-9-]{1,15}";

  appIds = description: mkOption {
    type = types.listOf (types.strMatching "[A-Za-z0-9][A-Za-z0-9.+_-]*");
    default = [ ];
    inherit description;
  };

  # apps.lock.json's shape, as tools/lock-apps writes it. Checked here too so
  # a hand-edited lock fails evaluation instead of the machine.
  lockedApp = types.attrTag {
    baked = mkOption {
      type = types.submodule {
        options = {
          version = mkOption { type = types.str; };
          url = mkOption { type = types.strMatching "https://.*"; };
          hash = mkOption { type = types.strMatching "sha256-[A-Za-z0-9+/]{43}="; };
          file = mkOption { type = types.strMatching "[A-Za-z0-9+._?=-]+"; };
          installerType = mkOption { type = types.enum [ "msi" "wix" "nullsoft" "inno" "burn" "exe" ]; };
          args = mkOption { type = types.str; };
          successCodes = mkOption { type = types.nonEmptyListOf types.int; };
        };
      };
    };
    winget = mkOption {
      type = types.submodule { options.version = mkOption { type = types.str; }; };
    };
    msstore = mkOption { type = types.submodule { }; };
  };
in
{
  options = {
    source = mkOption {
      type = types.package;
      description = "Stock Microsoft Windows 11 install ISO.";
    };

    edition = mkOption {
      type = types.enum (builtins.attrNames genericKeys);
      default = "Windows 11 Pro";
    };

    language = mkOption {
      type = types.str;
      default = "en-US";
      description = "Setup and OS UI language. Must be a language the source ISO ships.";
    };

    inputLocale = mkOption {
      type = types.str;
      default = "0409:00000409";
      description = "Keyboard layout(s), e.g. 0406:00000406 for Danish.";
    };

    timeZone = mkOption {
      type = types.str;
      default = "UTC";
      description = "Windows time zone ID (tzutil /l), not an IANA name.";
    };

    computerName = mkOption {
      type = windowsName;
      default = "defenestrated";
    };

    user = {
      name = mkOption { type = windowsName; };
      password = mkOption {
        type = types.str;
        default = "";
        description = ''
          Plaintext, and it ends up in the world-readable nix store and on the
          ISO. Fine for a VM; change it after install on anything real.
        '';
      };
      autoLogon = mkOption {
        type = types.bool;
        default = false;
      };
    };

    disk = mkOption {
      type = types.attrTag {
        interactive = mkOption {
          type = types.submodule { };
          description = "Setup shows its disk picker.";
        };
        wipe = mkOption {
          type = types.submodule {
            options.diskId = mkOption { type = types.ints.unsigned; };
          };
          description = "Setup erases this disk without asking and installs to it (UEFI/GPT).";
        };
      };
      default.interactive = { };
    };

    installTarget = mkOption {
      # by-id only: kernel names (nvme0n1, sda) reorder between boots, and
      # this is the disk build.install erases.
      type = types.nullOr (types.strMatching "/dev/disk/by-id/[^/]+");
      default = null;
      example = "/dev/disk/by-id/nvme-Samsung_SSD_970_EVO_1TB_S5H9NS1NB05355E";
      description = ''
        Whole disk `nix run .#…build.install` erases and installs onto when
        given no argument. Ignored by the ISO, which goes by `disk`.
      '';
    };

    bootPrompt = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Keep "Press any key to boot from CD or DVD". Turning it off makes the
        ISO boot straight into setup, which is only safe when the disk comes
        first in the boot order.
      '';
    };

    shell = mkOption {
      type = types.attrTag {
        command = mkOption {
          type = types.str;
          description = "Command line of something already on the system, e.g. cmd.exe.";
        };
        package = mkOption {
          type = types.package;
          description = ''
            Its bin/ is installed to C:\Program Files\defenestrate, and its
            meta.mainProgram becomes the shell.
          '';
        };
      };
      description = "What Winlogon starts as the user's shell instead of explorer.exe.";
    };

    fonts = mkOption {
      type = types.listOf types.path;
      default = [ ];
      example = lib.literalExpression ''[ "''${pkgs.nerd-fonts.iosevka}/share/fonts/truetype/NerdFonts/Iosevka/IosevkaNerdFontMono-Regular.ttf" ]'';
      description = ''
        TrueType/OpenType files installed for everyone into C:\Windows\Fonts.
        File names must be unique there: an existing file can't be replaced
        safely (see the README on ACLs).
      '';
    };

    apps = appIds ''
      winget package IDs whose installers are baked into the image and
      installed offline on the machine's first real boot. Each must be
      locked in appsLock (`nix run .#lock-apps -- ID`).
    '';
    wingetApps = appIds ''
      winget package IDs that can't be baked (bootstrappers, installers
      behind an unversioned URL): winget installs the locked version on the
      first real boot, which needs the network. `nix run .#lock-apps -- --winget ID`.
    '';
    storeApps = appIds ''
      Microsoft Store product IDs (9N…), installed by winget on the first
      real boot, online. The store only serves the current version.
      `nix run .#lock-apps -- --msstore ID`.
    '';
    appsLock = mkOption {
      type = types.path;
      default = ../apps.lock.json;
      description = "Lockfile tools/lock-apps writes; pins what apps, wingetApps and storeApps install.";
    };
    lockedApps = mkOption {
      type = types.attrsOf lockedApp;
      internal = true;
      readOnly = true;
    };

    registry = {
      software = regHive "HKLM\\SOFTWARE.";
      system = regHive ''
        HKLM\SYSTEM. Applied to the running system at install time, so
        CurrentControlSet paths work.
      '';
      defaultUser = regHive "HKCU of every account created after install (the Default profile).";
    };

    build = mkOption {
      type = types.lazyAttrsOf types.package;
      internal = true;
      readOnly = true;
    };
  };

  config = {
    lockedApps = if builtins.pathExists config.appsLock then (lib.importJSON config.appsLock).apps else { };

    # 22H2+ OOBE stops at "connect to a network" without this. The interactive
    # escapes (bypassnro.cmd, ms-cxh:localonly) are gone since 24H2; the
    # registry value still works. Lesson from nixconf's saturn-windows-image.sh.
    registry.software."Microsoft\\Windows\\CurrentVersion\\OOBE".BypassNRO.dword = 1;

    # A file in C:\Windows\Fonts only exists to GDI once it's listed here.
    # The value name is just a label; the data is the file name.
    registry.software."Microsoft\\Windows NT\\CurrentVersion\\Fonts" = lib.listToAttrs (map
      (file:
        let name = builtins.unsafeDiscardStringContext (baseNameOf file);
        in lib.nameValuePair "${lib.removeSuffix ".ttf" (lib.removeSuffix ".otf" name)} (TrueType)" { sz = name; })
      config.fonts);

    build =
      let
        # Shell is a plain REG_SZ, so no %ProgramFiles%; setup always installs to C:.
        shellCommand =
          if config.shell ? package
          then ''"C:\Program Files\defenestrate\${config.shell.package.meta.mainProgram}"''
          else config.shell.command;

        regFiles = pkgs.callPackage ./regfiles.nix {
          files = {
            software = { hive = "software"; keys = config.registry.software; };
            system = { hive = "system"; keys = config.registry.system; };
            defaultUser = { hive = "defaultUser"; keys = config.registry.defaultUser; };
            # Applied by FirstLogonCommands instead of specialize: OOBE runs
            # its own session as defaultuser0, and that session needs
            # explorer. With the shell switched at specialize, OOBE died with
            # "Why did my PC restart?".
            shell = {
              hive = "software";
              keys."Microsoft\\Windows NT\\CurrentVersion\\Winlogon".Shell.sz = shellCommand;
            };
          };
        };
        # null when no apps are configured.
        apps = pkgs.callPackage ./apps.nix {
          locked = config.lockedApps;
          wanted = { baked = config.apps; winget = config.wingetApps; msstore = config.storeApps; };
          userName = config.user.name;
        };
        mkUnattend = { disk, firstLogonEnd ? "reboot" }: pkgs.writeText "autounattend.xml" (import ./unattend.nix {
          config = config // { inherit disk; };
          inherit lib firstLogonEnd;
          armApps = apps != null;
          productKey = genericKeys.${config.edition};
        });
        mkIso = { unattend, bootPrompt }: pkgs.callPackage ./iso.nix {
          inherit (config) source edition;
          inherit (config) fonts;
          inherit regFiles unattend bootPrompt apps;
          shellPackage = config.shell.package or null;
        };

        unattend = mkUnattend { inherit (config) disk; };
        iso = mkIso { inherit unattend; inherit (config) bootPrompt; };

        # For image.nix, whose VM sees one disk and nothing
        # else: it's always DiskID 0, so wiping it unasked is safe whatever
        # config.disk says (that one's for the ISO on a stick, next to the
        # machine's other disks). The prompt goes because the disk is first in
        # the VM's boot order, so the CD only boots while the disk is blank.
        # First logon powers off, which is how image.nix knows setup is done.
        soleDiskIso = mkIso {
          unattend = mkUnattend {
            disk.wipe.diskId = 0;
            firstLogonEnd = "poweroff";
          };
          bootPrompt = false;
        };
        image = pkgs.callPackage ./image.nix { iso = soleDiskIso; armsApps = apps != null; };
      in
      {
        inherit regFiles unattend iso;
        vm = pkgs.callPackage ./vm.nix { inherit iso; };
        inherit image;
        install = pkgs.callPackage ./install.nix {
          inherit (config) installTarget;
          inherit image;
        };
      } // lib.optionalAttrs (apps != null) { inherit apps; };
  };
}
