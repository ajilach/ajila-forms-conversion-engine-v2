#!/usr/bin/env python3
"""Tests for stage_benchmark.py: `python3 scripts/test_stage_benchmark.py`.

The runs go through a stand-in `blueprint` that writes a summary.json in the
layout `runner::analysis` writes, so the whole run → manifest → report path is
exercised without a model or AEM.
"""

import csv
import io
import json
import os
import sqlite3
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import stage_benchmark as sb  # noqa: E402

FAKE_BLUEPRINT = r'''#!/usr/bin/env python3
import json, sys, uuid
from pathlib import Path
args = sys.argv[1:]
if "x/broken" in args:
    print("error: the Author turn failed: 401 Unauthorized")
    sys.exit(1)
def opt(name, default=None):
    return args[args.index(name) + 1] if name in args else default
analysis = Path(opt("--analysis-dir")) / "2026-10-15_run"
analysis.mkdir(parents=True)
assess = "--assess" in args
rounds = opt("--max-review-rounds")
session = opt("--session") or str(uuid.uuid4())
model, judge = opt("--model"), opt("--judge-model")
cheap = "flash" in judge
verdicts = {"rule-a": "positive", "rule-b": "negative" if not cheap else "positive", "rule-c": "positive"}
runs = [{"check": 1, "rule_name": r, "verdict": v} for r, v in verdicts.items()]
neg = sum(v == "negative" for v in verdicts.values())
check = {"check": 1, "positive": 3 - neg, "negative": neg, "unchecked": 0, "judged": 3, "scripted": 10}
stages = []
review = []
if not assess:
    stages.append({"name": "Author", "ended_by": "finish_authoring", "turns": 40, "tool_failures": 1,
                   "failed_requests": 0, "agent_spend": {"cost_usd": 2.0}, "judges_spend": {"cost_usd": 0.5}})
elif rounds == "1":
    stages.append({"name": "Reviewer", "turns": 20, "agent_spend": {"cost_usd": 1.0},
                   "judges_spend": {"cost_usd": 0.2}})
    review = [{"round": 1, "approved": False, "report": "ISSUES\n1. Title wrong.\n2. Footer missing.\n"}]
no_judges = "--no-judges" in args
board = [{"rule_id": "id-" + r, "title": r, "kind": "judge",
          "state": "not_checked" if no_judges else ("pass" if v == "positive" else "fail"),
          "outdated": False} for r, v in verdicts.items()]
board.append({"rule_id": "id-script", "title": "scripted", "kind": "script", "state": "fail", "outdated": False})
summary = {"schema_version": 3, "run": {"session_id": session}, "finished": True,
           "outcome": {"produced": True, "rules": board},
           "spend": {"cost_usd": 3.0}, "time": {"wall_ms": 120000}, "stages": stages,
           "judges": {"rule_checks": [check] if rounds == "0" or not assess else [], "runs": runs,
                      "spend": {"cost_usd": 0.4}},
           "review_verdicts": review}
(analysis / "summary.json").write_text(json.dumps(summary))
print(f"Session: {session}")
'''


class Bench(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.pdf = root / "AAOV_033_IT.pdf"
        self.pdf.write_bytes(b"%PDF-1.7 test")
        self.fake = root / "blueprint"
        self.fake.write_text(FAKE_BLUEPRINT)
        self.fake.chmod(self.fake.stat().st_mode | stat.S_IEXEC)
        self.config_path = root / "bench.json"
        self.config_path.write_text(json.dumps({
            "form": [str(self.pdf)], "out": str(root / "out"), "blueprint": str(self.fake), "repeats": 2,
            # Not OpenRouter, so no key check goes over the network.
            "base_url": "http://gateway.invalid/v1",
            "baseline": {"author": "a/opus", "reviewer": "a/opus", "judge": "a/opus"},
            "candidates": {"author": ["a/opus", "g/flash"], "reviewer": ["a/opus", "g/flash"],
                           "judge": ["a/opus", "g/flash"]},
        }))
        self.db = root / "history.db"
        os.environ["OPENROUTER_API_KEY"] = "test"

    def tearDown(self):
        self.tmp.cleanup()

    def bench(self, *argv):
        out = io.StringIO()
        with redirect_stdout(out):
            sb.main(list(argv))
        return out.getvalue()

    def make_db(self, label="AAOV_033_IT"):
        conn = sqlite3.connect(self.db)
        conn.executescript(
            "CREATE TABLE references_ (ref_id TEXT PRIMARY KEY, profile TEXT, label TEXT, description TEXT,"
            " description_embedding BLOB, model_version TEXT, created_at TEXT);"
            "CREATE TABLE reference_pdfs (ref_id TEXT, pdf_index INTEGER, page_count INTEGER, pdf_bytes BLOB,"
            " PRIMARY KEY (ref_id, pdf_index));"
            "CREATE TABLE reference_files (ref_id TEXT, path TEXT, content TEXT, PRIMARY KEY (ref_id, path));"
            "CREATE TABLE reference_docs (doc_id TEXT PRIMARY KEY, profile TEXT, label TEXT, content TEXT,"
            " created_at TEXT);")
        ref = sb.document_hash([str(self.pdf)])
        conn.execute("INSERT INTO references_ VALUES (?, 'ubs', ?, 'd', x'00', 'm', 't')", (ref, label))
        conn.execute("INSERT INTO references_ VALUES ('other', 'ubs', 'AAGS_019_DE', 'd', x'00', 'm', 't')")
        conn.execute("INSERT INTO reference_pdfs VALUES (?, 0, 2, x'01')", (ref,))
        conn.execute("INSERT INTO reference_files VALUES (?, 'jcr_root/.content.xml', '<x/>')", (ref,))
        conn.commit()
        conn.close()
        return ref

    def count(self, table, ref):
        conn = sqlite3.connect(self.db)
        try:
            return conn.execute(f"SELECT COUNT(*) FROM {table} WHERE ref_id = ?", (ref,)).fetchone()[0]
        finally:
            conn.close()

    def test_the_reference_id_is_the_engines_document_hash(self):
        # sha256 of the sorted per-file sha256 hex digests, joined.
        import hashlib
        inner = hashlib.sha256(b"%PDF-1.7 test").hexdigest()
        self.assertEqual(sb.document_hash([str(self.pdf)]), hashlib.sha256(inner.encode()).hexdigest())

    def test_a_reference_is_removed_with_a_copy_and_restored(self):
        ref = self.make_db(label="renamed by hand")  # found by its content hash
        self.bench("refs", "remove", "--config", str(self.config_path), "--db", str(self.db))
        for table in sb.REFERENCE_TABLES:
            self.assertEqual(self.count(table, ref), 0, table)
        self.assertEqual(self.count("references_", "other"), 1, "another form's reference stays")
        self.assertTrue(list(self.db.parent.glob("history.db.bak-*")), "a backup is made first")
        self.bench("refs", "restore", "--config", str(self.config_path), "--db", str(self.db))
        for table in sb.REFERENCE_TABLES:
            self.assertEqual(self.count(table, ref), 1, table)

    def test_a_run_refuses_while_the_form_is_a_reference(self):
        self.make_db()
        with self.assertRaises(SystemExit) as stopped:
            self.bench("run", "--config", str(self.config_path), "--db", str(self.db), "--phase", "author")
        self.assertIn("refs remove", str(stopped.exception))

    def test_each_phase_varies_only_its_own_role(self):
        config = sb.load_config(self.config_path)
        run_dir = Path(self.tmp.name)
        author = sb.command(config, {"phase": "author", "model": "g/flash"}, run_dir)
        self.assertEqual(author[author.index("--model") + 1], "g/flash")
        self.assertEqual(author[author.index("--judge-model") + 1], "a/opus")
        self.assertEqual(author[author.index("--max-review-rounds") + 1], "0")
        review = sb.command(config, {"phase": "review", "model": "g/flash", "target": "S"}, run_dir)
        self.assertIn("--assess", review)
        self.assertEqual(review[review.index("--max-review-rounds") + 1], "1")
        self.assertEqual(review[review.index("--judge-model") + 1], "a/opus")
        self.assertEqual(review[review.index("--reviewer-model") + 1], "g/flash", "not the app's saved one")
        self.assertIn("--no-judges", author)
        self.assertIn("--no-judges", review)
        score = sb.command(config, {"phase": "score", "model": "a/opus", "target": "S"}, run_dir)
        self.assertEqual(score[score.index("--max-review-rounds") + 1], "0")
        self.assertNotIn("--no-judges", score)
        self.assertEqual(author[author.index("--reviewer-model") + 1], "a/opus")
        judge = sb.command(config, {"phase": "judge", "model": "g/flash", "target": "S"}, run_dir)
        self.assertEqual(judge[judge.index("--max-review-rounds") + 1], "0")
        self.assertEqual(judge[judge.index("--judge-model") + 1], "g/flash")
        self.assertNotIn("--no-judges", judge)

    def test_a_full_benchmark_runs_once_and_reports_every_stage(self):
        self.make_db(label="AAGS_only")  # the hash still matches: remove it first
        self.bench("refs", "remove", "--config", str(self.config_path), "--db", str(self.db))
        self.bench("run", "--config", str(self.config_path), "--db", str(self.db))
        out = Path(self.tmp.name) / "out"
        manifest = sb.Manifest(out)
        phases = sorted(r["phase"] for r in manifest.runs.values())
        # 2 authors × 2 repeats; the 3 Author results the baseline does not
        # review are graded; 2 reviewers and 2 judges × 2 repeats on the target.
        self.assertEqual(phases.count("author"), 4)
        # The baseline judge's run on the target scores it; the other three
        # Author results get a score run of their own.
        self.assertEqual(phases.count("score"), 3)
        self.assertEqual(phases.count("grade"), 3)
        self.assertEqual(phases.count("review"), 4)
        self.assertEqual(phases.count("judge"), 4)
        self.assertTrue(all(r["status"] == "ok" for r in manifest.runs.values()))

        again = self.bench("run", "--config", str(self.config_path), "--db", str(self.db))
        self.assertNotIn("…", again, "a finished run is not run again")

        report = (out / "report.md").read_text()
        self.assertIn("## Author", report)
        with (out / "judges.csv").open() as f:
            judges = list(csv.DictReader(f))
        flash = [j for j in judges if j["model"] == "g/flash"]
        self.assertTrue(flash and all(abs(float(j["agreement"]) - 2 / 3) < 1e-9 for j in flash), flash)
        self.assertIn("rule-b", report, "a disputed rule is listed")
        with (out / "authors.csv").open() as f:
            authors = list(csv.DictReader(f))
        self.assertTrue(all(a["grade_issues"] == "2" for a in authors), authors)
        self.assertTrue(all(a["scripted_failing"] == "1" and a["judged_failing"] == "1" for a in authors), authors)

    def test_an_older_summary_without_the_board_falls_back_to_the_judges(self):
        summary = {"judges": {"rule_checks": [{"check": 2, "positive": 1, "negative": 1, "unchecked": 0,
                                               "judged": 2}],
                              "runs": [{"check": 1, "rule_name": "x", "verdict": "negative"},
                                       {"check": 2, "rule_name": "x", "verdict": "positive"},
                                       {"check": 2, "rule_name": "y", "verdict": "negative"}]}}
        self.assertEqual(sb.final_check(summary)["verdicts"], {"x": "positive", "y": "negative"})

    def test_a_phase_stops_after_two_runs_that_fail_at_once(self):
        config = json.loads(self.config_path.read_text())
        config["candidates"]["author"] = ["x/broken", "a/opus"]
        self.config_path.write_text(json.dumps(config))
        with self.assertRaises(SystemExit) as stopped:
            self.bench("run", "--config", str(self.config_path), "--db", str(self.db), "--phase", "author")
        self.assertIn("two author runs in a row", str(stopped.exception))
        runs = sb.Manifest(Path(self.tmp.name) / "out").runs
        self.assertEqual(sorted(runs), ["author/x_broken/r1", "author/x_broken/r2"], "the rest is not started")

    def test_the_key_is_trimmed(self):
        os.environ["OPENROUTER_API_KEY"] = "  sk-or-abc\n"
        self.assertEqual(sb.api_key(sb.load_config(self.config_path)), "sk-or-abc")

    def test_grades_filled_in_become_precision_and_recall(self):
        self.bench("run", "--config", str(self.config_path), "--db", str(self.db))
        out = Path(self.tmp.name) / "out"
        path = out / "review-grades.csv"
        with path.open() as f:
            rows = list(csv.DictReader(f))
        for row in rows:
            row.update(found="2", false="0", missed="2")
        with path.open("w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
            w.writeheader()
            w.writerows(rows)
        self.bench("report", "--config", str(self.config_path))
        with (out / "reviewers.csv").open() as f:
            reviewers = list(csv.DictReader(f))
        self.assertTrue(all(r["precision"] == "1.0" and r["recall"] == "0.5" for r in reviewers), reviewers)
        with path.open() as f:
            self.assertEqual(next(csv.DictReader(f))["found"], "2", "the grades survive a new report")


if __name__ == "__main__":
    unittest.main()
