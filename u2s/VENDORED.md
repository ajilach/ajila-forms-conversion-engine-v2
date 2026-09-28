# Vendored u2s crates

Source: `ajilach/ajila-forms-conversion-engine-v3` (local checkout usually at
`../unstructured-to-structured`).

Upstream commit: `a31de57`

Re-sync with `scripts/sync-u2s.sh <checkout>`. It copies the crates and plain-file assets, then
re-applies every file in `u2s/patches/` in order. Do not edit vendored code directly: make the
change, regenerate the matching patch file, and keep the upstream diff as small as possible.

## Layout

`u2s/` mirrors the upstream repository root (`crates/`, `vendor/fonts/`, `corpus/ubs/`,
`fixtures/`, `specs/AEM.md`), so every `CARGO_MANIFEST_DIR/../../<asset>` path in upstream code
resolves without a patch.

Assets that originate in this repo are symlinks rather than copies:

- `corpus/ubs/*.pdf` points at `core/input/`. The exception is `AAJB_033_IT.pdf`, which exists
  only upstream and is copied.
- `vendor/fonts/ubs-frutiger/*.ttf` points at `profiles/ubs/parser/fonts/`.
- `fixtures/*` links into `crates/` exactly as upstream does.

## Crates

| Crate | Role here |
|---|---|
| `u2s-xfa`, `u2s-render-xfa`, `u2s-xfa-mcp`, `u2s-render-xfa-mcp` | Source-form reads, renders and live interaction (`xfa_*`) |
| `u2s-render-pdf`, `u2s-render-pdf-mcp` | Viewing the PDFs the verifiers produce (`pdf_*`); needs pdfium, see `scripts/fetch-pdfium.sh` |
| `u2s-aem-ubs-verify-mcp`, `u2s-aem-verify-core`, `u2s-mapper-aem`, `u2s-aem` | AEM verification against a Docker AEM + Chromium |
| `u2s-redacto-ubs-verify-mcp`, `u2s-redacto-verify-core`, `u2s-mapper-redacto`, `u2s-redacto` | Redacto dump verification against a throwaway Postgres |
| `u2s-verify-core`, `u2s-render-core`, `u2s-blob`, `u2s-core` | Shared runtime |
| `u2s-mcp`, `u2s-render-test-harness` | Test-only: upstream's stdio conformance battery for the server binaries |

The dependency versions the crates inherit with `workspace = true` sit in the root
`Cargo.toml`, pinned as upstream pins them.

## Local patches (`u2s/patches/`)

- `0001-drop-u2s-engine-dev-dependency.patch`: removes the `u2s-engine` dev-dependency from
  `u2s-aem` and `u2s-redacto`, together with the three schema tests that used it
  (`skeleton.rs`, and `strict_adaptation.rs` in both crates). `u2s-engine` is v3's LLM engine;
  it is not vendored here.
- `0002-library-targets-for-in-process-hosts.patch`: gives the five server crates a library
  target, which `agent/src/u2s.rs` links in-process.
  - `u2s-xfa-mcp`, `u2s-render-xfa-mcp`, `u2s-render-pdf-mcp` and `u2s-redacto-ubs-verify-mcp`
    get a `lib.rs` that compiles their unchanged `main.rs` as a module (`#[path]`). Inside
    `main.rs`, the patch only makes the server type, its `dispatch` and the `specs` module
    `pub`.
  - Each constructor that reads the environment gets a `with_parts(...)` twin, which takes
    limits, blob store and profile as values; `new()` delegates to it. The Redacto verifier
    also gets `shutdown()`, which tears down its default session.
  - `u2s-aem-ubs-verify-mcp` gets a `lib.rs` exposing its driver and specs.
  - `u2s-aem-verify-core`'s `AemVerifyServer` becomes public, with `with_parts`, `dispatch`,
    `spawn_idle_sweep` and `shutdown`.

Not vendored, so their fixture links are dropped: `u2s-aem-mcp` (`generic_minimal.zip`) and
`u2s-test-verify-mcp` (`verify_fixture_package.json`).
