# Blueprint

Decodes PDFs and extracts structured data for automated forms conversion.

## Supported Formats

**Input:**
- PDF (AcroForm)
- PDF (XFA)

**Output:**
- Structured JSON representation
- Standalone HTML
- XSD (XML Schema Definition)
- AEM Adaptive Forms package
- Redacto PostgreSQL dump

## Project Structure

| Crate | Description |
|---|---|
| `cli` | Command-line interface: the `convert` subcommand runs the AI conversion the app runs headless; `sessions` lists resumable conversions; `verify` checks setup. |
| `app` | Dioxus desktop application: drag-and-drop upload driving the autonomous conversion agent. |
| `agent` | Headless conversion-agent engine — the tool catalog/executor, edit-history store, reference store, and adapter over vendored u2s tools. No UI or LLM dependency, shared by the app, the pipeline and the MCP server. |
| `pipeline` | The conversion controller: the Author → Reviewer stage sequencing, retry recovery and abort handling. Depends on neither a UI framework nor an LLM provider — the consumer supplies a `TurnProvider` and a `RunObserver`. |
| `runner` | The host side of a run, shared by the app and the CLI: the two LLM transports (the Anthropic Messages API with prompt caching, and any OpenAI-compatible endpoint), history eviction, the operator settings, and the entry points that build the agent, open an edit-history session and record the result. |
| `mcp` | Model Context Protocol (stdio) server that exposes the conversion tools so an external LLM client (Claude Desktop, Claude Code, Cursor) can drive a conversion. |

## Prerequisites

- [Rust](https://rustup.rs/) (edition 2024)
- [Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started) — only needed for the desktop app

Dioxus can easily be installed using cargo-binstall:

```sh
cargo install cargo-binstall
cargo binstall dioxus-cli@0.7.9
```

In order to version large files we need the git lfs extension

```sh
brew install git-lfs
git lfs install
git lfs pull
```

## Running Tests

```sh
# Run the full test suite (--release is recommended for speed)
cargo test --release
```

## CLI

The CLI binary is defined in the `cli` crate. It has three subcommands: `convert` (the AI conversion), `sessions` (to list and resume runs), and `verify` (to check setup).

### AI conversion from the console

`blueprint convert` runs the same autonomous conversion the desktop app runs (Author → Reviewer → fix rounds). Progress prints as it happens, and artefacts are written to `--out` instead of the Downloads folder. A run started here can be reopened in the app, and vice versa.

The API key, model, review-round cap, extra instructions and AEM credentials default to the app's settings; every one can be overridden per invocation.

```sh
# Convert a form (multilingual sources allowed)
cargo run --release -p blueprint-cli -- convert form_DE.pdf form_EN.pdf --profile ubs

# Produce a Redacto document instead of an AEM package
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --target redacto

# Write artefacts to a specific directory
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --out ./out

# Use a specific key and model instead of the app's settings
ANTHROPIC_API_KEY=sk-… cargo run --release -p blueprint-cli -- convert path/to/form.pdf --model claude-opus-4-8

# Route the run through an OpenAI-compatible endpoint (OpenRouter, a local gateway)
OPENAI_API_KEY=sk-or-… cargo run --release -p blueprint-cli -- convert path/to/form.pdf \
  --provider openai --base-url https://openrouter.ai/api/v1 --model anthropic/claude-opus-4.1

# Steer the agent and allow more review rounds
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --instructions "Keep every footnote." --max-review-rounds 5

# Modify an existing AEM package instead of authoring from scratch
cargo run --release -p blueprint-cli -- convert form_DE.pdf template-package.zip

# List and resume earlier runs
cargo run --release -p blueprint-cli -- sessions
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID>

# Refine a run with feedback
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID> --feedback "The IBAN field must be mandatory."
```

### Verification setup

Both AEM and Redacto targets require verification during the conversion: the Author
and Reviewer drive the built form with verification tools, not with a browser.
Verification uses vendored u2s verifiers linked into the binaries (source in `u2s/`,
see `u2s/VENDORED.md`). Every run checks whether verification is set up before it
starts and refuses to run if it is not.

For an **AEM target**, the verifier boots its own AEM Forms instance plus a headless
Chromium, both in Docker, installs the built package there, and lets the Author and
Reviewer drive the form with the `aem_verify_*` tools (open, interact with controls,
set values, advance pages, submit, close, screenshot). The PDF a submission produces
is read with the `pdf_*` tools. The source form is read with the `xfa_*` tools.

Prerequisites on the machine running an AEM conversion:
- Docker running
- The AEM Forms image from ajila's private Azure registry pulled locally: `az login`, `az acr login --subscription BC_AZ_Ajila_10128 --name ajila`, then `docker pull ajila.azurecr.io/aemforms-arm:6.5.17.0`
- The Docker data volume with the UBS platform baked in (one-time setup: `docker/aem/bake-ubs-platform.sh`, see `docker/aem/README.md`)
- Only an ARM image exists today, so AEM conversions currently run only on Apple Silicon hosts

For a **Redacto target**, the verifier boots a Redacto platform of the run's own (Postgres, migration, core and rendering containers), imports the built dump there and renders it once per language.

Prerequisites for Redacto: Docker running, the public Postgres image pulled (`verify prepare` does that), and the platform images from ajila's private registry pulled: `az acr login --name ajilaclouddev`, then `docker pull` each image the settings name (see `docker/redacto/README.md`).

**For both targets:**
- Check rules: every rule runs in a sandboxed worker process, which is the converting binary itself started with `--u2s-rules-worker`, so nothing extra ships. The agent tests need the standalone worker built first: `cargo build --release -p u2s-rules-host --bin u2s-rules-worker`.
- `pdfium`: `./scripts/fetch-pdfium.sh` downloads the pinned pdfium library (checksum-verified) into `vendor/pdfium/`; a release ships `libpdfium` next to the binary.
- Settings: verifier settings live in the desktop app's settings (tab "Verification"): AEM image, data volume (default `u2s-aem-ubs-data`), container port (default 8080), user/password (default admin/admin), optional platform, optional Redacto URL; for Redacto: the migration, core and rendering images, Postgres image (default `postgres:16-alpine`), platform, rendering user/password (default admin/admin). The CLI reads the same stored settings.
- CLI overrides: `--aem-image <IMAGE>` and `--aem-volume <VOLUME>` apply to the current run.
- `blueprint verify prepare` pulls the public verifier images (headless Chromium `chromedp/headless-shell:stable` and Postgres); the AEM image must be pulled by hand (see above).
- `blueprint verify check [--target aem|redacto]` runs the readiness check a run performs: settings complete, Docker reachable, images present locally, the AEM data volume exists, pdfium loads.

```sh
cargo run --release -p blueprint-cli -- verify prepare    # Pull public verifier images
cargo run --release -p blueprint-cli -- verify check      # Full readiness check
```

A failed readiness check produces an error starting with "Verification is not
possible, so the run cannot start:" and lists every missing item. There is no switch
to run without verification.

Artefacts are named as in the app: `forms-package-<code>.zip`,
`forms-package-bindrefs-<code>.zip`, `schema-<code>.xsd`, `redacto-<code>.sql`,
plus `agent-log-<code>.md` — the run transcript. The finalize step only builds the
package; there is no upload or AEM path in the output. Ctrl-C stops the run at its
next checkpoint: no artefacts are written, but the session id is printed and the edit
history holds what the agent had built, so the run can be resumed with `--session`.

## App

The app is built with [Dioxus](https://dioxuslabs.com/) and targets the desktop. This is the recommended way of running the migration engine.

It bundles an AI conversion agent that drives the engine's tools turn by turn to convert a form interactively. The agent uses the Anthropic API by default — set the API key and model in the app's settings, under AI Model. The same settings tab switches the agent to any OpenAI-compatible chat-completions endpoint (OpenRouter, a local gateway) by entering a base URL, key and model id; that path sends no prompt-cache breakpoints, so a long run costs more input tokens there, and the model has to support tool calling and image input. Every tree change is versioned into a local edit-history SQLite database, so conversions can be reviewed and resumed.

Reopening the app restores the conversions that were open, sources and all, but never restarts them: a reopened tab sits on its result with a Continue button, and the agent runs only once that is pressed. Continue carries the session on as it stands — the agent finishes the tree the previous run left and rebuilds the outputs, which are not kept between sessions. The feedback field is the other way in, for when there is something specific to change.

### Development

```sh
cd app
dx serve --platform desktop
```

### Production Build

`dx run --release --platform desktop --package blueprint-app` works directly once pdfium is fetched (`./scripts/fetch-pdfium.sh`). A distributable app needs pdfium and the `mcp` server next to its executable, and `dx bundle` is what puts them there (`dx build` does not copy them). Stage them first, from the repo root:

```sh
./scripts/fetch-pdfium.sh
./scripts/stage-sidecars.sh
dx bundle --release --platform desktop --package blueprint-app --package-types macos
```

The app lands in `target/dx/blueprint-app/bundle/macos/macos/BlueprintApp.app`.

## MCP Server

The `mcp` crate is a [Model Context Protocol](https://modelcontextprotocol.io/) server that exposes the conversion tools over stdio, so an external LLM client (Claude Desktop, Claude Code, Cursor, …) can drive a conversion step by step. The client supplies the reasoning; the server supplies the tools, backed by the headless `agent` engine. It shares the same edit-history SQLite as the desktop app, so a conversion driven over MCP can later be reviewed in the app.

```sh
# Build the server binary
cargo build --release -p mcp
```

Register the built binary (`target/release/mcp`) in the client's MCP config with `command` pointing at it. The desktop app can also install the bundled server into Claude Desktop's config automatically. A call to `start_conversion` (with a `pdf_path` or `pdf_base64`, and an optional `profile`) loads a source PDF; every other tool then operates on that loaded conversion.

## Library Documentation

```sh
cargo doc -p blueprint --open
```

## Checking a conversion against the feedback guard

The sister repo `ajila-forms-conversion-feedback` guards the deployed UBS corpus against systemic defects, and its CI fails any form that re-introduces one. A form this engine converts joins that corpus, so the guard is the acceptance test for AEM output. Run it on a package a conversion wrote:

```sh
python3 scripts/check_feedback_rules.py out/AAOS_package.zip forms/AAOS_033_IT.pdf
python3 scripts/check_feedback_rules.py out/BAGE_package.zip forms/BAGE_019_DE.pdf forms/BAGE_019_EN.pdf
```

The script matches each package to its source PDFs by the form code in its `AF_<CODE>` path and runs the feedback repo's detectors over it. Exit code 0 means every enrolled rule is clean. Pass `--feedback-repo` when the checkout is not next to this one.

## Regenerating build assets

Two scripts regenerate checked-in assets. Neither runs as part of the build; run
them by hand when the asset needs to change.

```sh
# The quantized sentence-embedding model in references-mcp/models/ (semantic matching).
pip install torch transformers safetensors
python3 scripts/download_model.py

# The desktop app icons in app/icons/, from app/assets/app-icon.svg.
pip install cairosvg pillow
python3 scripts/generate_icon.py
```
