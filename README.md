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
| `core` | Core library — PDF parsing, XFA processing, analysis pipeline, and all output renderers. |
| `cli` | Command-line interface: the deterministic export run, plus `convert` — the AI conversion the app runs, headless. |
| `app` | Dioxus desktop application: drag-and-drop upload driving the autonomous conversion agent. |
| `agent` | Headless conversion-agent engine — the tool catalog/executor, edit-history store, reference store, and AEM client. No UI or LLM dependency, shared by the app, the pipeline and the MCP server. |
| `pipeline` | The conversion controller: the Analyst → Author → Reviewer stage sequencing, retry recovery and abort handling. Depends on neither a UI framework nor an LLM provider — the consumer supplies a `TurnProvider` and a `RunObserver`. |
| `runner` | The host side of a run, shared by the app and the CLI: the two LLM transports (the Anthropic Messages API with prompt caching, and any OpenAI-compatible endpoint), history eviction, the operator settings, and the entry points that build the agent, open an edit-history session and record the result. |
| `mcp` | Model Context Protocol (stdio) server that exposes the conversion tools so an external LLM client (Claude Desktop, Claude Code, Cursor) can drive a conversion. |
| `judge` | Evaluates translation quality of multi-language PDF forms and writes scores to CSV. |

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

## Running Benchmarks

Benchmarks live in `core/benches/` and use [Criterion](https://github.com/bheisler/criterion.rs). They automatically discover all PDFs in `core/input/`.

```sh
cargo bench -p blueprint
```

## CLI

The CLI binary is defined in the `cli` crate.

```sh
# Basic analysis (no file output)
cargo run --release -p blueprint-cli -- path/to/form.pdf

# Export structured JSON
cargo run --release -p blueprint-cli -- path/to/form.pdf --structured

# Export standalone HTML
cargo run --release -p blueprint-cli -- path/to/form.pdf --html

# Export AEM Adaptive Forms JCR content XML (XFA PDFs only)
cargo run --release -p blueprint-cli -- path/to/form.pdf --aem

# Export XSD (XML Schema Definition)
cargo run --release -p blueprint-cli -- path/to/form.pdf --xsd

# Use a profile for output-specific configuration
cargo run --release -p blueprint-cli -- path/to/form.pdf --aem --profile ubs

# Export GraphViz DOT decision flow
cargo run --release -p blueprint-cli -- path/to/form.pdf --graphviz

# Render images (modes: plain, labelled, annotated; repeatable)
cargo run --release -p blueprint-cli -- path/to/form.pdf --render plain --render labelled

# Custom render scale (default 1.5)
cargo run --release -p blueprint-cli -- path/to/form.pdf --render plain --scale 2.0

# Enable analysis modules
cargo run --release -p blueprint-cli -- path/to/form.pdf --module ubs

# Multilingual merge (pass multiple language variants)
cargo run --release -p blueprint-cli -- form_DE.pdf form_EN.pdf --structured --html

# Dump raw XFA XML and exit
cargo run --release -p blueprint-cli -- path/to/form.pdf --dump-xfa
```

### AI conversion from the console

`blueprint convert` runs the same autonomous conversion the desktop app runs —
the `pipeline` controller (Analyst → Author → Reviewer → fix rounds) over the
shared `runner` transport, the same tool catalog, the same edit-history SQLite.
Only the reporting and the output location differ: progress is printed as it
happens, and the artefacts are written to `--out` instead of the Downloads
folder. A run started here can be reopened in the app, and vice versa.

The API key, model, review-round cap, extra instructions and AEM credentials
default to whatever is configured in the app's settings; every one of them can be
overridden per invocation.

```sh
# Convert a form (multilingual sources allowed, as above)
cargo run --release -p blueprint-cli -- convert form_DE.pdf form_EN.pdf --profile ubs

# Produce a Redacto document instead of an AEM package
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --target redacto

# Write the artefacts somewhere else, and add the structured JSON
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --out ./out --structured

# Use a specific key and model instead of the app's settings
ANTHROPIC_API_KEY=sk-… cargo run --release -p blueprint-cli -- convert path/to/form.pdf --model claude-opus-4-8

# Route the run through an OpenAI-compatible endpoint (OpenRouter, a local gateway)
OPENAI_API_KEY=sk-or-… cargo run --release -p blueprint-cli -- convert path/to/form.pdf \
  --provider openai --base-url https://openrouter.ai/api/v1 --model anthropic/claude-opus-4.1

# Steer the agent, and allow more review rounds
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --instructions "Keep every footnote." --max-review-rounds 5

# Modify an existing AEM package instead of authoring from scratch
cargo run --release -p blueprint-cli -- convert form_DE.pdf template-package.zip

# Upload the finished package to AEM (off unless asked for)
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --upload --aem-host http://localhost:4502 --aem-user admin --aem-password admin

# Carry an earlier run on: list the sessions, then continue one as it stands
cargo run --release -p blueprint-cli -- sessions
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID>

# Or refine it, by giving the agent something specific to apply
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID> --feedback "The IBAN field must be mandatory."
```

### Browser verification

With `--upload` (or the app's AEM connection) the Author and Reviewer also get a
real browser: the run spawns the Playwright MCP server as a child process, logs it
in to AEM, and the two stages open the deployed form's preview, walk every wizard
page, fill the fields, submit, and read the PDF the submission downloads
(`aem_form_urls`, `browser_*`, `inspect_pdf`). The Reviewer reports a page that will
not advance, a field that cannot be filled or a PDF missing entered data as a
defect.

Prerequisites on the machine running the conversion: Node.js 18+ (with `npx`),
Google Chrome, and an AEM author instance the configured user can log in to.
Chrome is launched headless and isolated by the run itself; nothing has to be
started beforehand, and nothing survives the run.

The server version is pinned in `agent/src/browser.rs` (`PLAYWRIGHT_MCP_VERSION`);
`latest` is never used. `npx` runs with the npm cache preferred, so once the
package is cached a run never touches the registry. Warm the cache once, with a
connection:

```sh
cargo run --release -p blueprint-cli -- browser prepare   # Node, Chrome, npm cache; needs no AEM
cargo run --release -p blueprint-cli -- browser check     # the full preflight a run performs, against the configured AEM
```

Every run with the browser enabled repeats that preflight before it starts and
refuses to run when it fails, with the reason and the fix (or the switch to turn
the browser off). It never degrades silently. Switches:

```sh
# No browser for this run (the fetch_aem_dor_pdf fallback remains available to the agent)
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --upload --no-browser

# npx lives somewhere unusual (a Finder-launched app sees a minimal PATH; the app has the same setting)
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --upload --npx /opt/homebrew/bin/npx
```

The tool surface the model sees is the checked-in snapshot
`agent/tests/playwright_mcp_tools.json`, verified against the live server at every
start. To move to a new Playwright MCP version: bump `PLAYWRIGHT_MCP_VERSION`,
regenerate the snapshot with
`UPDATE_SNAPSHOTS=1 cargo test -p agent -- --ignored playwright_mcp_tool_surface_matches_snapshot`,
review the diff (tool descriptions are prompt surface), and re-read the prompts
that name the tools.

Artefacts are named as in the app: `forms-package-<code>.zip`,
`forms-package-bindrefs-<code>.zip`, `schema-<code>.xsd`, `redacto-<code>.sql`,
plus `agent-log-<code>.md` — the run transcript. Ctrl-C stops the run at its next
checkpoint: no artefacts are written, but the session id is printed and the edit
history holds what the agent had built, so the run can be resumed with
`--session`.

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

```sh
cd app
dx build --release --platform desktop
```

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

## Judge

The judge evaluates translation quality of multi-language PDF forms in `core/input/`. It processes all form codes in parallel using all available CPU cores and writes scores to `judge/results.csv` (override with `--input-dir`, `--profile` and `--output`).

```sh
# Run the judge on all form codes (parallel)
cargo run --release -p judge

# Run the judge on a single form code
cargo run --release -p judge -- --form-code ABCD_019

# Compare results against a baseline
cd judge
cp results.csv results-baseline.csv
# ... make changes ...
cargo run --release -p judge
python3 compare.py
```

## Checking a conversion against the feedback guard

The sister repo `ajila-forms-conversion-feedback` fixes systemic defects across the
deployed UBS corpus, and its CI guard fails any form that re-introduces one. A form
this engine converts joins that corpus, so the guard is the acceptance test for the
AEM output. Run it on a fresh conversion without importing anything:

```sh
python3 scripts/check_feedback_rules.py core/input/AAOS_033_IT.pdf
python3 scripts/check_feedback_rules.py core/input/BAGE_019_DE.pdf core/input/BAGE_019_EN.pdf
python3 scripts/check_feedback_rules.py --json core/input/AAOS_033_IT.pdf > report.json
```

It converts each form with `--aem --profile ubs`, builds a throwaway directory the
feedback repo's detectors read as their corpus (holding only the forms under test), and
runs `check_regressions.py --no-skip` over it. Exit code 0 means every enrolled rule is
clean. Pass `--feedback-repo` when the checkout is not next to this one.

## Regenerating build assets

Two scripts regenerate checked-in assets. Neither runs as part of the build; run
them by hand when the asset needs to change.

```sh
# The quantized sentence-embedding model in core/models/ (semantic matching).
pip install torch transformers safetensors
python3 scripts/download_model.py

# The desktop app icons in app/icons/, from app/assets/app-icon.svg.
pip install cairosvg pillow
python3 scripts/generate_icon.py
```
