# Run analysis

Every AI conversion — from the desktop app or from `blueprint convert` — is
recorded into a folder of its own: where the run spent its time, what every
agent was told and did, which calls it repeated, which errors recurred and what
the Reviewer found. The files are plain Markdown and JSON Lines, written for a
person to read and for an AI assistant to be handed as they are.

The point is to answer, after the fact, the questions a long run raises:
where did the time go, where did an agent go in circles, which failures could a
deterministic script fix instead of the model, which work could a new MCP tool
take over, and which parts of a form could agents build in parallel.

## Where the folders go

| Started from | Default location | Change it with |
|---|---|---|
| Desktop app | `run-analysis/` at the root of this repository (the checkout the app was built from) | Settings → Run analysis → Folder |
| `blueprint convert` | the same `run-analysis/` folder | `--analysis-dir <DIR>` |

A binary moved away from its checkout (a bundled app) falls back to
`<config dir>/blueprint/run-analysis/` — on macOS
`~/Library/Application Support/blueprint/run-analysis/`. The folder is not in
`.gitignore`: commit a run's folder when it should be shared, but mind its size.

Recording is on by default. Switch it off in the app under Settings → Run
analysis, or with `--no-analysis` on the CLI. The CLI decides for itself from
its flags: the app's setting does not apply to a console run. A run's folder is
named `<date>_<time>_<source file>_<session>` (with `-2`, `-3`, … when that
name is taken); the app and the console both print it when the run starts and
again when it ends.

Nothing is deleted automatically. A long run of a complex form can leave
tens of megabytes, mostly in `trace.jsonl` and the transcripts; delete old
folders by hand when they are no longer needed.

## What is in a folder

| File | What it is |
|---|---|
| `README.md` | What each file is, how to read `trace.jsonl`, and questions to ask an AI assistant about the run. |
| `report.md` | The analysis: time split between the model, the tools, failed requests, context shaping and everything else; a table per stage and per tool; failed requests; the slowest model turns and tool calls; repeated identical calls and long streaks of one tool; recurring errors; the largest tool results; edit activity per stage; every review verdict with its report; every control event (nudges, retries, stuck-watch stops, exhausted budgets). Rewritten after every stage and every review verdict, so an unfinished run has one too. |
| `timeline.md` | Every model turn and tool call in order, with the time since the start, duration, tokens, cost and short excerpts. |
| `transcript/NN-<Stage>.md` | One file per stage, split into parts of about 1.5 MB: the system prompt, every model message and reasoning, every tool call's arguments and result in full (a single block is cut at 30,000 characters, with a pointer to the complete text in the trace). |
| `trace.jsonl` | Everything, one JSON object per line: nothing is truncated except images, which are replaced by a placeholder with their size. |
| `summary.json` | The report's figures as data, for comparing runs. |
| `evaluation.md` | The human evaluation, filled in by hand after checking the converted form: verdict, overall score, minutes of manual fixing, scores per area, one row per finding, notes on how the run went and requirements for v3. Written once as a template when the run starts and never overwritten. |

Every entry that names an event carries `#seq`, the line number of that event
in `trace.jsonl`, so a row in the report leads straight to the full record.

The files are written as the run goes and flushed line by line: a run that is
aborted, crashes or is killed still leaves everything up to that moment.
Recording never stops a conversion. If the trace, timeline or a transcript
cannot be written, recording stops for that run with one warning and the
conversion carries on; if only the report cannot be rewritten (a viewer holding
it open on Windows, say), that is one warning and the next stage tries again.
The files are written synchronously as events happen, which costs
milliseconds against model turns that take seconds to minutes.

## Comparing runs

`scripts/analyze_runs.py` reads the `summary.json` of any number of runs and
writes one overview: time per stage kind across forms, tool cost across forms,
errors that recur across forms, repeated calls that learned nothing, control
events, and every change the Reviewer asked for.

```sh
python3 scripts/analyze_runs.py run-analysis --out runs-overview.md
python3 scripts/analyze_runs.py run-analysis --finished-only --json overview.json
```

## Human evaluation

Each run folder starts with an `evaluation.md` template. After checking the
result in AEM (preview, DoR) and Redacto against the source PDF, fill in:

1. the front matter — `evaluator`, `evaluated_on`, `verdict` (`usable`,
   `small-fixes`, `major-rework`, `unusable`), `score` (1–5) and
   `manual_fix_minutes`;
2. the scores per area (1–5, `-` when not applicable);
3. one row per error under **Findings**, with a category from the list in the
   file and a severity (`blocker`, `major`, `minor`);
4. **How the run went** — stuck stages, tool errors, freezes, restarts;
5. **Requirements for v3** — one `- R:` line each, naming the finding it comes
   from.

`scripts/collect_evaluations.py` joins every filled-in `evaluation.md` with its
run's `summary.json` (wall time, cost, review verdict) and writes one overview:
a row per run, findings by category and severity, every finding, and every v3
requirement with the run it came from. Templates still unfilled are listed as
pending.

```sh
python3 scripts/collect_evaluations.py run-analysis --out evaluations-overview.md --csv evaluations.csv
```

## Using the files with an AI assistant

Give it `runs-overview.md` (or one run's `report.md`) first; both are short by
design. Then hand it only what it asks for: `timeline.md` for order and timing,
one stage's transcript at a time for the reasoning behind a step, and
`trace.jsonl` through a tool that can search it. Each run's `README.md` lists
questions that work well.

## How it works

The controller (`pipeline`) reports a structured trace next to its progress
events: `RunObserver::trace` receives a `pipeline::TraceEvent` for every stage
start and end, every attempt, every request (how long shaping it took), every
model turn (latency, tokens, cost, finish reason, text, reasoning, the tool
calls it asked for with full arguments), every request that failed and how long
it took to fail, every tool call (duration, full result, a stable hash of it),
every review verdict and every control decision. Durations are measured there with a monotonic clock; the default
`trace` does nothing, so observers that do not record are unaffected.

`runner::analysis::RecordingObserver` wraps the caller's observer for the
length of a run (`runner::run::drive`), writes each event into the folder and
passes it on unchanged, so the app and the CLI get the recording without doing
anything.

The recording is independent of the edit history's `conversations` table:
that table holds what a stage resumes from, this folder holds what happened,
with timing, every attempt and every control decision.
