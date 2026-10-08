# Vendored u2s crates

Source: `ajilach/ajila-forms-conversion-engine-v3` (local checkout usually at
`../unstructured-to-structured`).

Upstream commit: `e19bcd9`

Re-sync with `scripts/sync-u2s.sh <checkout>`. It copies the crates and plain-file assets, then
re-applies every file in `u2s/patches/` in order. Do not edit vendored code directly: make the
change, regenerate the matching patch file, and keep the upstream diff as small as possible.

## Layout

`u2s/` mirrors the upstream repository root (`crates/`, `vendor/fonts/`, `corpus/ubs/`,
`fixtures/`, `specs/AEM.md`), so every `CARGO_MANIFEST_DIR/../../<asset>` path in upstream code
resolves without a patch.

Assets that originate in this repo are symlinks rather than copies:

- `corpus/ubs/*.pdf` points at `forms/`. The exception is `AAJB_033_IT.pdf`, which exists
  only upstream and is copied.
- `vendor/fonts/ubs-frutiger/*.ttf` points at `profiles/ubs/parser/fonts/`.
- `fixtures/*` links into `crates/` exactly as upstream does.

## Crates

| Crate | Role here |
|---|---|
| `u2s-xfa`, `u2s-render-xfa`, `u2s-xfa-mcp`, `u2s-render-xfa-mcp` | Source-form reads, renders and live interaction (`xfa_*`) |
| `u2s-render-pdf`, `u2s-render-pdf-mcp` | Viewing the PDFs the verifiers produce (`pdf_*`); pdfium is embedded by agent/build.rs |
| `u2s-aem-ubs-verify-mcp`, `u2s-aem-verify-core`, `u2s-mapper-aem`, `u2s-aem` | AEM verification against a Docker AEM + Chromium |
| `u2s-redacto-ubs-verify-mcp`, `u2s-redacto-verify-core`, `u2s-mapper-redacto`, `u2s-redacto` | Redacto dump verification: imports and renders the dump on a Redacto platform (Postgres, migration, core, rendering) booted per session, see `docker/redacto/README.md` |
| `u2s-aem-ubs-mcp` | The UBS AEM format: the authored `UbsAemDocument`, `encode` into a FileVault package through the UBS templates, `decode` back. It ships no check rules (v3 keeps rules in its database); this repo's live in `rules/aem/` |
| `u2s-redacto-ubs-mcp` | The UBS Redacto format: the authored `UbsRedactoDocument`, `encode` into the platform's dump with the UBS metadata and page furniture, `decode` back |
| `u2s-doc-tools`, `u2s-jsondoc`, `u2s-schema` | The `json_*` document tools and `rule_*` rule tools over one revisioned JSON document, schema validation, and the loader for rules checked in as files |
| `u2s-rules`, `u2s-rules-host`, `u2s-facts` | The rule sandbox and the worker process each rule runs in, with its memory and time ceiling; the converting binaries are their own worker (`u2s_rules_host::worker`), the agent tests use the `u2s-rules-worker` binary |
| `u2s-verify-core`, `u2s-render-core`, `u2s-blob`, `u2s-core` | Shared runtime |
| `u2s-mcp`, `u2s-render-test-harness` | Test-only: upstream's stdio conformance battery for the server binaries |

The dependency versions the crates inherit with `workspace = true` sit in the root
`Cargo.toml`, pinned as upstream pins them.

## Local patches (`u2s/patches/`)

v3's tool servers are libraries as well as stdio binaries (each crate's `lib.rs` holds the server,
`main.rs` serves it from the environment), so `agent/src/u2s.rs` links them in-process through their
own `with_parts`, `dispatch`, `specs` and, for the verifiers, `shutdown`, with no patch. What is
still patched here:

- `0001-drop-u2s-engine-dev-dependency.patch`: removes the `u2s-engine` dev-dependency from
  `u2s-aem` and `u2s-redacto`, together with the three schema tests that used it
  (`skeleton.rs`, and `strict_adaptation.rs` in both crates). `u2s-engine` is v3's LLM engine;
  it is not vendored here.
- `0002-pdfium-host-library-path.patch`: adds `u2s_render_pdf::set_library_path(PathBuf)`,
  which lets the host name the pdfium library to bind; it is consulted after `PDFIUM_LIB_PATH`.

## How the agent offers the vendored tools

`agent/src/u2s.rs` takes each server's own specs, keeps `name`, `description` and
`input_schema`, and changes only this:

| Upstream | Offered as | Why |
|---|---|---|
| `xfa_*` (both XFA servers), `pdf_*` | unchanged | the names do not collide |
| `verify_*` of `u2s-aem-ubs-verify-mcp` | `aem_verify_*` | both verifiers name tools `verify_status` / `verify_run` |
| `verify_*` of `u2s-redacto-ubs-verify-mcp` | `redacto_verify_*` | same |

Sibling tool names inside a verifier's descriptions are renamed with it. The verifiers lose
the arguments the adapter supplies (`package`, `package_path`, `artifact_blob`,
`artifact_path`, `session_id`): the artifact is always the run's latest build and each agent
has one session. A sentence saying so is appended to those descriptions.

## Environment the vendored code still reads

The adapter passes every setting it knows as values (`with_parts`, `Profile::from_reader`,
a constructed `RenderProfile`). The library paths still read these at runtime, all optional
with sensible defaults; nothing sets them:

- `u2s-render-core` `Limits` overrides: `DEFAULT_DPI`, `MAX_EDGE_PX`, `MAX_IMAGES_PER_CALL`,
  `MAX_RESPONSE_BYTES`, `MAX_INLINE_BYTES`, `SEARCH_MAX_PAGES` (only via `Limits::from_env`,
  which the adapter does not call).
- `u2s-render-xfa`: `RASTER_CACHE_SIZE`, `DOC_CACHE_SIZE`; `u2s-xfa`: `XFA_MAX_CALC_ITERATIONS`.
- `u2s-render-pdf`: `PDFIUM_LIB_PATH`, overriding the library path set by the host via
  `set_library_path()` and the embedded library written to the user cache directory at runtime.
- `U2S_FONT_DIR`, `U2S_FONT_FALLBACK`, `U2S_BLOB_DIR`: read only by the stdio binaries and
  `from_env` constructors, never by the adapter.

Not vendored, so their fixture links are dropped: `u2s-aem-mcp` (`generic_minimal.zip`) and
`u2s-test-verify-mcp` (`verify_fixture_package.json`).
