#!/usr/bin/env python3
"""Smoke tests for analyze_runs.py: `python3 scripts/test_analyze_runs.py`."""

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import analyze_runs  # noqa: E402


def summary(code: str, approved, wall_ms: int = 60_000) -> dict:
    """A minimal run summary in the layout `runner::analysis` writes."""
    return {
        "schema_version": 1,
        "run": {"label": f"{code}.pdf", "started": "2026-09-30T14:00:00+02:00", "kind": "fresh conversion"},
        "finished": True,
        "outcome": {"produced": True, "form_code": code},
        "approved": approved,
        "time": {"wall_ms": wall_ms, "model_ms": 40_000, "tool_ms": 10_000, "shaping_ms": 0,
                 "failed_request_ms": 0, "other_ms": 10_000},
        "spend": {"cost_usd": 1.5},
        "counts": {"stages": 3, "turns": 20, "tool_calls": 30, "failed_tool_calls": 2},
        "stages": [{"name": "Author", "duration_ms": 50_000, "model_ms": 40_000, "tool_ms": 10_000,
                    "turns": 20, "ended": "finished", "spend": {"cost_usd": 1.0}}],
        "tools": [{"name": "validate_aem_package", "calls": 5, "failures": 2, "total_ms": 5_000,
                   "max_ms": 2_000, "result_chars": 1_000}],
        "recurring_errors": [{"tool": "validate_aem_package", "pattern": "field '…' has no bindRef",
                              "count": 2, "example": "field 'IBAN' has no bindRef"}],
        "repeated_calls": [{"name": "get_xfa", "count": 3, "distinct_results": 1, "total_ms": 300}],
        "control_counts": {"output-cap nudge": 1},
        "review_verdicts": [{"round": 1, "approved": approved, "verdict": "changes requested",
                             "report": "The footer is missing."}],
    }


def judged_summary(code: str) -> dict:
    """A schema-2 summary: the same run, with its judges recorded."""
    run = summary(code, True)
    run["schema_version"] = 2
    run["judge_runs"] = 3
    run["judge_cost_usd"] = 0.9
    run["agent_cost_usd"] = 0.6
    run["stages"][0].update({"ended": "review_submitted", "ended_by": "finish_authoring", "judge_runs": 3,
                             "judges_spend": {"cost_usd": 0.9}, "agent_spend": {"cost_usd": 0.1}})
    run["judges"] = {"final_check": {"name": "Final check", "duration_ms": 30_000, "model_ms": 0, "tool_ms": 0,
                                     "turns": 0, "ended": "finished", "judge_runs": 1,
                                     "spend": {"cost_usd": 0.2}, "judges_spend": {"cost_usd": 0.2},
                                     "agent_spend": {"cost_usd": None}},
                     "by_rule": [
        {"rule_id": "id-a", "rule_name": "sub-headings-are-title-draws", "rule_title": "Sub-headings",
         "runs": 2, "positive": 0, "negative": 2, "unchecked": 0, "turns": 14, "total_ms": 60_000,
         "spend": {"cost_usd": 0.7}},
        {"rule_id": "id-b", "rule_name": "verbatim-source-text", "rule_title": "Verbatim",
         "runs": 1, "positive": 1, "negative": 0, "unchecked": 0, "turns": 5, "total_ms": 20_000,
         "spend": {"cost_usd": 0.2}},
    ]}
    return run


class AnalyzeRunsTest(unittest.TestCase):
    def run_script(self, *paths: Path) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = analyze_runs.main([str(p) for p in paths])
        return code, out.getvalue(), err.getvalue()

    def test_errors_recurring_across_forms_are_grouped(self):
        with tempfile.TemporaryDirectory() as tmp:
            for code in ("AAOS", "BAGE"):
                run = Path(tmp) / f"2026-09-30_1400_{code}"
                run.mkdir()
                (run / "summary.json").write_text(json.dumps(summary(code, False)))
            status, out, _ = self.run_script(Path(tmp))
        self.assertEqual(status, 0)
        self.assertIn("2 run(s)", out)
        self.assertIn("| `validate_aem_package` | 2 | 4 |", out, "grouped across both runs")
        self.assertIn("The footer is missing.", out)
        self.assertIn("| `get_xfa` | 2 | 4 | 4 |", out)

    def test_judges_are_aggregated_and_older_runs_show_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name, run in (("2026-10-08_1400_OLD", summary("OLD", False)),
                              ("2026-10-09_1400_NEW", judged_summary("NEW"))):
                d = Path(tmp) / name
                d.mkdir()
                (d / "summary.json").write_text(json.dumps(run))
            status, out, err = self.run_script(Path(tmp))
        self.assertEqual(status, 0, err)
        self.assertIn("2 run(s)", out)
        self.assertIn("- Judges: 3 run(s), USD 0.90 (60% of the spend of the 1 run(s) that recorded them)", out)
        self.assertIn("| USD 1.50 | USD 0.90 (3) | `2026-10-09_1400_NEW` |", out)
        self.assertIn("| USD 1.50 | - | `2026-10-08_1400_OLD` |", out, "a schema-1 run recorded no judges")
        self.assertIn("## Judges across runs, by cost", out)
        self.assertIn("| `sub-headings-are-title-draws` | 1 | 2 | 0 | 2 | 0 | 7.0 | 1m 00s | USD 0.70 |", out)
        self.assertIn("finish_authoring ×1", out, "the stage end names the terminal tool")
        self.assertIn("| USD 2.00 | USD 0.10 of 1 | USD 0.90 (3) of 1 |", out,
                      "the split covers the one run that recorded it, and says so")
        self.assertIn("| Final check | 1 | 1 |", out)

    def test_a_bad_file_is_skipped_not_fatal(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name, content in (("list", b"[1, 2]"), ("binary", b"\xff\xfe"), ("broken", b"{")):
                d = Path(tmp) / name
                d.mkdir()
                (d / "summary.json").write_bytes(content)
            good = Path(tmp) / "good"
            good.mkdir()
            (good / "summary.json").write_text(json.dumps(summary("AAOS", True)))
            status, out, err = self.run_script(Path(tmp))
        self.assertEqual(status, 0)
        self.assertIn("1 run(s)", out)
        self.assertEqual(err.count("warning: skipping"), 3, err)

    def test_nothing_to_read_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = self.run_script(Path(tmp))
        self.assertEqual(status, 1)
        self.assertIn("No recorded runs found", err)


if __name__ == "__main__":
    unittest.main()
