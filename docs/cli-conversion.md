# Converting a form from the command line

`blueprint convert` runs the same autonomous conversion the desktop app runs —
the `pipeline` controller (Analyst → Author → Reviewer → fix rounds) over the
shared `runner` transport, the same tool catalog, the same edit-history SQLite
database. Only the reporting and the output location differ. A run started here
can be reopened in the app, and vice versa.

This guide covers the `convert` subcommand and everything needed to get it
running. For the deterministic export run (bare arguments, no model involved),
see the CLI section in the [README](../README.md).

---

## 1. Prerequisites

### 1.1 Toolchain

| Requirement | Why | Check |
|---|---|---|
| Rust 1.88+ (edition 2024) | Workspace `rust-version`; let-chains are used throughout | `rustc --version` |
| Git LFS | `core/models/` holds a 235 MB safetensors model and a 17 MB tokenizer, both `include_bytes!`-embedded into the binary at compile time | `git lfs version` |

Install both, then pull the large files. Without the LFS pull the build embeds
LFS pointer text instead of the model and fails:

```sh
brew install git-lfs
git lfs install
git lfs pull
```

Verify the model is real and not a pointer — it must be hundreds of megabytes:

```sh
ls -lh core/models/model.safetensors
```

The Dioxus CLI is **not** needed. That is only for the desktop app.

### 1.2 Build

```sh
cargo build --release -p blueprint-cli
```

The binary lands at `target/release/blueprint`. Expect a long first build and a
large binary — the embedded semantic-matching model (`semantic-matching` is a
default feature of `core`, and the CLI enables it explicitly) accounts for most
of it.

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

A profile supplies the AEM config, parser fonts, XSD types and reference
library. `ubs` is currently the only one installed, so `--profile` can be
omitted — the CLI picks it automatically when exactly one profile exists. It
errors rather than guessing if several are installed, and a resumed session
inherits the profile it was created with.

### 1.5 Optional: AEM and the browser

Only needed with `--upload`. Without that flag the run touches nothing outside
the machine: no upload, and the agent gets no AEM fetch or verify tools at all.

With `--upload` you need:

- An AEM author instance the configured user can log in to.
- Node.js 18+ with `npx` on `PATH` (or pass `--npx /path/to/npx`).
- Google Chrome installed at a standard location.

Node and Chrome are for the browser verification the Author and Reviewer use.
The run spawns the pinned Playwright MCP server (`PLAYWRIGHT_MCP_VERSION` in
`agent/src/browser.rs`, currently 0.0.79) as a child process, logs it in to AEM,
and the two stages open the deployed form's preview, walk every wizard page,
fill fields, submit, and read the PDF the submission downloads. Chrome runs
headless and isolated; nothing has to be started beforehand and nothing survives
the run.

Warm the npm cache once, with a network connection:

```sh
# Node, Chrome and the npm cache. Needs no AEM.
cargo run --release -p blueprint-cli -- browser prepare

# The full preflight a run performs, including the AEM login.
cargo run --release -p blueprint-cli -- browser check
```

`browser check` reads the AEM host and credentials from the desktop app's
settings; it has no flags for them. On a CLI-only machine use `browser prepare`
for the machine-side checks and let the run itself do the AEM half.

Every run with the browser enabled repeats the preflight before it starts and
refuses to run when it fails, naming the reason and the fix. It never degrades
silently. Turn it off for a run with `--no-browser`; the agent's
`fetch_aem_dor_pdf` fallback stays available.

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

1. **Preflight.** Resolve the profile and settings, then — for an AEM target with
   the browser on — run the browser preflight. A refused preflight leaves no
   session behind and spends no tokens.
2. **Claim the form.** An AEM run takes a process-wide lease on
   `(host, jcr_path)`. Two runs of the same form against the same instance would
   overwrite each other, and the Reviewer would verify the wrong one, so the
   second is refused. The lease covers runs inside one process (the app, the
   CLI), not two separate processes pointed at one instance.
3. **Open the session.** Sources are hashed and stored content-addressed, a
   session row is created and an empty initial edit is recorded.
4. **Analyst → Author → (Reviewer → Author fix)\*.** The review rounds are capped
   by `--max-review-rounds` (default 3).
5. **Finalize.** `build_aem_package` runs as a visible step; with an AEM
   connection and nothing uploaded yet, `upload_to_aem` follows. Then the
   envelope, packages, XSD and Redacto SQL are assembled and recorded back into
   the history.

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
Uploaded to AEM: /content/forms/af/…
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
| both | `agent-log-<code>.md` — the Markdown run transcript |
| with `--structured` | `structured-<code>.json` |

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
| `--structured` | off | Also write the structured document as JSON |
| `--provider <anthropic\|openai>` | the app's setting | `openai` means any OpenAI-compatible endpoint |
| `--base-url <URL>` | `https://openrouter.ai/api/v1` | Implies `--provider openai` |
| `--api-key <KEY>` | `$ANTHROPIC_API_KEY` / `$OPENAI_API_KEY`, then the app | |
| `--model <ID>` | the app's setting, then `claude-opus-5` | |
| `--max-review-rounds <N>` | 3 | Reviewer → Author-fix rounds before finalizing |
| `--instructions <TEXT>` | the app's setting | Appended to every role's system prompt |
| `--instructions-file <PATH>` | — | Conflicts with `--instructions` |
| `--retries <N>` | 2 | Operator-level retries after the controller's own |
| `--upload` | off | Enables the AEM fetch/verify tools **and** the upload |
| `--aem-host <URL>` | the app's setting | Requires `--upload` |
| `--aem-user <NAME>` | the app's setting | Requires `--upload` |
| `--aem-password <PW>` | the app's setting | Requires `--upload` |
| `--no-browser` | off | Requires `--upload` |
| `--npx <PATH>` | the app's setting, then auto-detect | Requires `--upload` |
| `--session <ID>` | — | Resume; skips the Analyst |
| `--feedback <TEXT>` | — | Requires `--session` |

`--upload` is what carries the AEM connection into the run. Without it the host
is blanked regardless of what the app has saved, so the four AEM flags are
rejected on their own rather than quietly ignored.

---

## 5. Worked examples

```sh
# Redacto document instead of an AEM package
cargo run --release -p blueprint-cli -- convert form.pdf --target redacto

# Somewhere else, with the structured JSON alongside
cargo run --release -p blueprint-cli -- convert form.pdf --out ./out --structured

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

# Upload to a local author instance, with browser verification
cargo run --release -p blueprint-cli -- convert form.pdf --upload \
  --aem-host http://localhost:4502 --aem-user admin --aem-password admin

# Upload, but skip the browser click-through
cargo run --release -p blueprint-cli -- convert form.pdf --upload --no-browser
```

---

## 6. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Build fails in `core/src/semantic/` on `include_bytes!` | `core/models/` holds LFS pointers | `git lfs install && git lfs pull` |
| `No API key for the anthropic provider` | Nothing in `--api-key`, `$ANTHROPIC_API_KEY` or the app's settings | Set one of the three |
| `Several profiles are installed (…) — pick one with --profile.` | More than one profile present | Name it explicitly |
| `--upload needs an AEM host and user` | No host/user from flags or the app | Pass `--aem-host` and `--aem-user` |
| `Browser verification is not possible: …` | Preflight failed (Node, Chrome, npm cache or AEM login) | Run `browser prepare`, then `browser check`; or `--no-browser` |
| `npx (Node.js 18+) was not found` | A Finder-launched process sees a minimal `PATH` | `--npx /opt/homebrew/bin/npx` |
| `… is already being converted by another run` | Another run in the same process holds the lease on `(host, jcr_path)` | Wait for it, or target another instance |
| `The run stopped before producing a result.` | Aborted, or the retry budget ran out | Resume with `--session <ID>` |
| `Session … holds no saved form` | Continuing a session with nothing recorded | Start fresh from the sources |

---

## 7. Checking the result

A converted form joins the deployed UBS corpus, whose CI guard fails any form
that re-introduces a known systemic defect. Run it on a fresh conversion without
importing anything:

```sh
python3 scripts/check_feedback_rules.py core/input/AAOS_033_IT.pdf
python3 scripts/check_feedback_rules.py --json core/input/AAOS_033_IT.pdf > report.json
```

Exit code 0 means every enrolled rule is clean. Pass `--feedback-repo` when the
`ajila-forms-conversion-feedback` checkout is not next to this one.
