# The AppImage's entry point. A Nix-built program cannot load the host's GPU drivers (they are
# linked against the host's libraries, which the Nix dynamic loader does not search), so the
# AppImage carries mesa's Vulkan drivers (AMD radv, Intel anv, nouveau nvk, lavapipe on the CPU)
# and points the Vulkan loader at them — unless the user chose drivers already. The desktop
# entry and icon are copied next to it (nix-appimage takes them from the entry point's package).
{
  lib,
  runCommand,
  writeShellScript,
  mesa,
  vibenote,
}:

let
  start = writeShellScript "vibenote" ''
    if [ -z "''${VK_DRIVER_FILES-}" ] && [ -z "''${VK_ICD_FILENAMES-}" ]; then
      icds=""
      for f in ${mesa}/share/vulkan/icd.d/*.json; do
        icds="''${icds:+$icds:}$f"
      done
      export VK_DRIVER_FILES="$icds"
    fi
    exec ${lib.getExe vibenote} "$@"
  '';
in
runCommand "vibenote-appimage-entry-${vibenote.version}"
  {
    meta.mainProgram = "vibenote";
  }
  ''
    install -Dm755 ${start} $out/bin/vibenote
    mkdir -p $out/share
    cp -r ${vibenote}/share/applications ${vibenote}/share/icons $out/share/
  ''
