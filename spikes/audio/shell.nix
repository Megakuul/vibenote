{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ pkg-config cargo rustc rustPlatform.bindgenHook ];
  buildInputs = with pkgs; [ pipewire libopus alsa-lib ];
}
