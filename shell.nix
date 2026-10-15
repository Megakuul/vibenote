# vibenote dev shell. Usage: nix-shell (from the repo root).
# Set CARGO_TARGET_DIR yourself to build elsewhere (the agent uses /dev/shm on the owner's machine).
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config cargo rustc clippy rustfmt cargo-deny libinput.bin rustPlatform.bindgenHook cmake ];
  buildInputs = with pkgs; [
    wayland wayland-protocols libxkbcommon vulkan-loader libGL
    libinput udev pipewire libopus alsa-lib
  ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon vulkan-loader libGL ]);
  # libpdfium for the PDF worker (ADR 0004; BSD-3-Clause/Apache-2.0/MIT, see ATTRIBUTION.md).
  PDFIUM_DIR = "${pkgs.pdfium}/lib";
}
