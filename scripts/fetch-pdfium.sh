#!/usr/bin/env bash
# Fetch the pinned pdfium dynamic library into vendor/pdfium/lib, where
# u2s-render-pdf looks for it (next to the binary, then vendor/pdfium/lib in
# any ancestor directory).
#
# Adapted from ajila-forms-conversion-engine-v3 (see u2s/VENDORED.md), plus
# per-asset SHA-256 pins and Windows support.
#
# The release is pinned: pdfium's antialiasing changes between builds, and the
# ABI must match the pdfium_XXXX feature in u2s/u2s-render-pdf/Cargo.toml.
# Bumping it means updating PDFIUM_RELEASE, that feature, and every checksum.
set -euo pipefail

PDFIUM_RELEASE="chromium/7881"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/vendor/pdfium"

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64)
    ASSET="pdfium-mac-arm64.tgz"
    SHA256="52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40" ;;
  Darwin/x86_64)
    ASSET="pdfium-mac-x64.tgz"
    SHA256="6dedf83990e0e3d6b7c93c9e7589c5a126b0ae14b7464d76120cff7a26afb18b" ;;
  Linux/x86_64)
    ASSET="pdfium-linux-x64.tgz"
    SHA256="1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d" ;;
  Linux/aarch64)
    ASSET="pdfium-linux-arm64.tgz"
    SHA256="ee7f7b7d5468958336a818c1cd580bdd20972846b7377b13f9a923d92d1d4674" ;;
  MINGW*/x86_64 | MSYS*/x86_64 | CYGWIN*/x86_64)
    ASSET="pdfium-win-x64.tgz"
    SHA256="73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08" ;;
  *) echo "unsupported platform: $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_RELEASE}/${ASSET}"

rm -rf "$DEST"
mkdir -p "$DEST"
echo "fetching $URL"
curl -fsSL "$URL" -o "$DEST/$ASSET"

ACTUAL="$( (sha256sum "$DEST/$ASSET" 2>/dev/null || shasum -a 256 "$DEST/$ASSET") | cut -d' ' -f1)"
if [ "$ACTUAL" != "$SHA256" ]; then
  echo "checksum mismatch for $ASSET: expected $SHA256, got $ACTUAL" >&2
  rm -rf "$DEST"
  exit 1
fi

tar -xzf "$DEST/$ASSET" -C "$DEST"
rm -f "$DEST/$ASSET"

# The Windows archive ships the DLL in bin/, but the loader only looks in lib/.
if [ -f "$DEST/bin/pdfium.dll" ]; then
  mkdir -p "$DEST/lib"
  mv "$DEST/bin/pdfium.dll" "$DEST/lib/"
fi

echo "pdfium ready:"
ls -la "$DEST/lib/"
