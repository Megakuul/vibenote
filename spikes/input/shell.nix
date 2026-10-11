# Dev shell for the Phase 0 input spike. Usage: nix-shell spikes/input/shell.nix
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config cargo rustc clippy rustfmt libinput.bin ];
  buildInputs = with pkgs; [
    gtk4 glib cairo pango gdk-pixbuf graphene
    wayland wayland-protocols libxkbcommon vulkan-loader libGL
    libinput udev
  ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon vulkan-loader libGL ]);
}
