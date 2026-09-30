# Converting a form from the command line

`blueprint convert` runs the same autonomous conversion the desktop app runs (Analyst → Author → Reviewer → fix rounds). Progress prints as it happens, and artefacts land in an output directory instead of the Downloads folder. A run started here can be reopened in the app, and vice versa.

This guide covers the `convert` subcommand and everything needed to get it running.

---

## 1. Prerequisites

### 1.1 Toolchain

| Requirement | Why | Check |
|---|---|---|
| Rust 1.88+ (edition 2024) | Workspace `rust-version`; let-chains are used throughout | `rustc --version` |
| Git LFS | `agent/models/` holds a 235 MB safetensors model and a 17 MB tokenizer, both `include_bytes!`-embedded into the binary at compile time | `git lfs version` |

Install both, then pull the large files. Without the LFS pull the build embeds
LFS pointer text instead of the model and fails:

```sh
brew install git-lfs
git lfs install
git lfs pull
```

Verify the model is real and not a pointer — it must be hundreds of megabytes:

```sh
ls -lh agent/models/model.safetensors
```

The Dioxus CLI is **not** needed. That is only for the desktop app.

### 1.2 Build

```sh
cargo build --release -p blueprint-cli
```

The binary lands at `target/release/blueprint`. Expect a long first build and a
large binary — the embedded semantic-matching model accounts for most of it.

Every example below uses `cargo run --release -p blueprint-cli -- …`, which is
interchangeable with calling `target/release/blueprint` directly.

### 1.3 An API key

The run needs a model. There is no CLI command that writes settings, so on a
machine that has never run the desktop app, the key must come from a flag or the
environment on every invocation.

Resolution order for the key, highest first:

1. `--api-key`
2. `$ANTHROPIC_API_KEY` (or `$OPENAI_API_KEY` when the provider is `openai`)
3. Whatever the desktop app saved in its settings

If all three are empty the run stops before spending anything, naming the
environment variable it looked for.

The model resolves the same way minus the environment step: `--model`, else the
app's setting, else the built-in default `claude-opus-5`.

### 1.4 A profile

A profile supplies the parser fonts and (in `history.db`) the reference library. `ubs` is currently the only one installed, so `--profile` can be omitted — the CLI picks it automatically when exactly one profile exists. It errors rather than guessing if several are installed, and a resumed session inherits the profile it was created with.

### 1.5 Verification setup (required)

Both AEM and Redacto targets require verification during the conversion. Verification
uses vendored u2s verifiers linked into the binaries (source in `u2s/`; see
`u2s/VENDORED.md`). Every run checks whether verification is set up before it
starts and refuses to run if it is not. There is no switch to skip it.

**For an AEM target:** The verifier boots its own AEM Forms instance plus a headless
Chromium, both in Docker, installs the built package, and lets the Author and
Reviewer drive the form interactively using `aem_verify_*` tools (open, control
interaction, set values, advance pages, submit, screenshot). The submitted form's
PDF is read with `pdf_*` tools. The source PDF is read with `xfa_*` tools.

You need:
- Docker running
- The AEM Forms image from ajila's private Azure registry pulled locally: `az login`, `az acr login --subscription BC_AZ_Ajila_10128 --name ajila`, then `docker pull ajila.azurecr.io/aemforms-arm:6.5.17.0`
- The Docker data volume with the UBS platform baked in (one-time setup: run `docker/aem/bake-ubs-platform.sh`; see `docker/aem/README.md` for details)
- Apple Silicon host (only an ARM image exists today)

**For a Redacto target:** The `redacto_verify_*` tools boot a Redacto platform of the
run's own (Postgres, migration, core, rendering), import the built dump there and render
it once per language.

You need:
- Docker running
- The public Postgres image pulled (`verify prepare` does that)
- The platform's migration, core and rendering images from ajila's private registry: `az acr login --name ajilaclouddev`, then `docker pull` each (see `docker/redacto/README.md`)

**For both targets:**
- Check rules: every rule runs in a sandboxed worker process, which is `blueprint` itself started with `--u2s-rules-worker`; nothing extra needs building or shipping.
- `pdfium`: Run `./scripts/fetch-pdfium.sh` to download the pinned pdfium library (checksum-verified) into `vendor/pdfium/`; a release ships `libpdfium` next to the binary.
- Settings: Verifier settings are stored in the desktop app's settings tab ("Verification"). The CLI reads the same settings. Defaults: AEM image (default none, must be pulled manually), data volume (default `u2s-aem-ubs-data`), AEM port (default 8080), AEM user/password (default admin/admin); for Redacto: the migration, core and rendering images (default: the ones `ajila-redacto-platform`'s CI publishes to `ajilaclouddev.azurecr.io`), Postgres image (default `postgres:16-alpine`), platform, rendering user/password (default admin/admin).
- CLI overrides: `--aem-image <IMAGE>` and `--aem-volume <VOLUME>` apply to the current run.
- Prepare: Run `blueprint verify prepare` to pull the public verifier images (headless Chromium `chromedp/headless-shell:stable` and Postgres). The AEM image must be pulled by hand (see above).
- Check: Run `blueprint verify check [--target aem|redacto]` to run the readiness check a conversion performs: settings complete, Docker reachable, images present locally, the AEM data volume exists, pdfium loads.

```sh
cargo run --release -p blueprint-cli -- verify prepare    # Pull public verifier images
cargo run --release -p blueprint-cli -- verify check      # Full readiness check
```

A failed check produces an error starting with "Verification is not possible, so
the run cannot start:" and lists every missing item.

### 1.6 Where state lives

`<config_dir>/blueprint/history.db` — on macOS `~/Library/Application
Support/blueprint/history.db`. It holds the desktop app's settings, the
edit-history sessions, the stored source bytes and each session's running spend
total. The CLI, the app and the MCP server all share it. Deleting it discards
every resumable session.

---

## 2. Running a conversion

### 2.1 The minimum

```sh
ANTHROPIC_API_KEY=sk-… cargo run --release -p blueprint-cli -- convert path/to/form.pdf
```

Multiple language variants of the same form are passed together and merged:

```sh
cargo run --release -p blueprint-cli -- convert form_DE.pdf form_EN.pdf --profile ubs
```

An AEM content-package ZIP can be passed alongside the PDFs. It is pre-loaded as
the agent's editable working tree, and the Author is told to inspect and modify
it rather than author from scratch:

```sh
cargo run --release -p blueprint-cli -- convert form_DE.pdf template-package.zip
```

The run needs at least one PDF or one content package. Sources are distinguished
by file extension, so the `.pdf` suffix matters.

### 2.2 What the run does

1. **Preflight.** Resolve the profile and settings, then run the verification
   readiness check: settings complete, Docker reachable, images present locally,
   the AEM data volume exists (for AEM targets), pdfium loads. A refused preflight
   leaves no session behind and spends no tokens.
2. **Open the session.** Sources are hashed and stored content-addressed, a
   session row is created and an empty initial edit is recorded.
3. **Analyst → Author → (Reviewer → Author fix)\*.** The review rounds are capped
   by `--max-review-rounds` (default 3). The Author and Reviewer drive the built
   form using the `aem_verify_*` (or `redacto_verify_*`) verification tools to
   interact with it, submit it, and verify the output.
4. **Finalize.** The envelope, packages, XSD and Redacto SQL are assembled and
   recorded back into the history. There is no upload: the built package is the
   final artefact.

### 2.3 Reading the console

Stage banners (`── Author — building the AEM form ──`), the model's thoughts,
one line per tool call completed in place with `✓`/`✗` and its elapsed time, and
periodic context-fill readouts. Warnings go to stderr.

At the end:

```
── Result ──
Session: <id>
Spend: … in (… cached, … written) · … out · USD 1.23
Session total: …            # only when the session has earlier runs folded in
Elapsed: 412s
Form code: AAOS
Wrote: ./forms-package-AAOS.zip
…
```

Spend is reported even when the run stopped early — those turns were still
billed — and is folded into the session's running total, so a form resumed for a
later feedback round keeps what its earlier rounds cost.

### 2.4 Artefacts

Written to `--out` (default the current directory, created if missing), named
exactly as the desktop app names them in Downloads:

| Target | Files |
|---|---|
| `aem` (default) | `forms-package-<code>.zip`, `forms-package-bindrefs-<code>.zip`, `schema-<code>.xsd` |
| `redacto` | `redacto-<code>.sql` |
| both | `document-<code>.json` (the final document), `agent-log-<code>.md` (the Markdown run transcript) |
| both | `run-analysis/<date>_<time>_<source>_<session>/` — the run recorded for analysis (report, timeline, per-stage transcripts, full trace, `evaluation.md` template), in this repository's `run-analysis/` unless `--analysis-dir` names another folder; see [run-analysis.md](run-analysis.md). Written while the run happens, so it exists for a stopped run too. Placed with `--analysis-dir`, skipped with `--no-analysis`. |

`forms-package-bindrefs` is the same package built with `bind_to_xsd` on: every
field carries a `bindRef` and the schema is bundled. When the form code could
not be resolved the suffix is dropped (`forms-package.zip`). An artefact the run
did not produce is reported as `Not produced: <filename>` rather than silently
skipped.

### 2.5 Stopping and failure

`Ctrl-C` sets the abort flag; the run stops at its next checkpoint rather than
killing the process. No artefacts are written, but the session id is printed and
the edit history holds what the agent had built, so the run can be resumed. A
second `Ctrl-C` is the operating system's business.

A failed model turn is retried automatically by the controller first. Past that,
`--retries` (default 2) is the budget that stands in for the app's Retry button.
When it runs out the run stops and keeps whatever was built — a headless run that
retried forever would sit on a revoked key until killed.

---

## 3. Resuming and refining

List what can be resumed:

```sh
cargo run --release -p blueprint-cli -- sessions
```

One line per session, newest first: timestamp, session id, profile, edit count,
label.

**Continue** — carry the session on with nothing to apply. The Analyst is
skipped; the Author finishes the tree the earlier run left and the outputs are
rebuilt:

```sh
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID>
```

**Refine** — give the agent something specific to apply. The feedback becomes the
first pinned review:

```sh
cargo run --release -p blueprint-cli -- convert path/to/form.pdf --session <ID> \
  --feedback "The IBAN field must be mandatory."
```

Either way the source PDFs must be passed again; the run replays them through the
agent. `--feedback` requires `--session`. A continuation against a session that
holds no saved form is a hard error, not a run that bills a stage to produce
nothing.

---

## 4. Flag reference (`convert`)

| Flag | Default | Notes |
|---|---|---|
| `<DOCUMENT>…` | required | Source PDF(s), optionally plus an AEM content-package ZIP |
| `--profile <NAME>` | the only installed profile, or the session's | Errors if several exist and none is named |
| `--target <aem\|redacto>` | `aem` | Case-insensitive |
| `--out <DIR>` | `.` | Created if missing |
| `--provider <anthropic\|openai>` | the app's setting | `openai` means any OpenAI-compatible endpoint |
| `--base-url <URL>` | `https://openrouter.ai/api/v1` | Implies `--provider openai` |
| `--api-key <KEY>` | `$ANTHROPIC_API_KEY` / `$OPENAI_API_KEY`, then the app | |
| `--model <ID>` | the app's setting, then `claude-opus-5` | |
| `--max-review-rounds <N>` | 3 | Reviewer → Author-fix rounds before finalizing |
| `--instructions <TEXT>` | the app's setting | Appended to every role's system prompt |
| `--instructions-file <PATH>` | — | Conflicts with `--instructions` |
| `--retries <N>` | 2 | Operator-level retries after the controller's own |
| `--aem-image <IMAGE>` | the app's setting | AEM Forms image (e.g. `ajila.azurecr.io/aemforms-arm:6.5.17.0`); AEM target only |
| `--aem-volume <VOLUME>` | the app's setting (default `u2s-aem-ubs-data`) | Docker data volume with the UBS platform; AEM target only |
| `--session <ID>` | — | Resume; skips the Analyst |
| `--feedback <TEXT>` | — | Requires `--session` |
| `--analysis-dir <DIR>` | `run-analysis/` in this repository | Where the run's analysis folder is created |
| `--no-analysis` | off | Do not record the run for analysis |

---

## 5. Worked examples

```sh
# Redacto document instead of an AEM package
cargo run --release -p blueprint-cli -- convert form.pdf --target redacto

# Write artefacts to a specific directory
cargo run --release -p blueprint-cli -- convert form.pdf --out ./out

# A specific key and model
ANTHROPIC_API_KEY=sk-… cargo run --release -p blueprint-cli -- convert form.pdf \
  --model claude-opus-5

# Through an OpenAI-compatible endpoint. Note: no prompt-cache breakpoints are
# sent on this path, so a long run costs more input tokens; the model must
# support tool calling and image input.
OPENAI_API_KEY=sk-or-… cargo run --release -p blueprint-cli -- convert form.pdf \
  --provider openai --base-url https://openrouter.ai/api/v1 \
  --model anthropic/claude-opus-4.1

# Steer the agent and allow more review rounds
cargo run --release -p blueprint-cli -- convert form.pdf \
  --instructions "Keep every footnote." --max-review-rounds 5

# Override the AEM image and data volume for this run
cargo run --release -p blueprint-cli -- convert form.pdf \
  --aem-image ajila.azurecr.io/aemforms-arm:6.5.17.0 --aem-volume my-data-volume
```

---

## 6. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Build fails in `agent/src/semantic/` on `include_bytes!` | `agent/models/` holds LFS pointers | `git lfs install && git lfs pull` |
| `No API key for the anthropic provider` | Nothing in `--api-key`, `$ANTHROPIC_API_KEY` or the app's settings | Set one of the three |
| `Several profiles are installed (…) — pick one with --profile.` | More than one profile present | Name it explicitly |
| `Verification is not possible, so the run cannot start:` | Preflight failed (Docker, images, pdfium, settings) | Run `blueprint verify check` to see what is missing; `blueprint verify prepare` pulls public images; see `docker/aem/README.md` for AEM image setup |
| `… is already being converted by another run` | Another run in the same process holds the lease on `(host, jcr_path)` | Wait for it, or target another instance |
| `The run stopped before producing a result.` | Aborted, or the retry budget ran out | Resume with `--session <ID>` |
| `Session … holds no saved form` | Continuing a session with nothing recorded | Start fresh from the sources |

---

## 7. Checking the result

A converted form joins the deployed UBS corpus, whose CI guard fails any form
that re-introduces a known systemic defect. Run it on a package a conversion wrote:

```sh
python3 scripts/check_feedback_rules.py out/AAOS_package.zip forms/AAOS_033_IT.pdf
```

Exit code 0 means every enrolled rule is clean. Pass `--feedback-repo` when the
`ajila-forms-conversion-feedback` checkout is not next to this one.
