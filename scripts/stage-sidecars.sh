#!/usr/bin/env bash
# Build and stage the binaries the desktop app ships next to its executable,
# where `[bundle].external_bin` in app/Dioxus.toml picks them up:
#
# - `mcp`, the stdio server the app registers with Claude Desktop;
# - `u2s-rules-worker`, the process every check rule runs in (a run refuses to
#   start without it);
# - pdfium, which the u2s PDF renderer loads (not on Windows: external_bin
#   appends .exe, which a DLL cannot carry).
#
# dx resolves each external_bin as `<value>-<target-triple>` relative to the
# repo root and copies it with the triple stripped. It silently skips a missing
# one, so run this before every `dx build` or `dx bundle`, from any directory:
#
#   scripts/stage-sidecars.sh [target-triple]   # defaults to the host triple
#
# Needs pdfium fetched first (scripts/fetch-pdfium.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
SIDECAR="$ROOT/sidecar"
OUT="$ROOT/target/$TARGET/release"

case "$TARGET" in
  *windows*) EXE=".exe" ;;
  *) EXE="" ;;
esac
case "$TARGET" in
  *apple-darwin) PDFIUM="libpdfium.dylib" ;;
  *linux*) PDFIUM="libpdfium.so" ;;
  *) PDFIUM="" ;;
esac

if [ -n "$PDFIUM" ] && [ ! -f "$ROOT/vendor/pdfium/lib/$PDFIUM" ]; then
  echo "error: $ROOT/vendor/pdfium/lib/$PDFIUM is missing; run scripts/fetch-pdfium.sh first" >&2
  exit 1
fi

cd "$ROOT"
cargo build --release -p mcp --target "$TARGET"
cargo build --release -p u2s-rules-host --bin u2s-rules-worker --target "$TARGET"

mkdir -p "$SIDECAR"
cp "$OUT/mcp$EXE" "$SIDECAR/mcp-$TARGET$EXE"
cp "$OUT/u2s-rules-worker$EXE" "$SIDECAR/u2s-rules-worker-$TARGET$EXE"
if [ -n "$PDFIUM" ]; then
  cp "vendor/pdfium/lib/$PDFIUM" "$SIDECAR/$PDFIUM-$TARGET"
fi
echo "staged sidecars for $TARGET in $SIDECAR"
