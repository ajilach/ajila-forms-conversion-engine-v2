---
form: TEST_001_DE.pdf
session: it-session
run_folder: 2026-10-08_093605_TEST_001_DE_pdf_it-sessi
started: 2026-09-30T14:00:00+02:00
model: scripted mock
evaluator:
evaluated_on:
# usable | small-fixes | major-rework | unusable
verdict:
# overall 1-5 (5 = deliverable as is)
score:
# minutes a person needed (or would need) to make the result deliverable
manual_fix_minutes:
---

# Human evaluation — TEST_001_DE.pdf

Fill this in after checking the converted form (AEM preview, DoR, Redacto) against
the source PDF. Keep the front matter keys and the table columns as they are:
`scripts/collect_evaluations.py` reads them across all runs. `report.md` in this
folder shows where the run spent its time; cite its `#seq` numbers where useful.

## Summary

<!-- Two or three sentences: what is good, what is wrong, would you ship it. -->

## Scores

1 = wrong or missing, 3 = right after small fixes, 5 = right as produced, `-` = not
applicable.

| Area | Score | Notes |
|---|---|---|
| Structure and sections (panels, wizard pages, order) | | |
| Texts and labels (every language, formatting) | | |
| Fields (types, options, required, lengths) | | |
| Behaviour (show/hide, repeatables, set values, scripts) | | |
| UBS conventions (header, PN_BR, signatures, internal bank use) | | |
| DoR / PDF output | | |
| Redacto output | | |

## Findings

One row per error found. **Category**: `source-reading`, `structure`, `text`,
`field`, `behaviour`, `ubs-convention`, `dor`, `redacto`, `verification`,
`pipeline`, `other`. **Severity**: `blocker` (form unusable), `major` (wrong for
the customer), `minor` (cosmetic). **Fix**: what you did by hand, and how long it
took.

| # | Category | Severity | Where (panel / field) | What is wrong | Fix |
|---|---|---|---|---|---|
| 1 | | | | | |

## How the run went

<!-- What you noticed while it ran or in report.md: stuck stages, tool errors,
time sinks, missing review verdict, the app freezing, restarts. -->

## Requirements for v3

One line each, starting with `- R:`. Say what v3 must do and which finding it
comes from, e.g. `- R: set min=max occurrences of signature panels from the
source (finding 3)`.

- R:
