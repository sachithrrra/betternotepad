#!/bin/sh
# Downloads the official Sparkle release and vendors Sparkle.framework plus its
# signing tools (generate_keys, sign_update, generate_appcast) into app/vendor/.
# Run once before `cargo bundle` release builds.
set -eu

VERSION="2.10.0"
URL="https://github.com/sparkle-project/Sparkle/releases/download/${VERSION}/Sparkle-${VERSION}.tar.xz"

cd "$(dirname "$0")/.."
rm -rf vendor
mkdir -p vendor
curl -fL "$URL" -o "/tmp/Sparkle-${VERSION}.tar.xz"
tar -xf "/tmp/Sparkle-${VERSION}.tar.xz" -C vendor Sparkle.framework bin
rm "/tmp/Sparkle-${VERSION}.tar.xz"

echo "Vendored Sparkle ${VERSION} into app/vendor/"
echo "  Sparkle.framework  - linked into the app at build time"
echo "  bin/generate_keys, bin/sign_update, bin/generate_appcast"
