# The vibenote package: the app, its sandboxed PDF worker and the CLI, with the handwriting
# assets, pdfium and the libraries the app loads at run time (Vulkan, Wayland, xkbcommon).
{
  lib,
  rustPlatform,
  pkg-config,
  cmake,
  makeWrapper,
  wayland,
  wayland-protocols,
  libxkbcommon,
  vulkan-loader,
  libGL,
  libinput,
  udev,
  pipewire,
  libopus,
  alsa-lib,
  pdfium,
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;
  # Only what the build needs: every workspace member's sources (cargo resolves the whole
  # workspace) and the bundled assets; no tests data, docs, plans or spikes.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../.cargo
      ../crates
      ../apps
      ../xtask
      ../tools/simtab
      ../tests/acceptance
      ../assets
      ../LICENSE
      ../ATTRIBUTION.md
    ];
  };
  # Libraries opened with dlopen at run time.
  runtimeLibs = [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
  ];
in
rustPlatform.buildRustPackage {
  pname = "vibenote";
  inherit (cargoToml.workspace.package) version;
  inherit src;

  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "--package"
    "vibenote"
    "--package"
    "vibenote-cli"
    "--package"
    "vibenote-pdfd"
  ];
  # The test suite needs a GPU, a Wayland session and test data; `cargo xtask ci` runs it.
  doCheck = false;

  # Line tables are not needed in the package (the workspace's release profile keeps them for
  # local profiling).
  env.CARGO_PROFILE_RELEASE_DEBUG = "0";

  nativeBuildInputs = [
    pkg-config
    cmake
    makeWrapper
    rustPlatform.bindgenHook
  ];
  buildInputs = runtimeLibs ++ [
    wayland-protocols
    libinput
    udev
    pipewire
    libopus
    alsa-lib
  ];
  # cmake is only for crates' build scripts; the workspace itself is built by cargo.
  dontUseCmakeConfigure = true;

  postInstall = ''
    # Handwriting packs and the writing/drawing classifier (found through $VIBENOTE_ASSETS).
    mkdir -p $out/share/vibenote
    cp -r assets/packs assets/hwr $out/share/vibenote/

    # Desktop entry and icon.
    install -Dm644 assets/icons/vibenote.svg $out/share/icons/hicolor/scalable/apps/vibenote.svg
    for d in assets/icons/hicolor/*/; do
      size=$(basename "$d")
      install -Dm644 "$d/vibenote.png" "$out/share/icons/hicolor/$size/apps/vibenote.png"
    done
    install -Dm644 /dev/stdin $out/share/applications/vibenote.desktop <<EOF
    [Desktop Entry]
    Type=Application
    Name=vibenote
    GenericName=Handwritten notes
    Comment=A fast, paper-like handwriting workspace
    Exec=vibenote %U
    Icon=vibenote
    Terminal=false
    Categories=Office;Education;Graphics;
    Keywords=notes;handwriting;pen;tablet;pdf;
    StartupWMClass=vibenote
    EOF

    # Licences: the project, every bundled asset, pdfium and the code it carries
    # (ATTRIBUTION.md, "Native libraries").
    doc=$out/share/doc/vibenote
    install -Dm644 LICENSE ATTRIBUTION.md -t $doc
    install -Dm644 assets/fonts/*LICENSE* -t $doc/fonts
    install -Dm644 ${pdfium.gclientDeps.src.path}/LICENSE $doc/pdfium/LICENSE
    (cd ${pdfium.gclientDeps.src.path} && find third_party -name 'LICENSE*' -type f) | while read -r f; do
      install -Dm644 "${pdfium.gclientDeps.src.path}/$f" "$doc/pdfium/$f"
    done

    wrapProgram $out/bin/vibenote \
      --set-default VIBENOTE_ASSETS $out/share/vibenote \
      --set-default VIBENOTE_LAUNCHER $out/bin/vibenote \
      --set-default VIBENOTE_PDFIUM ${pdfium}/lib \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
    wrapProgram $out/bin/vibenote-cli \
      --set-default VIBENOTE_ASSETS $out/share/vibenote \
      --set-default VIBENOTE_PDFIUM ${pdfium}/lib \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  meta = {
    description = "A fast, paper-like handwriting workspace for the Linux/Wayland desktop";
    homepage = "https://github.com/megakuul/vibenote";
    license = lib.licenses.mit;
    mainProgram = "vibenote";
    platforms = lib.platforms.linux;
  };
}
