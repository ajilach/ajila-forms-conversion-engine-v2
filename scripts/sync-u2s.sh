#!/usr/bin/env bash
# Re-sync the vendored u2s crates from an ajila-forms-conversion-engine-v3
# checkout, then re-apply this repo's local patches (u2s/patches/*.patch).
#
#   scripts/sync-u2s.sh ../unstructured-to-structured
#
# See u2s/VENDORED.md. A patch that no longer applies stops the sync: fix it
# against the new upstream, regenerate it, and run the script again.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <path-to-v3-checkout>" >&2
  exit 2
fi

SRC="$(cd "$1" && pwd)"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/u2s"

if [ -n "$(git -C "$ROOT" status --porcelain -- u2s docker/aem docker/redacto)" ]; then
  echo "u2s/, docker/aem/ or docker/redacto/ has uncommitted changes; commit or stash them first" >&2
  exit 1
fi

COMMIT="$(git -C "$SRC" rev-parse --short HEAD)"

# Keep in step with the table in u2s/VENDORED.md and the workspace members.
CRATES=(
  u2s-aem u2s-aem-ubs-mcp u2s-aem-ubs-verify-mcp u2s-aem-verify-core u2s-blob
  u2s-core u2s-doc-tools u2s-facts u2s-jsondoc u2s-mapper-aem
  u2s-mapper-redacto u2s-mcp u2s-redacto u2s-redacto-ubs-mcp
  u2s-redacto-ubs-verify-mcp u2s-redacto-verify-core u2s-render-core
  u2s-render-pdf u2s-render-pdf-mcp u2s-render-test-harness u2s-render-xfa
  u2s-render-xfa-mcp u2s-rules u2s-rules-host u2s-schema u2s-verify-core
  u2s-xfa u2s-xfa-mcp
)

# The recorded commit must describe exactly what is copied: only the paths
# copied below must be clean upstream, so work in progress on the rest of the
# upstream workspace does not block a sync.
COPIED=(specs/aem/aem-xml-spec.md vendor/fonts corpus/ubs fixtures docker/aem docker/redacto)
for c in "${CRATES[@]}"; do COPIED+=("crates/$c"); done
if [ -n "$(git -C "$SRC" status --porcelain -- "${COPIED[@]}")" ]; then
  echo "$SRC has uncommitted changes in what is copied; commit them upstream first so $COMMIT matches:" >&2
  git -C "$SRC" status --short -- "${COPIED[@]}" >&2
  exit 1
fi

for c in "${CRATES[@]}"; do
  rsync -a --delete --exclude target "$SRC/crates/$c/" "$DEST/crates/$c/"
done

# Plain-file assets. Symlinked assets (the corpus forms and Frutiger faces
# that originate in this repo, and the fixture links into crates/) are left
# as they are.
rm -f "$DEST/specs/AEM.md"
mkdir -p "$DEST/specs/aem"
cp "$SRC/specs/aem/aem-xml-spec.md" "$DEST/specs/aem/aem-xml-spec.md"
cp "$SRC/vendor/fonts/DejaVuSans.ttf" "$SRC/vendor/fonts/LICENSE" "$DEST/vendor/fonts/"
cp "$SRC/vendor/fonts/ubs-frutiger/README.md" "$DEST/vendor/fonts/ubs-frutiger/"
cp "$SRC/corpus/ubs/README.md" "$DEST/corpus/ubs/"
cp "$SRC/fixtures/README.md" "$DEST/fixtures/"
cp "$SRC/docker/aem/README.md" "$SRC/docker/aem/bake-ubs-platform.sh" "$ROOT/docker/aem/"
rsync -a --delete "$SRC/docker/aem/dompurify/" "$ROOT/docker/aem/dompurify/"
mkdir -p "$ROOT/docker/redacto"
cp "$SRC/docker/redacto/README.md" "$ROOT/docker/redacto/"

for p in "$DEST"/patches/*.patch; do
  echo "applying $(basename "$p")"
  git -C "$ROOT" apply "$p"
done

sed -i.bak "s/^Upstream commit: .*/Upstream commit: \`$COMMIT\`/" "$DEST/VENDORED.md"
rm -f "$DEST/VENDORED.md.bak"

echo "synced to $COMMIT. Next: cargo build --workspace, then the u2s and agent tests."
git -C "$ROOT" status --short -- u2s docker
