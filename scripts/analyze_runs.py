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

Runs recorded with schema 2 also carry their judges (the agents that decide
the rules without a script): how many ran, what they cost, which rules they
judged and how each stage's cost splits into its own turns and its judges'.
A schema-1 run recorded none of that, so its judge figures show as "-".

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

SUPPORTED_SCHEMAS = (1, 2)
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
    if data.get("schema_version") not in SUPPORTED_SCHEMAS:
        print(
            f"warning: skipping {path}: schema version {data.get('schema_version')} "
            f"(this script reads {', '.join(map(str, SUPPORTED_SCHEMAS))})",
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


def has_judges(run: dict) -> bool:
    """Whether the run recorded its judges (schema 2 on): a run that did not
    has no judge figures, which is not the same as zero."""
    return "judge_runs" in run


def cost_of(spend) -> float:
    return (spend or {}).get("cost_usd") or 0.0


def stage_end(stage: dict) -> str:
    """How a stage ended, naming the terminal tool when one ended it."""
    ended = stage.get("ended")
    if isinstance(ended, dict):
        return "error"
    if ended == "review_submitted" and stage.get("ended_by"):
        return stage["ended_by"]
    return ended or "running"


def aggregate(runs: list[dict]) -> dict:
    stages = defaultdict(lambda: {"runs": set(), "occurrences": 0, "ms": 0, "model_ms": 0, "tool_ms": 0,
                                  "turns": 0, "cost": 0.0, "agent_cost": 0.0, "judge_cost": 0.0,
                                  "judged_cost": 0.0, "judged_occurrences": 0, "judge_runs": 0,
                                  "ends": defaultdict(int)})
    judges = defaultdict(lambda: {"runs": set(), "judge_runs": 0, "positive": 0, "negative": 0, "unchecked": 0,
                                  "turns": 0, "ms": 0, "cost": 0.0, "title": ""})
    tools = defaultdict(lambda: {"runs": set(), "calls": 0, "failures": 0, "ms": 0, "max_ms": 0,
                                 "result_chars": 0})
    errors = defaultdict(lambda: {"runs": set(), "count": 0, "example": ""})
    repeats = defaultdict(lambda: {"runs": set(), "wasted": 0, "repeated": 0, "ms": 0})
    controls = defaultdict(lambda: {"runs": set(), "count": 0})
    for i, run in enumerate(runs):
        # The final rule check runs after the last stage: no stage of its own
        # in the trace, but a row of its own here.
        final_check = (run.get("judges") or {}).get("final_check")
        for s in run.get("stages", []) + ([final_check] if final_check else []):
            agg = stages[s["name"]]
            agg["runs"].add(i)
            agg["occurrences"] += 1
            agg["ms"] += s.get("duration_ms", 0)
            agg["model_ms"] += s.get("model_ms", 0)
            agg["tool_ms"] += s.get("tool_ms", 0)
            agg["turns"] += s.get("turns", 0)
            agg["cost"] += cost_of(s.get("spend"))
            # The split is known only for ended stages of runs that recorded
            # their judges; its own total keeps Agent + Judges = Cost.
            if "judges_spend" in s and s.get("ended") is not None:
                agg["judged_occurrences"] += 1
                agg["judged_cost"] += cost_of(s.get("spend"))
                agg["judge_cost"] += cost_of(s.get("judges_spend"))
                agg["agent_cost"] += cost_of(s.get("agent_spend"))
                agg["judge_runs"] += s.get("judge_runs", 0)
            agg["ends"][stage_end(s)] += 1
        for r in (run.get("judges") or {}).get("by_rule", []):
            agg = judges[r.get("rule_name") or r.get("rule_id")]
            agg["runs"].add(i)
            agg["title"] = agg["title"] or r.get("rule_title", "")
            for key in ("positive", "negative", "unchecked", "turns"):
                agg[key] += r.get(key, 0)
            agg["judge_runs"] += r.get("runs", 0)
            agg["ms"] += r.get("total_ms", 0)
            agg["cost"] += cost_of(r.get("spend"))
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
    return {"stages": stages, "tools": tools, "errors": errors, "repeats": repeats, "controls": controls,
            "judges": judges}


def split_cell(stage: dict, cost: float, runs: int | None = None) -> str:
    """One side of a stage's agent/judges split, saying how many occurrences
    it covers when that is not all of them."""
    if not stage["judged_occurrences"]:
        return "-"
    text = usd(cost) + (f" ({runs})" if runs is not None else "")
    if stage["judged_occurrences"] < stage["occurrences"]:
        text += f" of {stage['judged_occurrences']}"
    return text


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
    w(f"- Everything else (retry waits, operator pauses, controller): {duration(other)} ({percent(other, total_wall)})")
    judged = [r for r in runs if has_judges(r)]
    if judged:
        judge_cost = sum(r.get("judge_cost_usd") or 0.0 for r in judged)
        judged_total = sum(cost_of(r.get("spend")) for r in judged)
        judge_runs = sum(r.get("judge_runs", 0) for r in judged)
        share = "-" if not judged_total else f"{judge_cost * 100 / judged_total:.0f}%"
        w(f"- Judges: {number(judge_runs)} run(s), {usd(judge_cost)} ({share} of the spend of the "
          f"{len(judged)} run(s) that recorded them)")
    w("")

    w("## Runs\n")
    w("| Started | Form | Kind | Outcome | Wall | Model | Tools | Stages | Turns | Tool calls (failed) | Reviews | Cost | Judges (runs) | Folder |")
    w("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
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
            f"| {usd((r.get('spend') or {}).get('cost_usd'))} "
            f"| {usd(r.get('judge_cost_usd')) + ' (' + str(r['judge_runs']) + ')' if r.get('judge_runs') else '-'} "
            f"| `{Path(r['_folder']).name}` |"
        )
    w("")

    w("## Stages across runs\n")
    w("Where the wall time goes, by kind of stage. A stage that dominates is the first candidate for "
      "splitting into parallel agents or for moving work into deterministic tools.\n")
    w("Agent and Judges split the cost of the occurrences that recorded their judges (schema 2, ended): "
      "\"of N\" says how many those are when it is not all of them.\n")
    w("| Stage | Runs | Times run | Total time | Share of all stage time | Average per occurrence | Model | Tools | Turns (avg) | Cost | Agent | Judges (runs) | How it ended |")
    w("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    stage_total = sum(s["ms"] for s in agg["stages"].values())
    for name, s in sorted(agg["stages"].items(), key=lambda kv: -kv[1]["ms"]):
        ends = ", ".join(f"{k} ×{v}" for k, v in sorted(s["ends"].items()))
        w(
            f"| {name} | {len(s['runs'])} | {s['occurrences']} | {duration(s['ms'])} | {percent(s['ms'], stage_total)} "
            f"| {duration(s['ms'] / max(s['occurrences'], 1))} | {percent(s['model_ms'], s['ms'])} "
            f"| {percent(s['tool_ms'], s['ms'])} | {s['turns'] / max(s['occurrences'], 1):.1f} | {usd(s['cost'])} "
            f"| {split_cell(s, s['agent_cost'])} "
            f"| {split_cell(s, s['judge_cost'], s['judge_runs'])} | {ends} |"
        )
    w("")

    if agg["judges"]:
        w("## Judges across runs, by cost\n")
        w("The rules a judge agent decides (no `check.js`). The most expensive ones that come back the same "
          "every time are the first candidates for a script; the ones often unchecked need a clearer rule.\n")
        w("| Rule | Runs | Judge runs | Positive | Negative | Unchecked | Turns (avg) | Total time | Cost |")
        w("|---|---|---|---|---|---|---|---|---|")
        for name, j in sorted(agg["judges"].items(), key=lambda kv: -kv[1]["cost"])[:TOP * 2]:
            w(
                f"| `{name}` | {len(j['runs'])} | {j['judge_runs']} | {j['positive']} | {j['negative']} "
                f"| {j['unchecked']} | {j['turns'] / max(j['judge_runs'], 1):.1f} | {duration(j['ms'])} | {usd(j['cost'])} |"
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
        "runs": [{k: v for k, v in r.items() if k in ("run", "outcome", "approved", "time", "spend", "counts", "_folder",
                                                      "judge_runs", "judge_cost_usd", "agent_cost_usd")}
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
