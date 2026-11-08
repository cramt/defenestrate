# Throwaway QEMU guest: wipes its disk without asking and logs straight in.
{ ... }:

{
  user = {
    name = "user";
    autoLogon = true;
  };
  disk.wipe.diskId = 0;
  # nix/vm.nix boots the disk first, so skipping the prompt can't loop setup.
  bootPrompt = false;
  # Exercises the baked-app path on every image build.
  apps = [ "Mozilla.Firefox" ];
}
