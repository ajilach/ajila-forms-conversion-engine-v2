# Vendored u2s crates

Source: `ajilach/ajila-forms-conversion-engine-v3` (local checkout usually at
`../unstructured-to-structured`).

Upstream commit: `fcc5498b8bd51297c72b494e698b670aff55547b`

The u2s crates are git dependencies at that one revision (`[workspace.dependencies]` in the root
`Cargo.toml`, which `agent` inherits), and their sources are checked in under `vendor/crates/`,
the output of `cargo vendor` for that git source. `.cargo/config.toml` points cargo there, so a
build never fetches them: Docker, CI and a fresh checkout need no access to the v3 repository.
crates.io is not vendored and is fetched as usual.

Do not edit `vendor/crates/`: cargo checks every file there against the crate's
`.cargo-checksum.json` and refuses a changed one. A change to u2s is made upstream and pinned
here.

## Updating

```sh
scripts/sync-u2s.sh ../unstructured-to-structured [rev]   # rev: default HEAD
cargo test --release --workspace
```

The script refuses a revision no remote branch contains, pins it in `Cargo.toml`, re-vendors
(which also moves `Cargo.lock`), regenerates `.cargo/config.toml`, copies the non-crate assets
from the same revision (`docker/aem/{README.md,bake-ubs-platform.sh,dompurify}` and
`docker/redacto/README.md`), records the revision above and builds against the new copy. Commit
the result as one change.

Only what a consumer compiles is pinned: the u2s crates' own tests run upstream, not here. Their
committed fixtures still come along in `vendor/crates/`, and a few agent tests read the UBS golden
documents and a generated PDF from there, so they follow the pinned revision.

## Crates

| Crate | Role here |
|---|---|
| `u2s-xfa`, `u2s-render-xfa`, `u2s-xfa-mcp`, `u2s-render-xfa-mcp` | Source-form reads, renders and live interaction (`xfa_*`) |
| `u2s-render-pdf`, `u2s-render-pdf-mcp` | Viewing the PDFs the verifiers produce (`pdf_*`); pdfium is embedded by agent/build.rs |
| `u2s-aem-ubs-verify-mcp`, `u2s-aem-verify-core`, `u2s-mapper-aem`, `u2s-aem` | AEM verification against a Docker AEM + Chromium |
| `u2s-redacto-ubs-verify-mcp`, `u2s-redacto-verify-core`, `u2s-mapper-redacto`, `u2s-redacto` | Redacto dump verification: imports and renders the dump on a Redacto platform (Postgres, migration, core, rendering) booted per session, see `docker/redacto/README.md` |
| `u2s-aem-ubs-mcp` | The UBS AEM format: the authored `UbsAemDocument`, `encode` into a FileVault package (lowered onto the generic AEM model, encoded by `u2s-mapper-aem`), `decode` back. It ships no check rules (v3 keeps rules in its database); this repo's live in `rules/aem/` |
| `u2s-redacto-ubs-mcp` | The UBS Redacto format: the authored `UbsRedactoDocument`, `encode` into the platform's dump with the UBS metadata and page furniture, `decode` back |
| `u2s-doc-tools`, `u2s-jsondoc`, `u2s-schema` | The `json_*` document tools and `rule_*` rule tools over one revisioned JSON document, schema validation, and the loader for rules checked in as files |
| `u2s-rules`, `u2s-rules-host`, `u2s-facts` | The rule sandbox and the worker process each rule runs in, with its memory and time ceiling; the converting binaries are their own worker (`u2s_rules_host::worker`), the agent tests use the `u2s-rules-worker` binary |
| `u2s-verify-core`, `u2s-render-core`, `u2s-blob`, `u2s-core` | Shared runtime |

The table lists the crates `agent` uses and what they pull in; `cargo vendor` adds the rest of
their dependency tree among the u2s crates.

## No local patches

v3's tool servers are libraries as well as stdio binaries (each crate's `lib.rs` holds the server,
`main.rs` serves it from the environment), so `agent/src/u2s.rs` links them in-process through their
own `with_parts`, `dispatch`, `specs` and, for the verifiers, `shutdown`. The host pdfium path is
upstream's `u2s_render_pdf::set_library_path`. Nothing is patched: a change this repo needs goes
upstream.

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
