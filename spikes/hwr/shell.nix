# HWR spike: Python venv lives in /dev/shm (RAM) because the owner's disk is nearly full.
{ pkgs ? import <nixpkgs> {} }:
pkgs.mkShell {
  buildInputs = [ pkgs.python312 pkgs.stdenv.cc.cc.lib pkgs.zlib pkgs.libGL pkgs.glib ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib pkgs.zlib pkgs.libGL pkgs.glib ];
  HF_HOME = "/dev/shm/vibenote-hwr/hf";
  shellHook = ''
    export VENV=/dev/shm/vibenote-hwr/venv
    [ -d $VENV ] || python3 -m venv $VENV
    source $VENV/bin/activate
  '';
}
