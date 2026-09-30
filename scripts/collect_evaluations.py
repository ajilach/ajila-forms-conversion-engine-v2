#!/usr/bin/env python3
"""Collect the human evaluations of recorded conversion runs into one overview.

Every run folder written by the run analyzer (see `docs/run-analysis.md`) holds
an `evaluation.md` template. Once a person has checked the converted form and
filled it in, this script joins each evaluation with its run's `summary.json`
(wall time, cost, review verdict) and writes:

- one row per run: form, verdict, score, minutes of manual fixing, run time,
  cost, the Reviewer's verdict, number of findings;
- the scores per area across runs;
- findings counted by category and severity, then every finding;
- every "Requirements for v3" line, with the run it came from;
- the runs whose evaluation is still an unfilled template.

Usage:

    python3 scripts/collect_evaluations.py run-analysis
    python3 scripts/collect_evaluations.py run-analysis --out evaluations-overview.md --csv evaluations.csv

Each argument may be a run folder or any folder above run folders. Standard
library only.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

EVALUATION_FILE = "evaluation.md"
VERDICTS = ("usable", "small-fixes", "major-rework", "unusable")
SEVERITIES = ("blocker", "major", "minor")


def duration(ms) -> str:
    if not ms:
        return "-"
    secs = int(ms) // 1000
    h, m, s = secs // 3600, (secs % 3600) // 60, secs % 60
    return f"{h}h {m:02d}m" if h else f"{m}m {s:02d}s"


def usd(cost) -> str:
    return "-" if cost is None else f"USD {cost:.2f}"


def cell(text, limit: int = 160) -> str:
    text = " ".join(str(text if text is not None else "").split()).replace("|", "\\|")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def find_evaluations(paths: list[Path]) -> list[Path]:
    found: set[Path] = set()
    for path in paths:
        if path.is_file() and path.name == EVALUATION_FILE:
            found.add(path.resolve())
        elif path.is_dir():
            found.update(p.resolve() for p in path.rglob(EVALUATION_FILE))
    return sorted(found)


def front_matter(text: str) -> tuple[dict, str]:
    """The `key: value` lines between the leading `---` fences, and the body."""
    match = re.match(r"^---\n(.*?)\n---\n?(.*)$", text, re.S)
    if not match:
        return {}, text
    fields = {}
    for line in match.group(1).splitlines():
        if line.lstrip().startswith("#") or ":" not in line:
            continue
        key, value = line.split(":", 1)
        fields[key.strip()] = value.strip()
    return fields, match.group(2)


def section(body: str, title: str) -> str:
    """The text under `## <title>` up to the next `## ` heading."""
    match = re.search(rf"^## {re.escape(title)}\s*\n(.*?)(?=^## |\Z)", body, re.S | re.M)
    if not match:
        return ""
    return re.sub(r"<!--.*?-->", "", match.group(1), flags=re.S).strip()


def table_rows(text: str) -> list[list[str]]:
    """Data rows of the first Markdown table in `text` (header and rule skipped)."""
    rows = []
    lines = [l.strip() for l in text.splitlines() if l.strip().startswith("|")]
    for line in lines[2:]:
        cells = [c.strip() for c in line.strip("|").split("|")]
        rows.append(cells)
    return rows


def number(value: str):
    try:
        return float(value) if "." in value else int(value)
    except (TypeError, ValueError):
        return None


def load(path: Path) -> dict:
    text = path.read_text(encoding="utf-8")
    fields, body = front_matter(text)
    folder = path.parent
    summary = {}
    summary_path = folder / "summary.json"
    if summary_path.is_file():
        try:
            summary = json.loads(summary_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            summary = {}

    scores = []
    for row in table_rows(section(body, "Scores")):
        if len(row) >= 2 and row[0]:
            scores.append({"area": row[0], "score": number(row[1]), "notes": row[2] if len(row) > 2 else ""})

    findings = []
    for row in table_rows(section(body, "Findings")):
        row = row + [""] * (6 - len(row))
        if not any(row[1:6]):
            continue  # the template's empty row
        findings.append(
            {
                "n": row[0],
                "category": row[1].strip("`").lower() or "uncategorised",
                "severity": row[2].strip("`").lower() or "unrated",
                "where": row[3],
                "what": row[4],
                "fix": row[5],
            }
        )

    requirements = []
    for line in section(body, "Requirements for v3").splitlines():
        line = line.strip()
        if line.startswith("- R:") and line[4:].strip():
            requirements.append(line[4:].strip())

    filled = bool(fields.get("verdict") or fields.get("score") or findings or requirements)
    return {
        "folder": folder,
        "form": fields.get("form") or (summary.get("run") or {}).get("label") or folder.name,
        "evaluator": fields.get("evaluator", ""),
        "evaluated_on": fields.get("evaluated_on", ""),
        "verdict": fields.get("verdict", "").lower(),
        "score": number(fields.get("score", "")),
        "manual_fix_minutes": number(fields.get("manual_fix_minutes", "")),
        "model": fields.get("model") or (summary.get("run") or {}).get("model", ""),
        "summary_text": section(body, "Summary"),
        "how_it_went": section(body, "How the run went"),
        "scores": scores,
        "findings": findings,
        "requirements": requirements,
        "filled": filled,
        "wall_ms": (summary.get("time") or {}).get("wall_ms"),
        "cost_usd": (summary.get("spend") or {}).get("cost_usd"),
        "approved": summary.get("approved"),
        "finished": summary.get("finished"),
    }


def reviewer(run: dict) -> str:
    if run["finished"] is False:
        return "stopped"
    return {True: "approved", False: "not approved"}.get(run["approved"], "no verdict")


def render(runs: list[dict]) -> str:
    done = [r for r in runs if r["filled"]]
    pending = [r for r in runs if not r["filled"]]
    out = ["# Human evaluation of conversion runs", ""]
    out.append(
        f"{len(done)} evaluated run(s), {len(pending)} still to evaluate, "
        f"{sum(len(r['findings']) for r in done)} finding(s), "
        f"{sum(len(r['requirements']) for r in done)} requirement(s) for v3."
    )
    if done:
        verdicts = Counter(r["verdict"] or "-" for r in done)
        out.append("Verdicts: " + ", ".join(f"{v} {verdicts[v]}" for v in (*VERDICTS, "-") if verdicts[v]) + ".")
        scores = [r["score"] for r in done if isinstance(r["score"], (int, float))]
        minutes = [r["manual_fix_minutes"] for r in done if isinstance(r["manual_fix_minutes"], (int, float))]
        if scores:
            out.append(f"Average score {sum(scores) / len(scores):.1f} of 5.")
        if minutes:
            out.append(f"Manual fixing: {sum(minutes):g} min in total, {sum(minutes) / len(minutes):.0f} min per run.")
    out.append("")

    out += ["## Runs", "", "| Form | Verdict | Score | Fix (min) | Run time | Cost | Reviewer | Findings (B/M/m) | Folder |", "|---|---|---|---|---|---|---|---|---|"]
    for r in done:
        sev = Counter(f["severity"] for f in r["findings"])
        out.append(
            f"| {cell(r['form'], 60)} | {r['verdict'] or '-'} | {r['score'] if r['score'] is not None else '-'} "
            f"| {r['manual_fix_minutes'] if r['manual_fix_minutes'] is not None else '-'} | {duration(r['wall_ms'])} "
            f"| {usd(r['cost_usd'])} | {reviewer(r)} | {len(r['findings'])} ({sev['blocker']}/{sev['major']}/{sev['minor']}) "
            f"| `{r['folder'].name}` |"
        )
    out.append("")

    areas: dict[str, list] = defaultdict(list)
    for r in done:
        for s in r["scores"]:
            if isinstance(s["score"], (int, float)):
                areas[s["area"]].append(s["score"])
    if areas:
        out += ["## Scores per area", "", "| Area | Average | Runs | Lowest |", "|---|---|---|---|"]
        for area, values in sorted(areas.items(), key=lambda kv: sum(kv[1]) / len(kv[1])):
            out.append(f"| {cell(area, 80)} | {sum(values) / len(values):.1f} | {len(values)} | {min(values)} |")
        out.append("")

    findings = [(r, f) for r in done for f in r["findings"]]
    if findings:
        by_cat: dict[str, Counter] = defaultdict(Counter)
        for _, f in findings:
            by_cat[f["category"]][f["severity"]] += 1
        out += ["## Findings by category", "", "| Category | Total | Blocker | Major | Minor | Forms |", "|---|---|---|---|---|---|"]
        for cat, counts in sorted(by_cat.items(), key=lambda kv: -sum(kv[1].values())):
            forms = len({r["form"] for r, f in findings if f["category"] == cat})
            out.append(
                f"| {cat} | {sum(counts.values())} | {counts['blocker']} | {counts['major']} | {counts['minor']} | {forms} |"
            )
        out += ["", "## All findings", "", "| Form | # | Category | Severity | Where | What is wrong | Fix |", "|---|---|---|---|---|---|---|"]
        order = {s: i for i, s in enumerate(SEVERITIES)}
        for r, f in sorted(findings, key=lambda rf: (order.get(rf[1]["severity"], 9), rf[1]["category"])):
            out.append(
                f"| {cell(r['form'], 40)} | {cell(f['n'], 4)} | {f['category']} | {f['severity']} | {cell(f['where'], 60)} "
                f"| {cell(f['what'])} | {cell(f['fix'], 100)} |"
            )
        out.append("")

    reqs = [(r, q) for r in done for q in r["requirements"]]
    if reqs:
        out += ["## Requirements for v3", ""]
        for r, q in reqs:
            out.append(f"- {q} — *{cell(r['form'], 60)}*")
        out.append("")

    notes = [r for r in done if r["how_it_went"]]
    if notes:
        out += ["## How the runs went", ""]
        for r in notes:
            out.append(f"- **{cell(r['form'], 60)}**: {cell(r['how_it_went'], 600)}")
        out.append("")

    if pending:
        out += ["## Not evaluated yet", ""]
        out += [f"- `{r['folder'].name}` — {cell(r['form'], 80)}" for r in pending]
        out.append("")
    return "\n".join(out)


def write_csv(runs: list[dict], path: Path) -> None:
    with path.open("w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["folder", "form", "evaluator", "evaluated_on", "verdict", "score", "manual_fix_minutes",
                    "wall_minutes", "cost_usd", "reviewer", "findings", "blockers", "majors", "minors", "requirements", "model"])
        for r in runs:
            if not r["filled"]:
                continue
            sev = Counter(f["severity"] for f in r["findings"])
            w.writerow([r["folder"].name, r["form"], r["evaluator"], r["evaluated_on"], r["verdict"], r["score"],
                        r["manual_fix_minutes"], round(r["wall_ms"] / 60000, 1) if r["wall_ms"] else "",
                        round(r["cost_usd"], 2) if r["cost_usd"] is not None else "", reviewer(r), len(r["findings"]),
                        sev["blocker"], sev["major"], sev["minor"], len(r["requirements"]), r["model"]])


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="+", type=Path, help="run folders, or folders above them")
    parser.add_argument("--out", type=Path, help="write the overview here instead of stdout")
    parser.add_argument("--csv", type=Path, help="also write one row per evaluated run as CSV")
    args = parser.parse_args(argv)

    files = find_evaluations(args.paths)
    if not files:
        print("no evaluation.md found under " + ", ".join(map(str, args.paths)), file=sys.stderr)
        return 1
    runs = [load(p) for p in files]
    runs.sort(key=lambda r: r["folder"].name)
    text = render(runs)
    if args.out:
        args.out.write_text(text + "\n", encoding="utf-8")
    else:
        print(text)
    if args.csv:
        write_csv(runs, args.csv)
    return 0


if __name__ == "__main__":
    sys.exit(main())
