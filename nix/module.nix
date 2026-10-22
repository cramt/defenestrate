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
    # 22H2+ OOBE stops at "connect to a network" without this. The interactive
    # escapes (bypassnro.cmd, ms-cxh:localonly) are gone since 24H2; the
    # registry value still works. Lesson from nixconf's saturn-windows-image.sh.
    registry.software."Microsoft\\Windows\\CurrentVersion\\OOBE".BypassNRO.dword = 1;

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
        unattend = pkgs.writeText "autounattend.xml" (import ./unattend.nix {
          inherit lib config;
          productKey = genericKeys.${config.edition};
        });
        iso = pkgs.callPackage ./iso.nix {
          inherit (config) source edition bootPrompt;
          inherit regFiles unattend;
          shellPackage = config.shell.package or null;
        };
      in
      {
        inherit regFiles unattend iso;
        vm = pkgs.callPackage ./vm.nix { inherit iso; };
      };
  };
}
