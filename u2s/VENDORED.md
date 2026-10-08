# Vendored u2s crates

Source: `ajilach/ajila-forms-conversion-engine-v3` (local checkout usually at
`../unstructured-to-structured`).

Upstream commit: `29a28e9`

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
| `u2s-render-pdf`, `u2s-render-pdf-mcp` | Viewing the PDFs the verifiers produce (`pdf_*`); needs pdfium, see `scripts/fetch-pdfium.sh` |
| `u2s-aem-ubs-verify-mcp`, `u2s-aem-verify-core`, `u2s-mapper-aem`, `u2s-aem` | AEM verification against a Docker AEM + Chromium |
| `u2s-redacto-ubs-verify-mcp`, `u2s-redacto-verify-core`, `u2s-mapper-redacto`, `u2s-redacto` | Redacto dump verification: imports and renders the dump on a Redacto platform (Postgres, migration, core, rendering) booted per session, see `docker/redacto/README.md` |
| `u2s-aem-ubs-mcp` | The UBS AEM format: the authored `UbsAemDocument`, `encode` into a FileVault package through the UBS templates, `decode` back, and the UBS check rules in `rules/` |
| `u2s-redacto-ubs-mcp` | The UBS Redacto format: the authored `UbsRedactoDocument`, `encode` into the platform's dump with the UBS metadata and page furniture, `decode` back |
| `u2s-doc-tools`, `u2s-jsondoc`, `u2s-schema` | The `json_*` document tools and `rule_*` rule tools over one revisioned JSON document, schema validation, and the loader for rules checked in as files |
| `u2s-rules`, `u2s-rules-host`, `u2s-facts` | The rule sandbox and the worker process each rule runs in, with its memory and time ceiling; the converting binaries are their own worker (`u2s_rules_host::worker`), the agent tests use the `u2s-rules-worker` binary |
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
    also gets `shutdown()`, which tears down every session not in use.
  - `u2s-aem-ubs-verify-mcp` gets a `lib.rs` exposing its driver and specs.
  - `u2s-aem-verify-core`'s `AemVerifyServer` becomes public, with `with_parts`, `dispatch`
    and `shutdown`. Leftover containers of a crashed run are removed through upstream's own
    public `u2s_verify_core::session::remove_leftovers`.
- `0003-podman-engine.patch`: lets the verifiers run on Podman as well as Docker.
  `DockerLifecycle::connect` tries `DOCKER_HOST`/`CONTAINER_HOST`, `/var/run/docker.sock`,
  Docker Desktop's per-user socket, a Podman machine's API socket on macOS
  (`$TMPDIR/podman/*-api.sock`, `~/.local/share/containers/podman/machine/`), the Linux Podman
  sockets and finally `podman machine inspect`, and keeps the first that answers
  (`engine_sockets`). It adds `engine_description` and `volume_exists`. The AEM container
  gets the `host.docker.internal:host-gateway` mapping only when a Redacto URL is configured,
  since Podman before 5.3 refuses `host-gateway`.
- `0004-submit-diagnostics.patch`: makes a failed or empty submit explain itself. The UBS
  driver records the server's submit answer and the page's console lines (`ubs_js::SUBMIT`,
  `SUBMIT_RESULT`). The flow stops waiting for the download 15 s after the server answered;
  when no download came, it reads the stored DoR from `/tmp/ubsdocs/<uuid>/<name>` over HTTP
  (`FormDriver::stored_artefact_path`, `AemClient::fetch_path`) and says so in a
  `download_failed_read_from_repository` finding. A PDF with no font and no image raises
  `pdf_blank`. The matching lines of `ubsbundle.log` (`SummaryOutput:`, `rendering summary
  document`, errors) are attached as a `server_log` finding (`AemClient::tail_log`).
- `0005-submit-log-scope.patch`: the `server_log` finding keeps only the lines from the last
  submit's start marker on (`SubmitLogFilter`), so a second submit in the same session no longer
  reports the first one's lines.

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
- `u2s-render-pdf`: `PDFIUM_LIB_PATH`, overriding the lookup next to the binary and in
  `vendor/pdfium/lib`.
- `U2S_FONT_DIR`, `U2S_FONT_FALLBACK`, `U2S_BLOB_DIR`: read only by the stdio binaries and
  `from_env` constructors, never by the adapter.

Not vendored, so their fixture links are dropped: `u2s-aem-mcp` (`generic_minimal.zip`) and
`u2s-test-verify-mcp` (`verify_fixture_package.json`).
