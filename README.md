# vibenote

Simple and fast personal note taking app for wayland (and the result of my experiments with multi stage LLM harness construction).


> [!WARNING]
> Application was cleanroom implemented by LLMs (in Rust); if you are interested in my work read the spec / guardrails not the code.


## Install

### Nix (flake)

Run it once:

```sh
nix run github:megakuul/vibenote
```

Install it on NixOS (flake-based configuration):

```nix
{
  inputs.vibenote.url = "github:megakuul/vibenote";

  outputs = { nixpkgs, vibenote, ... }: {
    nixosConfigurations.<host> = nixpkgs.lib.nixosSystem {
      modules = [
        ({ pkgs, ... }: {
          environment.systemPackages = [ vibenote.packages.${pkgs.stdenv.hostPlatform.system}.default ];
        })
      ];
    };
  };
}
```

or with the overlay (`nixpkgs.overlays = [ vibenote.overlays.default ];`, then `pkgs.vibenote`),
or for one user: `nix profile add github:megakuul/vibenote`.

The package contains `vibenote` (the app, with a desktop entry), `vibenote-cli` (headless
commands, backup and restore) and the sandboxed PDF worker. GPU drivers come from the system
(`hardware.graphics.enable = true;` on NixOS).

### AppImage (other distributions)

Every release has `vibenote-v<version>-x86_64.AppImage` (built with `nix build .#appimage`):

```sh
chmod +x vibenote-v0.1.0-x86_64.AppImage
./vibenote-v0.1.0-x86_64.AppImage
```

On its first start it adds itself to your app launcher (a desktop entry and icons in
`~/.local/share`), and updates that entry when you move the file. Run any other way (`nix run`,
`cargo run`), Settings → "Add to app launcher" or `vibenote --install-desktop-entry` does the
same; a system-wide installation (the Nix package) already has its entry and icon.

It carries everything it needs, including mesa's Vulkan drivers (AMD, Intel, nouveau, and
lavapipe on the CPU); NVIDIA's proprietary driver is not used from inside it. It needs
unprivileged user namespaces; on Ubuntu 24.04 and later these are restricted by AppArmor
(`sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0`, or an AppArmor profile for the
AppImage).

## Develop

```sh
nix develop   
cargo run -p vibenote -- --workspace /tmp/vibenote-try.sqlite
cargo xtask ci
```

## Releases

Releases follow [conventional commits](https://www.conventionalcommits.org): on every push to
`main` the `release` workflow tags the next version (`tools/release/next-version.py`) from the
commits since the last tag (all commits before the first release) — `fix:`
or `perf:` → patch, `feat:` → minor, `feat!:` / `BREAKING CHANGE:` → major; `docs:`, `chore:`,
`ci:`, `refactor:`, `test:` release nothing — then builds the AppImage with Nix (version stamped
from the tag) and publishes a GitHub release with the changelog, the AppImage and its sha256.
Running the workflow by hand with a tag rebuilds that release's AppImage.
