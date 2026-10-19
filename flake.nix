{
  description = "vibenote: a fast, paper-like handwriting workspace for the Linux/Wayland desktop";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-appimage = {
      url = "github:ralismark/nix-appimage";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    nix-appimage,
  }: let
    systems = [
      "x86_64-linux"
      "aarch64-linux"
    ];
    forAll = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
  in {
    packages = forAll (
      system: pkgs: let
        vibenote = pkgs.callPackage ./nix/package.nix {};
      in {
        inherit vibenote;
        default = vibenote;
        appimage = nix-appimage.lib.${system}.mkAppImage {
          program = pkgs.lib.getExe (pkgs.callPackage ./nix/appimage-wrapper.nix {inherit vibenote;});
          name = "vibenote-${vibenote.version}-${pkgs.stdenv.hostPlatform.uname.processor}.AppImage";
          squashfsArgs = [
            "-comp"
            "zstd"
            "-Xcompression-level"
            "19"
            "-b"
            "1M"
          ];
        };
      }
    );

    apps = forAll (
      system: pkgs: {
        default = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.vibenote;
        };
        cli = {
          type = "app";
          program = "${self.packages.${system}.vibenote}/bin/vibenote-cli";
        };
      }
    );

    # `nixpkgs.overlays = [ vibenote.overlays.default ];` then `pkgs.vibenote`.
    overlays.default = final: _prev: {
      vibenote = final.callPackage ./nix/package.nix {};
    };

    # The development shell (same as `shell.nix`).
    devShells = forAll (
      system: pkgs: {
        default = import ./shell.nix {inherit pkgs;};
      }
    );

    checks = forAll (
      system: _pkgs: {
        vibenote = self.packages.${system}.vibenote;
      }
    );

    formatter = forAll (system: pkgs: pkgs.nixfmt);
  };
}
