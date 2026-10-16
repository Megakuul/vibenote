#!/usr/bin/env bash
# Rebuilds assets/fonts/LucideIcons-subset.ttf: the Lucide icon font (lucide-static, ISC; some
# icons MIT from Feather) cut down to the icons the chrome uses (crates/ui/src/theme.rs `icon`).
# Add a code point there and here, then run this script (needs network and nix).
set -euo pipefail
VERSION=1.57.0
SHA256=afc8602c5231ce64ee227286f93772d0438eddd3b947b645bc14bfd1e49da518
UNICODES="U+E04D,U+E05D,U+E06C,U+E06D,U+E06F,U+E076,U+E0C9,U+E0CC,U+E0D7,U+E0D9,U+E0F4,U+E0FB,U+E106,U+E112,U+E118,U+E11C,U+E12A,U+E130,U+E13D,U+E151,U+E154,U+E167,U+E177,U+E18E,U+E192,U+E198,U+E19A,U+E1B2,U+E1B6,U+E1B7,U+E1CE,U+E1D0,U+E1D1,U+E1F9,U+E247,U+E257,U+E28F,U+E2A0,U+E2A1,U+E385,U+E386,U+E387,U+E412,U+E4B3,U+E52B,U+E559,U+E1C3,U+E1D7,U+E09E,U+E14E,U+E1F7,U+E3E8,U+E14B,U+E0BB,U+E1DD"
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSL -o "$work/l.tgz" "https://registry.npmjs.org/lucide-static/-/lucide-static-$VERSION.tgz"
echo "$SHA256  $work/l.tgz" | sha256sum -c -
tar xzf "$work/l.tgz" -C "$work"
nix-shell -p python3Packages.fonttools --run \
  "pyftsubset '$work/package/font/lucide.ttf' --unicodes=$UNICODES --no-hinting --output-file='$root/assets/fonts/LucideIcons-subset.ttf'"
cp "$work/package/LICENSE" "$root/assets/fonts/Lucide-LICENSE.txt"
