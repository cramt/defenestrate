{ requireFile }:

# Microsoft only hands out download links that expire after 24 hours, so this
# can't be a fetchurl. Each user downloads the ISO once and adds it to the store.
requireFile rec {
  name = "Windows11_Client_x64_en-us_26300_9457.iso";
  hash = "sha256-vUMH3zK8ivM7OczssRdK6zRThmMPiaK4bHpONrVeplA=";
  message = ''
    defenestrate needs the stock Windows 11 26H2 (build 26300.9457) English x64 ISO:

      1. Download it from https://www.microsoft.com/software-download/windows11
         ("Download Windows 11 Disk Image (ISO) for x64 devices", English)
      2. nix-store --add-fixed sha256 ${name}

    If Microsoft has moved on to a newer build, the hash won't match: update
    name and hash in nix/source.nix.
  '';
}
