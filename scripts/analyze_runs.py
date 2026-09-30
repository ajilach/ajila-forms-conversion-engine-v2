#!/usr/bin/env python3
"""Compare many recorded conversion runs side by side.

Every AI conversion the engine runs is recorded into a run-analysis folder
(see `runner::analysis`): a `report.md` for that one run, and a `summary.json`
holding its figures as data. This script reads the `summary.json` of many runs
and writes one overview in Markdown: where the time goes across forms, which
tools cost the most, which errors recur across forms, which calls the agents
repeat for nothing, and what the Reviewer keeps finding.

That overview is the input for questions no single run can answer — which
fixes would pay off across the remaining forms, which work a deterministic
script or a new MCP tool should take over, and which stages dominate the
wall time and so are worth parallelising.

Usage:

    # Every run under the app's default folder (macOS)
    python3 scripts/analyze_runs.py ~/Library/Application\\ Support/blueprint/run-analysis

    # CLI runs, written next to their artefacts, into a file
    python3 scripts/analyze_runs.py ./out/run-analysis --out runs-overview.md

    # Only the finished runs, and the aggregate as JSON too
    python3 scripts/analyze_runs.py ./out/run-analysis --finished-only --json overview.json

Each argument may be a run folder or any folder above run folders; every
`summary.json` found below it is read. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

SUPPORTED_SCHEMA = 1
TOP = 25


def duration(ms: float) -> str:
    ms = int(ms)
    if ms < 1000:
        return f"{ms}ms"
    secs = ms // 1000
    if secs < 60:
        return f"{ms / 1000:.1f}s"
    h, m, s = secs // 3600, (secs % 3600) // 60, secs % 60
    return f"{h}h {m:02d}m {s:02d}s" if h else f"{m}m {s:02d}s"


def percent(part: float, whole: float) -> str:
    return "-" if not whole else f"{part * 100 / whole:.0f}%"


def number(n: float) -> str:
    return f"{int(n):,}"


def usd(cost) -> str:
    return "-" if cost is None else f"USD {cost:.2f}"


def cell(text, limit: int = 120) -> str:
    text = " ".join(str(text or "").split())
    if len(text) > limit:
        text = text[:limit] + "…"
    return text.replace("|", "\\|")


def find_summaries(paths: list[Path]) -> list[Path]:
    found: list[Path] = []
    for path in paths:
        if path.is_file() and path.name == "summary.json":
            found.append(path)
        elif path.is_dir():
            found.extend(sorted(path.rglob("summary.json")))
        else:
            print(f"warning: {path} is neither a run folder nor a folder of runs", file=sys.stderr)
    # One summary per folder, in the order the runs started (folder names
    # begin with the start time).
    unique = sorted(set(found), key=lambda p: p.parent.name)
    return unique


def load(path: Path) -> dict | None:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:  # ValueError covers bad JSON and bad UTF-8
        print(f"warning: skipping {path}: {e}", file=sys.stderr)
        return None
    if not isinstance(data, dict) or not isinstance(data.get("time"), dict):
        print(f"warning: skipping {path}: not a run summary", file=sys.stderr)
        return None
    if data.get("schema_version") != SUPPORTED_SCHEMA:
        print(
            f"warning: skipping {path}: schema version {data.get('schema_version')} "
            f"(this script reads {SUPPORTED_SCHEMA})",
            file=sys.stderr,
        )
        return None
    data["_folder"] = str(path.parent)
    return data


def outcome(run: dict) -> str:
    if not run.get("finished"):
        return "unfinished"
    if not (run.get("outcome") or {}).get("produced"):
        return "no result"
    approved = run.get("approved")
    return {True: "approved", False: "not approved", None: "no verdict"}[approved]


def aggregate(runs: list[dict]) -> dict:
    stages = defaultdict(lambda: {"runs": set(), "occurrences": 0, "ms": 0, "model_ms": 0, "tool_ms": 0,
                                  "turns": 0, "cost": 0.0, "ends": defaultdict(int)})
    tools = defaultdict(lambda: {"runs": set(), "calls": 0, "failures": 0, "ms": 0, "max_ms": 0,
                                 "result_chars": 0})
    errors = defaultdict(lambda: {"runs": set(), "count": 0, "example": ""})
    repeats = defaultdict(lambda: {"runs": set(), "wasted": 0, "repeated": 0, "ms": 0})
    controls = defaultdict(lambda: {"runs": set(), "count": 0})
    for i, run in enumerate(runs):
        for s in run.get("stages", []):
            agg = stages[s["name"]]
            agg["runs"].add(i)
            agg["occurrences"] += 1
            agg["ms"] += s.get("duration_ms", 0)
            agg["model_ms"] += s.get("model_ms", 0)
            agg["tool_ms"] += s.get("tool_ms", 0)
            agg["turns"] += s.get("turns", 0)
            agg["cost"] += (s.get("spend") or {}).get("cost_usd") or 0.0
            ended = s.get("ended")
            if isinstance(ended, dict):
                ended = "error"
            agg["ends"][ended or "running"] += 1
        for t in run.get("tools", []):
            agg = tools[t["name"]]
            agg["runs"].add(i)
            agg["calls"] += t["calls"]
            agg["failures"] += t["failures"]
            agg["ms"] += t["total_ms"]
            agg["max_ms"] = max(agg["max_ms"], t["max_ms"])
            agg["result_chars"] += t["result_chars"]
        for e in run.get("recurring_errors", []):
            agg = errors[(e["tool"], e["pattern"])]
            agg["runs"].add(i)
            agg["count"] += e["count"]
            agg["example"] = agg["example"] or e.get("example", "")
        for r in run.get("repeated_calls", []):
            agg = repeats[r["name"]]
            agg["runs"].add(i)
            agg["repeated"] += r["count"] - 1
            if r["distinct_results"] == 1:
                agg["wasted"] += r["count"] - 1
            agg["ms"] += r.get("total_ms", 0)
        for kind, count in (run.get("control_counts") or {}).items():
            controls[kind]["runs"].add(i)
            controls[kind]["count"] += count
    return {"stages": stages, "tools": tools, "errors": errors, "repeats": repeats, "controls": controls}


def render(runs: list[dict], agg: dict) -> str:
    out: list[str] = []
    w = out.append
    total_wall = sum(r["time"]["wall_ms"] for r in runs)
    total_model = sum(r["time"]["model_ms"] for r in runs)
    total_tool = sum(r["time"]["tool_ms"] for r in runs)
    total_cost = sum(((r.get("spend") or {}).get("cost_usd") or 0.0) for r in runs)

    w("# Conversion runs — overview\n")
    w(f"{len(runs)} run(s), {duration(total_wall)} of wall time in total, {usd(total_cost)}.\n")
    w(f"- Waiting for the model: {duration(total_model)} ({percent(total_model, total_wall)})")
    w(f"- Running tools: {duration(total_tool)} ({percent(total_tool, total_wall)})")
    total_failed = sum(r["time"].get("failed_request_ms", 0) for r in runs)
    total_shaping = sum(r["time"].get("shaping_ms", 0) for r in runs)
    w(f"- Waiting for requests that then failed: {duration(total_failed)} ({percent(total_failed, total_wall)})")
    w(f"- Shaping requests to fit the context budget: {duration(total_shaping)} ({percent(total_shaping, total_wall)})")
    other = sum(r["time"].get("other_ms", 0) for r in runs)
    w(f"- Everything else (retry waits, operator pauses, controller): {duration(other)} ({percent(other, total_wall)})\n")

    w("## Runs\n")
    w("| Started | Form | Kind | Outcome | Wall | Model | Tools | Stages | Turns | Tool calls (failed) | Reviews | Cost | Folder |")
    w("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in runs:
        meta = r.get("run", {})
        t = r["time"]
        c = r.get("counts", {})
        code = (r.get("outcome") or {}).get("form_code") or meta.get("label", "")
        w(
            f"| {cell(meta.get('started', ''), 25)} | {cell(code, 40)} | {cell(meta.get('kind', ''), 30)} "
            f"| {outcome(r)} | {duration(t['wall_ms'])} | {percent(t['model_ms'], t['wall_ms'])} "
            f"| {percent(t['tool_ms'], t['wall_ms'])} | {c.get('stages', 0)} | {c.get('turns', 0)} "
            f"| {c.get('tool_calls', 0)} ({c.get('failed_tool_calls', 0)}) | {len(r.get('review_verdicts', []))} "
            f"| {usd((r.get('spend') or {}).get('cost_usd'))} | `{Path(r['_folder']).name}` |"
        )
    w("")

    w("## Stages across runs\n")
    w("Where the wall time goes, by kind of stage. A stage that dominates is the first candidate for "
      "splitting into parallel agents or for moving work into deterministic tools.\n")
    w("| Stage | Runs | Times run | Total time | Share of all stage time | Average per occurrence | Model | Tools | Turns (avg) | Cost | How it ended |")
    w("|---|---|---|---|---|---|---|---|---|---|---|")
    stage_total = sum(s["ms"] for s in agg["stages"].values())
    for name, s in sorted(agg["stages"].items(), key=lambda kv: -kv[1]["ms"]):
        ends = ", ".join(f"{k} ×{v}" for k, v in sorted(s["ends"].items()))
        w(
            f"| {name} | {len(s['runs'])} | {s['occurrences']} | {duration(s['ms'])} | {percent(s['ms'], stage_total)} "
            f"| {duration(s['ms'] / max(s['occurrences'], 1))} | {percent(s['model_ms'], s['ms'])} "
            f"| {percent(s['tool_ms'], s['ms'])} | {s['turns'] / max(s['occurrences'], 1):.1f} | {usd(s['cost'])} | {ends} |"
        )
    w("")

    w("## Tools across runs, by total time\n")
    w("| Tool | Runs using it | Calls | Failed | Total time | Average | Longest | Result characters |")
    w("|---|---|---|---|---|---|---|---|")
    for name, t in sorted(agg["tools"].items(), key=lambda kv: -kv[1]["ms"])[:TOP * 2]:
        w(
            f"| `{name}` | {len(t['runs'])} | {number(t['calls'])} | {number(t['failures'])} | {duration(t['ms'])} "
            f"| {duration(t['ms'] / max(t['calls'], 1))} | {duration(t['max_ms'])} | {number(t['result_chars'])} |"
        )
    w("")

    if agg["errors"]:
        w("## Errors that recur across forms\n")
        w("The same tool failing the same way (numbers and quoted names folded). One that recurs across "
          "many forms is a systemic problem: fix it once in a prompt, a tool, a template or a deterministic "
          "post-processing script instead of letting every run rediscover it.\n")
        w("| Tool | Runs | Times | Error pattern | Example |")
        w("|---|---|---|---|---|")
        ranked = sorted(agg["errors"].items(), key=lambda kv: (-len(kv[1]["runs"]), -kv[1]["count"]))
        for (tool, pattern), e in ranked[:TOP * 2]:
            w(f"| `{tool}` | {len(e['runs'])} | {e['count']} | {cell(pattern, 140)} | {cell(e['example'], 180)} |")
        w("")

    if agg["repeats"]:
        w("## Repeated identical calls across runs\n")
        w("Calls made again with exactly the same arguments. \"Learned nothing\" counts the repeats that got "
          "back the same result as before — pure waste, usually the agent losing track after context "
          "shaping, or a tool whose answer it does not trust.\n")
        w("| Tool | Runs | Repeats | Learned nothing | Time in repeated calls |")
        w("|---|---|---|---|---|")
        for name, r in sorted(agg["repeats"].items(), key=lambda kv: -kv[1]["wasted"])[:TOP]:
            w(f"| `{name}` | {len(r['runs'])} | {r['repeated']} | {r['wasted']} | {duration(r['ms'])} |")
        w("")

    if agg["controls"]:
        w("## Control events across runs\n")
        w("| Kind | Runs | Times |")
        w("|---|---|---|")
        for kind, c in sorted(agg["controls"].items(), key=lambda kv: -kv[1]["count"]):
            w(f"| {kind} | {len(c['runs'])} | {c['count']} |")
        w("")

    findings = [(r, v) for r in runs for v in r.get("review_verdicts", []) if v.get("approved") is False]
    if findings:
        w("## What the Reviewer asked to change\n")
        w("Every rejecting verdict, so recurring findings across forms stand out — each is something the "
          "Author could be taught, or a check a tool could do before the review.\n")
        for r, v in findings:
            code = (r.get("outcome") or {}).get("form_code") or r.get("run", {}).get("label", "")
            w(f"### {code} — round {v.get('round', '?')}: {v.get('verdict', '')}\n")
            report = (v.get("report") or "").strip() or "(no report)"
            if len(report) > 1500:
                report = report[:1500] + "…"
            w("\n".join(f"> {line}" for line in report.splitlines()) + "\n")

    w("## Using this overview\n")
    w("- Each run's own `report.md`, `timeline.md` and `transcript/` (in the folder named in the Runs table) "
      "explain the figures above; `trace.jsonl` there has everything.")
    w("- Questions worth giving an AI assistant together with this file: which three changes would save the "
      "most wall time across these runs? Which recurring errors should become deterministic fixes? Which "
      "tools return far more than the agents use? Which stages could run in parallel?")
    return "\n".join(out) + "\n"


def to_json(runs: list[dict], agg: dict) -> dict:
    def plain(d):
        return {
            str(k) if not isinstance(k, tuple) else " | ".join(k): {
                kk: (sorted(vv) if isinstance(vv, set) else dict(vv) if isinstance(vv, defaultdict) else vv)
                for kk, vv in v.items()
            }
            for k, v in d.items()
        }

    return {
        "runs": [{k: v for k, v in r.items() if k in ("run", "outcome", "approved", "time", "spend", "counts", "_folder")}
                 for r in runs],
        **{name: plain(table) for name, table in agg.items()},
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="+", type=Path, help="run folders, or folders containing them")
    parser.add_argument("--out", type=Path, help="write the Markdown overview here instead of stdout")
    parser.add_argument("--json", type=Path, help="also write the aggregate as JSON")
    parser.add_argument("--finished-only", action="store_true", help="skip runs that did not finish")
    args = parser.parse_args(argv)

    runs = [r for r in (load(p) for p in find_summaries(args.paths)) if r is not None]
    if args.finished_only:
        runs = [r for r in runs if r.get("finished")]
    if not runs:
        print("No recorded runs found.", file=sys.stderr)
        return 1

    agg = aggregate(runs)
    text = render(runs, agg)
    if args.out:
        args.out.write_text(text, encoding="utf-8")
        print(f"Wrote {args.out} ({len(runs)} runs)")
    else:
        sys.stdout.write(text)
    if args.json:
        args.json.write_text(json.dumps(to_json(runs, agg), indent=2, default=str), encoding="utf-8")
        print(f"Wrote {args.json}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
