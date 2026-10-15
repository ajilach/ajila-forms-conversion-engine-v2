#!/usr/bin/env python3
"""Compare models stage by stage on one form: Author, Reviewer and judges.

A full conversion mixes the three roles: a weak Reviewer sends the Author into
extra fix rounds, and the judges' verdicts change what the Author edits. So a
full run cannot tell which role a cost or a defect belongs to. This script
tests each role on its own. The Author and the Reviewer run with `--no-judges`
(scripted rules only): the judges are measured, and do the judging, in phases
of their own, where they cost one check per form instead of one per call.

1. author   Each Author candidate converts the form with no review round and
            no judge (`convert --max-review-rounds 0 --no-judges`). Its
            scripted rules are the first quality figure.
2. score    The baseline judges judge every Author result once
            (`--assess --max-review-rounds 0`, no AEM): the judged rules that
            fail are the second quality figure.
3. grade    The baseline Reviewer assesses every Author result once
            (`--assess --max-review-rounds 1 --no-judges`): how many issues it
            finds is the third.
4. review   Each Reviewer candidate assesses the same fixed Author result(s).
            What it found is collected side by side for grading by hand.
5. judge    Each judge candidate judges every judged rule on the same fixed
            Author result(s). Agreement with the baseline judges and between
            repeats is computed.
6. report   CSV per phase and `report.md`: cost, time and quality per model.

Every run is one `blueprint convert` process with its own analysis folder
(`summary.json`), listed in `<out>/runs.jsonl`. A run that already finished is
skipped, so an interrupted benchmark carries on where it stopped. The author,
grade and review runs go one at a time: each drives the one AEM verifier. The
score and judge phases boot no AEM and may run with --jobs, also beside an AEM
phase in another terminal.

The form must not be among the tool's reference forms, or the Author starts
from its own answer. `refs` finds it in the reference library and removes it,
keeping a copy to restore:

    python3 scripts/stage_benchmark.py refs list    --config scripts/stage_benchmark.example.json
    python3 scripts/stage_benchmark.py refs remove  --config scripts/stage_benchmark.example.json
    python3 scripts/stage_benchmark.py refs restore --config scripts/stage_benchmark.example.json

Running (close the desktop app first; it shares history.db):

    export OPENROUTER_API_KEY=sk-or-...
    cargo build --release -p blueprint-cli
    python3 scripts/stage_benchmark.py run --config scripts/stage_benchmark.example.json --dry-run
    python3 scripts/stage_benchmark.py run --config scripts/stage_benchmark.example.json
    python3 scripts/stage_benchmark.py run --config ... --phase judge --jobs 3
    python3 scripts/stage_benchmark.py report --config scripts/stage_benchmark.example.json

Grading the Reviewers: `report` writes `<out>/reviews.md` (every report, by
target) and `<out>/review-grades.csv`. Fill in `found`, `false` and `missed`
per run; the next `report` turns them into precision and recall.

Standard library only.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import threading
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PHASES = ("author", "score", "grade", "review", "judge")
# The phases that boot no AEM, so their runs may go side by side.
NO_AEM = ("score", "judge")
REFERENCE_TABLES = ("references_", "reference_pdfs", "reference_files")
SESSION_LINE = re.compile(r"^Session:\s*([0-9a-f-]{36})", re.M)
ISSUE_LINE = re.compile(r"^\s*(\d+)[.)]\s+\S", re.M)


# ── Configuration ────────────────────────────────────────────────────────────


def load_config(path: Path) -> dict:
    config = json.loads(path.read_text())
    for key in ("form", "candidates", "baseline"):
        if key not in config:
            sys.exit(f"{path}: '{key}' is missing")
    config["form"] = [str(resolve(p)) for p in config["form"]]
    config["out"] = str(resolve(config.get("out", "benchmark")))
    config["blueprint"] = str(resolve(config.get("blueprint", "target/release/blueprint")))
    config.setdefault("base_url", "https://openrouter.ai/api/v1")
    config.setdefault("api_key_env", "OPENROUTER_API_KEY")
    config.setdefault("repeats", 2)
    config.setdefault("targets", "baseline")
    config.setdefault("extra_args", [])
    # Each run's own limits (`convert --max-spend/--max-tokens`, 0 = none).
    # Named on every run so the desktop app's defaults (USD 100, 20 M tokens)
    # do not decide where a candidate is cut off.
    config.setdefault("max_spend_per_run", 0)
    config.setdefault("max_tokens_per_run", 0)
    for role in ("author", "reviewer", "judge"):
        if role not in config["baseline"]:
            sys.exit(f"{path}: baseline.{role} is missing")
        config["candidates"].setdefault(role, [config["baseline"][role]])
    for pdf in config["form"]:
        if not Path(pdf).is_file():
            sys.exit(f"Source not found: {pdf}")
    return config


def resolve(p: str) -> Path:
    """A config path: absolute, or relative to the repository."""
    path = Path(os.path.expanduser(p))
    return path if path.is_absolute() else REPO / path


def slug(model: str) -> str:
    return re.sub(r"[^A-Za-z0-9.-]+", "_", model).strip("_")


def form_code(config: dict) -> str:
    return Path(config["form"][0]).name.split("_")[0]


# ── The reference library ────────────────────────────────────────────────────


def default_db() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "blueprint" / "history.db"
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "blueprint" / "history.db"


def document_hash(paths: list[str]) -> str:
    """The id a reference built from these PDFs has: `references_mcp::document_hash`."""
    digests = sorted(hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in paths)
    return hashlib.sha256("".join(digests).encode()).hexdigest()


def matching_references(conn: sqlite3.Connection, config: dict) -> list[tuple[str, str, str]]:
    """References that are this form: by content hash, or by its code in the label."""
    if not conn.execute("SELECT 1 FROM sqlite_master WHERE name = 'references_'").fetchone():
        return []
    rows = conn.execute(
        "SELECT ref_id, profile, label FROM references_ WHERE ref_id = ? OR label LIKE ?",
        (document_hash(config["form"]), f"%{form_code(config)}%"),
    ).fetchall()
    return [tuple(r) for r in rows]


def mentioning_docs(conn: sqlite3.Connection, config: dict) -> list[tuple[str, str]]:
    if not conn.execute("SELECT 1 FROM sqlite_master WHERE name = 'reference_docs'").fetchone():
        return []
    return [tuple(r) for r in conn.execute(
        "SELECT doc_id, label FROM reference_docs WHERE content LIKE ?", (f"%{form_code(config)}%",)
    )]


def refs_command(args: argparse.Namespace) -> None:
    config = load_config(args.config)
    db = Path(args.db) if args.db else default_db()
    if not db.is_file():
        sys.exit(f"No history.db at {db} (pass --db)")
    out = Path(config["out"])
    out.mkdir(parents=True, exist_ok=True)
    saved = out / f"removed-references-{form_code(config)}.sqlite"
    conn = sqlite3.connect(db, timeout=10)
    try:
        if args.action == "list":
            refs = matching_references(conn, config)
            for ref_id, profile, label in refs:
                print(f"reference  {label}  ({profile}, {ref_id[:12]}…)")
            for doc_id, label in mentioning_docs(conn, config):
                print(f"doc mentions {form_code(config)}: {label} ({doc_id[:12]}…) — not removed by 'refs remove'")
            if not refs:
                print(f"No reference matches {form_code(config)}.")
        elif args.action == "remove":
            remove_references(conn, db, config, saved)
        else:
            restore_references(conn, saved)
    finally:
        conn.close()


def remove_references(conn: sqlite3.Connection, db: Path, config: dict, saved: Path) -> None:
    refs = matching_references(conn, config)
    if not refs:
        print(f"No reference matches {form_code(config)}: nothing to remove.")
        return
    backup = db.with_name(f"history.db.bak-{dt.datetime.now():%Y%m%d-%H%M%S}")
    with sqlite3.connect(backup) as copy:
        conn.backup(copy)
    print(f"Backup of the whole database: {backup}")
    if saved.exists():
        sys.exit(f"{saved} already holds removed references; restore or move it first.")
    ids = [r[0] for r in refs]
    marks = ",".join("?" * len(ids))
    conn.execute("ATTACH DATABASE ? AS saved", (str(saved),))
    with conn:
        for table in REFERENCE_TABLES:
            if not conn.execute("SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?", (table,)).fetchone():
                continue
            # Same columns in the same order, which is all a restore needs.
            conn.execute(f"CREATE TABLE saved.{table} AS SELECT * FROM main.{table} WHERE ref_id IN ({marks})", ids)
            conn.execute(f"DELETE FROM main.{table} WHERE ref_id IN ({marks})", ids)
    conn.execute("DETACH DATABASE saved")
    for ref_id, profile, label in refs:
        print(f"Removed reference {label} ({profile}, {ref_id[:12]}…)")
    print(f"Kept for 'refs restore' in {saved}")


def restore_references(conn: sqlite3.Connection, saved: Path) -> None:
    if not saved.is_file():
        sys.exit(f"Nothing to restore: {saved} does not exist.")
    conn.execute("ATTACH DATABASE ? AS saved", (str(saved),))
    with conn:
        for table in REFERENCE_TABLES:
            if conn.execute("SELECT 1 FROM saved.sqlite_master WHERE name=?", (table,)).fetchone():
                conn.execute(f"INSERT OR IGNORE INTO main.{table} SELECT * FROM saved.{table}")
    labels = [r[0] for r in conn.execute("SELECT label FROM saved.references_")]
    conn.execute("DETACH DATABASE saved")
    saved.rename(saved.with_suffix(".restored.sqlite"))
    print(f"Restored: {', '.join(labels) or '(none)'}")


# ── The runs ─────────────────────────────────────────────────────────────────


class Manifest:
    """`runs.jsonl`: one line per attempt; the last line for a key wins."""

    def __init__(self, out: Path):
        self.path = out / "runs.jsonl"
        self.lock = threading.Lock()
        self.runs: dict[str, dict] = {}
        if self.path.exists():
            for line in self.path.read_text().splitlines():
                if line.strip():
                    run = json.loads(line)
                    self.runs[run["key"]] = run

    def record(self, run: dict) -> None:
        with self.lock:
            self.runs[run["key"]] = run
            with self.path.open("a") as f:
                f.write(json.dumps(run) + "\n")

    def done(self, key: str) -> dict | None:
        run = self.runs.get(key)
        return run if run and run.get("status") == "ok" else None

    def of(self, phase: str) -> list[dict]:
        return [r for r in self.runs.values() if r["phase"] == phase and r.get("status") == "ok"]

    def spent(self) -> float:
        return sum(r.get("cost_usd") or 0 for r in self.runs.values())


def plan(config: dict, manifest: Manifest, phases: list[str], only: str | None) -> list[dict]:
    """The runs still to do in `phases`, in order. Targets of later phases come
    from the Author runs already recorded."""
    base = config["baseline"]
    runs: list[dict] = []

    def add(phase: str, model: str, rep: int, target: str | None = None, **extra) -> None:
        if only and only not in model:
            return
        key = "/".join(x for x in (phase, slug(model), target[:8] if target else None, f"r{rep}") if x)
        runs.append({"key": key, "phase": phase, "model": model, "rep": rep, "target": target, **extra})

    if "author" in phases:
        for model in config["candidates"]["author"]:
            for rep in range(1, config["repeats"] + 1):
                add("author", model, rep)
    authors = manifest.of("author")
    targets = assessment_targets(config, authors)
    if "score" in phases:
        # A target the baseline judges judge in the judge phase anyway is
        # scored by that run instead.
        judged = set(targets) if base["judge"] in config["candidates"]["judge"] else set()
        for a in authors:
            if a["session"] not in judged:
                add("score", base["judge"], 1, a["session"], author_model=a["model"])
    if "grade" in phases:
        # A target the baseline Reviewer reviews in the review phase anyway is
        # graded by that review instead.
        reviewed = set(targets) if base["reviewer"] in config["candidates"]["reviewer"] else set()
        for a in authors:
            if a["session"] not in reviewed:
                add("grade", base["reviewer"], 1, a["session"], author_model=a["model"])
    if ("review" in phases or "judge" in phases) and not targets:
        print("No Author result to assess yet: run the author phase first (or set 'targets').", file=sys.stderr)
    for target in targets:
        if "review" in phases:
            for model in config["candidates"]["reviewer"]:
                for rep in range(1, config["repeats"] + 1):
                    add("review", model, rep, target)
        if "judge" in phases:
            for model in config["candidates"]["judge"]:
                for rep in range(1, config["repeats"] + 1):
                    add("judge", model, rep, target)
    return runs


def assessment_targets(config: dict, authors: list[dict]) -> list[str]:
    """Which Author results the Reviewers and judges assess: 'baseline' (the
    first baseline Author run), 'all', or a list of session ids."""
    targets = config["targets"]
    if isinstance(targets, list):
        return targets
    ordered = sorted(authors, key=lambda a: a["key"])
    if targets == "all":
        return [a["session"] for a in ordered]
    baseline = [a for a in ordered if a["model"] == config["baseline"]["author"]]
    return [baseline[0]["session"]] if baseline else []


def command(config: dict, run: dict, run_dir: Path) -> list[str]:
    base = config["baseline"]
    cmd = [config["blueprint"], "convert", *config["form"], "--out", str(run_dir / "artefacts"),
           "--base-url", config["base_url"], "--analysis-dir", str(run_dir / "analysis")]
    # Every role is named on every run: a role left out would take the model
    # the desktop app has saved for it, which is not part of the benchmark.
    phase, model = run["phase"], run["model"]
    if phase == "author":
        cmd += ["--model", model, "--reviewer-model", base["reviewer"], "--judge-model", base["judge"],
                "--max-review-rounds", "0", "--no-judges"]
    elif phase in ("grade", "review"):
        cmd += ["--session", run["target"], "--assess", "--max-review-rounds", "1",
                "--model", model, "--reviewer-model", model, "--judge-model", base["judge"], "--no-judges"]
    else:  # score, judge
        cmd += ["--session", run["target"], "--assess", "--max-review-rounds", "0",
                "--model", base["reviewer"], "--reviewer-model", base["reviewer"], "--judge-model", model]
    cmd += ["--max-spend", str(config["max_spend_per_run"]), "--max-tokens", str(config["max_tokens_per_run"])]
    return cmd + list(config["extra_args"])


def execute(config: dict, manifest: Manifest, run: dict, dry_run: bool) -> None:
    run_dir = Path(config["out"]) / "runs" / run["key"].replace("/", "__")
    cmd = command(config, run, run_dir)
    if dry_run:
        print(" ".join(shlex_quote(c) for c in cmd))
        return
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    env = dict(os.environ)
    env["OPENAI_API_KEY"] = api_key(config)
    started = dt.datetime.now()
    print(f"[{started:%H:%M}] {run['key']} …", flush=True)
    with (run_dir / "console.log").open("w") as log:
        log.write(" ".join(cmd) + "\n\n")
        log.flush()
        proc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=REPO, env=env)
    summary_path = find_summary(run_dir / "analysis")
    summary = json.loads(summary_path.read_text()) if summary_path else {}
    console = (run_dir / "console.log").read_text(errors="replace")
    session = summary.get("run", {}).get("session_id") or next(iter(SESSION_LINE.findall(console)), None)
    produced = bool(summary.get("finished"))
    record = {
        **run,
        "status": "ok" if proc.returncode == 0 and produced else "failed",
        "exit_code": proc.returncode,
        "session": session,
        "run_dir": str(run_dir),
        "summary": str(summary_path) if summary_path else None,
        "cost_usd": (summary.get("spend") or {}).get("cost_usd"),
        "started": started.isoformat(timespec="seconds"),
        "minutes": round((dt.datetime.now() - started).total_seconds() / 60, 1),
    }
    manifest.record(record)
    cost = f"USD {record['cost_usd']:.2f}" if record["cost_usd"] is not None else "cost unknown"
    print(f"        {record['status']} · {record['minutes']} min · {cost}"
          + ("" if record["status"] == "ok" else f" · see {run_dir / 'console.log'}"), flush=True)


def shlex_quote(s: str) -> str:
    return s if re.fullmatch(r"[\w./:=@,+-]+", s) else "'" + s.replace("'", "'\\''") + "'"


def find_summary(analysis: Path) -> Path | None:
    found = sorted(analysis.glob("**/summary.json"))
    return found[-1] if found else None


def run_command(args: argparse.Namespace) -> None:
    config = load_config(args.config)
    out = Path(config["out"])
    out.mkdir(parents=True, exist_ok=True)
    manifest = Manifest(out)
    if not args.dry_run:
        preflight(config, args)
    phases = list(PHASES) if args.phase == "all" else [args.phase]
    for phase in phases:
        todo = [r for r in plan(config, manifest, [phase], args.only) if not manifest.done(r["key"])]
        if not todo:
            continue
        print(f"── {phase}: {len(todo)} run(s) ──")
        jobs = args.jobs if phase in NO_AEM else 1

        quick_failures = [0]

        def one(run: dict) -> None:
            if args.max_cost is not None and manifest.spent() >= args.max_cost:
                print(f"Budget of USD {args.max_cost:.2f} reached: {run['key']} not started.")
                return
            if quick_failures[0] >= 2:
                print(f"{run['key']} not started: the last runs failed at once.")
                return
            execute(config, manifest, run, args.dry_run)
            done = manifest.runs.get(run["key"], {})
            # Two runs in a row that fail within a minute share a cause (the
            # key, the binary, Docker): starting the rest would only repeat it.
            if not args.dry_run and done.get("status") != "ok" and (done.get("minutes") or 0) < 1:
                quick_failures[0] += 1
            elif not args.dry_run:
                quick_failures[0] = 0

        if jobs > 1 and not args.dry_run:
            with ThreadPoolExecutor(jobs) as pool:
                list(pool.map(one, todo))
        else:
            for run in todo:
                one(run)
        if quick_failures[0] >= 2:
            sys.exit(f"Stopped: two {phase} runs in a row failed within a minute. The cause is in their "
                     "console.log (the last lines); fix it and run the same command again.")
        if args.dry_run and phase == "author":
            print("(the later phases are planned from the Author runs once they exist)")
    if not args.dry_run:
        write_report(config, manifest)


def api_key(config: dict) -> str:
    """The key the runs use: `api_key_env`, else $OPENAI_API_KEY, trimmed (a
    pasted key often carries a newline, which no header can hold)."""
    return (os.environ.get(config["api_key_env"]) or os.environ.get("OPENAI_API_KEY") or "").strip()


def check_key(config: dict) -> None:
    """Asks OpenRouter whether it knows the key before anything runs: a run
    with a bad key fails on its first turn, and so does every one after it."""
    if "openrouter.ai" not in config["base_url"]:
        return
    request = urllib.request.Request(config["base_url"].rstrip("/") + "/key",
                                     headers={"Authorization": f"Bearer {api_key(config)}"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            data = json.load(response).get("data", {})
    except urllib.error.HTTPError as e:
        sys.exit(f"OpenRouter refused the key in ${config['api_key_env']} (HTTP {e.code}: "
                 f"{e.read().decode(errors='replace')[:200]}). Export the real key and try again.")
    except (urllib.error.URLError, TimeoutError, ValueError) as e:
        print(f"Could not check the key with OpenRouter ({e}); going on.", file=sys.stderr)
        return
    left = data.get("limit_remaining")
    print(f"OpenRouter key: {data.get('label') or 'ok'}"
          + (f", USD {left:.2f} left on its limit" if isinstance(left, (int, float)) else ""))


def preflight(config: dict, args: argparse.Namespace) -> None:
    if not Path(config["blueprint"]).is_file():
        sys.exit(f"{config['blueprint']} not found: cargo build --release -p blueprint-cli")
    if not api_key(config):
        sys.exit(f"Set ${config['api_key_env']} to the OpenRouter key.")
    check_key(config)
    db = Path(args.db) if args.db else default_db()
    if db.is_file() and not args.allow_reference:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            refs = matching_references(conn, config)
        finally:
            conn.close()
        if refs:
            sys.exit(f"{form_code(config)} is still a reference form ({', '.join(r[2] for r in refs)}): "
                     "run 'refs remove' first, or pass --allow-reference.")


# ── What the runs show ───────────────────────────────────────────────────────


def stage(summary: dict, name: str) -> dict:
    return next((s for s in summary.get("stages", []) if s.get("name") == name), {})


def cost(spend: dict | None) -> float | None:
    return (spend or {}).get("cost_usd")


def final_check(summary: dict) -> dict:
    """Where the rules stand at the end of the run, after its final rule check.

    Read from the final rule board (`outcome.rules`), which holds the verdicts
    the final check reused from earlier checks of the run as well as the ones
    its judges gave. `verdicts` maps each judged rule to positive, negative or
    unchecked; the counts cover every rule, scripted ones included.
    """
    board = (summary.get("outcome") or {}).get("rules")
    if board:
        state = {"pass": "positive", "fail": "negative"}
        judged = {f"{r['title']} ({r['rule_id'][:8]})": state.get(r["state"], "unchecked")
                  for r in board if r.get("kind") == "judge"}
        return {
            "positive": sum(r["state"] == "pass" for r in board),
            "negative": sum(r["state"] == "fail" for r in board),
            "unchecked": sum(r["state"] not in ("pass", "fail") for r in board),
            "judged_negative": sum(v == "negative" for v in judged.values()),
            "scripted_negative": sum(r["state"] == "fail" for r in board if r.get("kind") == "script"),
            "judged": len(judged),
            "verdicts": judged,
        }
    # An engine without the board in its summary: the last check's own judges.
    checks = summary.get("judges", {}).get("rule_checks") or []
    if not checks:
        return {}
    last = checks[-1]
    verdicts = {j["rule_name"]: j["verdict"] for j in summary["judges"].get("runs", [])
                if j.get("check") == last.get("check")}
    return {"positive": last.get("positive", 0), "negative": last.get("negative", 0),
            "unchecked": last.get("unchecked", 0), "judged": last.get("judged", 0),
            "judged_negative": last.get("negative", 0), "scripted_negative": None, "verdicts": verdicts}


def review_of(summary: dict) -> dict:
    verdicts = summary.get("review_verdicts") or []
    if not verdicts:
        return {"approved": None, "issues": None, "report": ""}
    v = verdicts[-1]
    report = v.get("report", "")
    return {"approved": v.get("approved"), "issues": len(ISSUE_LINE.findall(report)), "report": report}


def read(run: dict) -> dict:
    return json.loads(Path(run["summary"]).read_text()) if run.get("summary") else {}


def mean(values: list) -> float | None:
    values = [v for v in values if v is not None]
    return statistics.mean(values) if values else None


def fmt(v, digits: int = 2) -> str:
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.{digits}f}"
    return str(v)


def first_by_target(runs: list[dict]) -> dict[str, dict]:
    """The lowest-repeat run per target."""
    chosen: dict[str, dict] = {}
    for r in sorted(runs, key=lambda r: -r["rep"]):
        chosen[r["target"]] = r
    return chosen


def author_rows(manifest: Manifest, baseline: dict) -> list[dict]:
    """One row per Author run. Its scripted rules are its own run's; its judged
    rules are the baseline judges' verdicts on it (a score run, or the first
    baseline judge run of the target); its grade is the baseline Reviewer's
    (a grade run, or the first baseline review of the target)."""
    gradings = first_by_target([r for r in manifest.of("review") if r["model"] == baseline["reviewer"]])
    gradings.update({g["target"]: g for g in manifest.of("grade")})
    grades = {t: review_of(read(g)) for t, g in gradings.items()}
    scorings = first_by_target([r for r in manifest.of("judge") if r["model"] == baseline["judge"]])
    scorings.update({g["target"]: g for g in manifest.of("score")})
    scores = {t: (final_check(read(g)), cost(read(g).get("spend"))) for t, g in scorings.items()}
    rows = []
    for run in sorted(manifest.of("author"), key=lambda r: r["key"]):
        s = read(run)
        author, check = stage(s, "Author"), final_check(s)
        grade = grades.get(run["session"], {})
        score, score_usd = scores.get(run["session"], ({}, None))
        rows.append({
            "model": run["model"], "rep": run["rep"], "session": run["session"],
            "finished_authoring": author.get("ended_by") == "finish_authoring",
            "author_usd": cost(author.get("agent_spend")),
            "author_judges_usd": cost(author.get("judges_spend")),
            "total_usd": cost(s.get("spend")), "score_usd": score_usd,
            "minutes": round(s.get("time", {}).get("wall_ms", 0) / 60000, 1),
            "turns": author.get("turns"), "tool_failures": author.get("tool_failures"),
            "failed_requests": author.get("failed_requests"),
            "scripted_failing": check.get("scripted_negative"),
            "judged_failing": score.get("judged_negative"),
            "judged_unchecked": sum(v == "unchecked" for v in score.get("verdicts", {}).values()) if score else None,
            "grade_approved": grade.get("approved"), "grade_issues": grade.get("issues"),
        })
    return rows


def review_rows(manifest: Manifest, grades: dict) -> list[dict]:
    rows = []
    for run in sorted(manifest.of("review"), key=lambda r: r["key"]):
        s = read(run)
        reviewer, review = stage(s, "Reviewer"), review_of(s)
        g = grades.get(run["key"], {})
        found, false, missed = (to_int(g.get(k)) for k in ("found", "false", "missed"))
        rows.append({
            "key": run["key"], "model": run["model"], "rep": run["rep"], "target": run["target"],
            "approved": review["approved"], "issues": review["issues"],
            "reviewer_usd": cost(reviewer.get("agent_spend")),
            "judges_usd": cost(reviewer.get("judges_spend")), "total_usd": cost(s.get("spend")),
            "minutes": round(s.get("time", {}).get("wall_ms", 0) / 60000, 1),
            "turns": reviewer.get("turns"),
            "precision": found / (found + false) if found is not None and false is not None and found + false else None,
            "recall": found / (found + missed) if found is not None and missed is not None and found + missed else None,
        })
    return rows


def to_int(v) -> int | None:
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def judge_rows(manifest: Manifest, baseline_model: str) -> tuple[list[dict], dict]:
    runs = sorted(manifest.of("judge"), key=lambda r: r["key"])
    checks = {r["key"]: final_check(read(r)) for r in runs}
    # The baseline's verdict per (target, rule): the most common across its
    # repeats; an unchecked rule says nothing to agree with.
    baseline: dict[tuple[str, str], list[str]] = {}
    for r in runs:
        if r["model"] == baseline_model:
            for rule, verdict in checks[r["key"]].get("verdicts", {}).items():
                if verdict != "unchecked":
                    baseline.setdefault((r["target"], rule), []).append(verdict)
    reference = {k: max(sorted(set(v)), key=v.count) for k, v in baseline.items()}
    rows, by_rule = [], {}
    for r in runs:
        s, check = read(r), checks[r["key"]]
        verdicts = check.get("verdicts", {})
        compared = [(rule, v) for rule, v in verdicts.items() if (r["target"], rule) in reference]
        agree = sum(1 for rule, v in compared if reference[(r["target"], rule)] == v)
        for rule, v in verdicts.items():
            by_rule.setdefault(rule, {}).setdefault(r["model"], []).append(v)
        rows.append({
            "key": r["key"], "model": r["model"], "rep": r["rep"], "target": r["target"],
            "judged": check.get("judged"),
            "positive": sum(v == "positive" for v in check.get("verdicts", {}).values()),
            "negative": sum(v == "negative" for v in check.get("verdicts", {}).values()),
            "unchecked": sum(v == "unchecked" for v in check.get("verdicts", {}).values()),
            "agreement": agree / len(compared) if compared else None,
            "judges_usd": cost(s.get("judges", {}).get("spend")) or s.get("judge_cost_usd"),
            "minutes": round(s.get("time", {}).get("wall_ms", 0) / 60000, 1),
        })
    return rows, by_rule


def consistency(by_rule: dict, model: str) -> float | None:
    """Share of rules a model judged the same in every repeat."""
    seen = [v for rules in by_rule.values() for m, v in rules.items() if m == model and len(v) > 1]
    return sum(1 for v in seen if len(set(v)) == 1) / len(seen) if seen else None


def write_csv(path: Path, rows: list[dict]) -> None:
    if not rows:
        return
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        w.writeheader()
        w.writerows(rows)


def table(headers: list[str], rows: list[list]) -> str:
    lines = ["| " + " | ".join(headers) + " |", "|" + "---|" * len(headers)]
    lines += ["| " + " | ".join(fmt(c) for c in row) + " |" for row in rows]
    return "\n".join(lines)


def by_model(rows: list[dict]) -> dict[str, list[dict]]:
    grouped: dict[str, list[dict]] = {}
    for r in rows:
        grouped.setdefault(r["model"], []).append(r)
    return grouped


def read_grades(path: Path) -> dict:
    if not path.exists():
        return {}
    with path.open() as f:
        return {row["key"]: row for row in csv.DictReader(f)}


def write_report(config: dict, manifest: Manifest) -> None:
    out = Path(config["out"])
    grades_path = out / "review-grades.csv"
    grades = read_grades(grades_path)
    authors = author_rows(manifest, config["baseline"])
    reviews = review_rows(manifest, grades)
    judges, by_rule = judge_rows(manifest, config["baseline"]["judge"])
    write_csv(out / "authors.csv", authors)
    write_csv(out / "reviewers.csv", reviews)
    write_csv(out / "judges.csv", judges)
    write_grade_template(grades_path, manifest, grades)
    write_reviews(out / "reviews.md", manifest)

    failed = [r for r in manifest.runs.values() if r.get("status") != "ok"]
    md = [f"# Stage benchmark — {form_code(config)}", "",
          f"Generated {dt.datetime.now():%Y-%m-%d %H:%M}. Baseline: Author `{config['baseline']['author']}`, "
          f"Reviewer `{config['baseline']['reviewer']}`, judges `{config['baseline']['judge']}`. "
          f"Spent so far: USD {manifest.spent():.2f}. Costs are USD; means over the repeats.", ""]

    md += ["## Author", "",
           "The Author runs without judges. Scripted rules as its own run left them, judged rules as the "
           "baseline judges found them afterwards, and the issues one baseline Reviewer round found. Lower is "
           "better in all three. *Scoring USD* is what judging the result cost, not part of the Author's cost.", ""]
    md.append(table(
        ["Model", "Runs", "Finished authoring", "Author USD", "Minutes", "Turns", "Scripted rules failing",
         "Judged rules failing", "Judged unchecked", "Reviewer issues", "Approved", "Scoring USD"],
        [[m, len(rs), f"{sum(r['finished_authoring'] for r in rs)}/{len(rs)}",
          mean([r["author_usd"] for r in rs]), mean([r["minutes"] for r in rs]), mean([r["turns"] for r in rs]),
          mean([r["scripted_failing"] for r in rs]), mean([r["judged_failing"] for r in rs]),
          mean([r["judged_unchecked"] for r in rs]), mean([r["grade_issues"] for r in rs]),
          f"{sum(1 for r in rs if r['grade_approved'])}/{sum(1 for r in rs if r['grade_approved'] is not None)}",
          mean([r["score_usd"] for r in rs])]
         for m, rs in by_model(authors).items()]) if authors else "_No Author run yet._")

    md += ["", "## Reviewer", "",
           "All on the same Author result(s). *Issues* counts the numbered points of the report; "
           f"precision and recall need `{grades_path.name}` filled in (reports in `reviews.md`).", ""]
    md.append(table(
        ["Model", "Runs", "Approved", "Issues", "Reviewer USD", "Its judges USD", "Minutes", "Precision", "Recall"],
        [[m, len(rs), f"{sum(1 for r in rs if r['approved'])}/{len(rs)}", mean([r["issues"] for r in rs]),
          mean([r["reviewer_usd"] for r in rs]), mean([r["judges_usd"] for r in rs]),
          mean([r["minutes"] for r in rs]), mean([r["precision"] for r in rs]), mean([r["recall"] for r in rs])]
         for m, rs in by_model(reviews).items()]) if reviews else "_No Reviewer run yet._")

    md += ["", "## Judges", "",
           "Every judged rule on the same Author result(s). *Agreement* is with the baseline judges' most "
           "common verdict; *consistency* is the share of rules judged the same in every repeat.", ""]
    md.append(table(
        ["Model", "Runs", "Judged", "Negative", "Unchecked", "Agreement", "Consistency", "Judges USD", "Minutes"],
        [[m, len(rs), mean([r["judged"] for r in rs]), mean([r["negative"] for r in rs]),
          mean([r["unchecked"] for r in rs]), mean([r["agreement"] for r in rs]), consistency(by_rule, m),
          mean([r["judges_usd"] for r in rs]), mean([r["minutes"] for r in rs])]
         for m, rs in by_model(judges).items()]) if judges else "_No judge run yet._")
    disputed = {rule: models for rule, models in by_rule.items()
                if len({v for vs in models.values() for v in vs}) > 1}
    if disputed:
        models = sorted({m for ms in by_rule.values() for m in ms})
        md += ["", "### Rules the judges disagree on", "",
               table(["Rule", *models], [[rule, *["/".join(disputed[rule].get(m, [])) or "-" for m in models]]
                                         for rule in sorted(disputed)])]
    if failed:
        md += ["", "## Failed runs", "",
               table(["Run", "Exit code", "Log"], [[r["key"], r.get("exit_code"), f"{r.get('run_dir')}/console.log"]
                                                  for r in failed])]
    (out / "report.md").write_text("\n".join(md) + "\n")
    print(f"Report: {out / 'report.md'}")


def write_grade_template(path: Path, manifest: Manifest, grades: dict) -> None:
    """The grading sheet, keeping what was already filled in."""
    rows = [{"key": r["key"], "model": r["model"], "target": r["target"], "rep": r["rep"],
             **{k: grades.get(r["key"], {}).get(k, "") for k in ("found", "false", "missed", "notes")}}
            for r in sorted(manifest.of("review"), key=lambda r: r["key"])]
    write_csv(path, rows)


def write_reviews(path: Path, manifest: Manifest) -> None:
    runs = sorted(manifest.of("review") + manifest.of("grade"), key=lambda r: (r["target"], r["phase"], r["key"]))
    if not runs:
        return
    md = ["# Reviewer reports", "", "Grade each in `review-grades.csv`: real issues found, false ones, real ones missed.", ""]
    target = None
    for r in runs:
        if r["target"] != target:
            target = r["target"]
            md += [f"## Author result {target}", ""]
        review = review_of(read(r))
        verdict = {True: "approved", False: "changes requested", None: "no verdict"}[review["approved"]]
        md += [f"### {r['key']} — {verdict}", "", review["report"].strip() or "_(no report)_", ""]
    path.write_text("\n".join(md) + "\n")


def report_command(args: argparse.Namespace) -> None:
    config = load_config(args.config)
    write_report(config, Manifest(Path(config["out"])))


# ── Entry point ──────────────────────────────────────────────────────────────


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    refs = sub.add_parser("refs", help="find, remove or restore the form among the reference forms")
    refs.add_argument("action", choices=("list", "remove", "restore"))
    refs.add_argument("--config", type=Path, required=True)
    refs.add_argument("--db", help="history.db (default: the app's)")
    refs.set_defaults(func=refs_command)

    run = sub.add_parser("run", help="run the phases still to do")
    run.add_argument("--config", type=Path, required=True)
    run.add_argument("--phase", choices=("all", *PHASES), default="all")
    run.add_argument("--only", help="only models whose id contains this text")
    run.add_argument("--jobs", type=int, default=1, help="parallel runs in the score and judge phases (no AEM)")
    run.add_argument("--max-cost", type=float, help="start no run once this many USD are spent")
    run.add_argument("--dry-run", action="store_true", help="print the commands only")
    run.add_argument("--allow-reference", action="store_true", help="run even if the form is a reference")
    run.add_argument("--db", help="history.db (default: the app's)")
    run.set_defaults(func=run_command)

    report = sub.add_parser("report", help="write the CSVs and report.md from the runs so far")
    report.add_argument("--config", type=Path, required=True)
    report.set_defaults(func=report_command)

    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
