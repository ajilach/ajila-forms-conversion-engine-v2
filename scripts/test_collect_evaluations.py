#!/usr/bin/env python3
"""Smoke tests for collect_evaluations.py: `python3 scripts/test_collect_evaluations.py`.

`testdata/evaluation-template.md` is an `evaluation.md` exactly as the run
analyzer writes it (`runner::analysis::render::evaluation_template`); refresh
it from a recorded run when the template changes.
"""

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
import collect_evaluations  # noqa: E402

TEMPLATE = (HERE / "testdata" / "evaluation-template.md").read_text(encoding="utf-8")


def run_folder(root: Path, name: str, evaluation: str, approved=True) -> Path:
    folder = root / name
    folder.mkdir()
    (folder / "evaluation.md").write_text(evaluation, encoding="utf-8")
    (folder / "summary.json").write_text(json.dumps({
        "schema_version": 1, "finished": True, "approved": approved,
        "time": {"wall_ms": 1_800_000}, "spend": {"cost_usd": 12.5},
    }), encoding="utf-8")
    return folder


def filled() -> str:
    return (TEMPLATE
            .replace("\nverdict:\n", "\nverdict: major-rework\n")
            .replace("\nscore:\n", "\nscore: 2\n")
            .replace("\nmanual_fix_minutes:\n", "\nmanual_fix_minutes: 90\n")
            .replace("| 1 | | | | | |",
                     "| 1 | behaviour | major | P3 | occurrences wrong | fixed by hand |\n"
                     "| 2 | `text` | minor | Footer | EN text in DE | |")
            .replace("\n- R:\n", "\n- R: occurrences from the source (finding 1)\n"))


class CollectEvaluations(unittest.TestCase):
    def test_the_untouched_template_is_pending(self):
        with tempfile.TemporaryDirectory() as tmp:
            run_folder(Path(tmp), "run-a", TEMPLATE)
            run = collect_evaluations.load(Path(tmp) / "run-a" / "evaluation.md")
            self.assertFalse(run["filled"])
            self.assertEqual(run["findings"], [])
            self.assertEqual(run["requirements"], [])

    def test_a_filled_evaluation_is_collected_with_its_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            run_folder(root, "run-a", filled(), approved=None)
            run_folder(root, "run-b", TEMPLATE)
            out = io.StringIO()
            with redirect_stdout(out):
                code = collect_evaluations.main([str(root), "--csv", str(root / "ev.csv")])
            self.assertEqual(code, 0)
            text = out.getvalue()
            self.assertIn("1 evaluated run(s), 1 still to evaluate, 2 finding(s), 1 requirement(s)", text)
            self.assertIn("| major-rework | 2 | 90 | 30m 00s | USD 12.50 | no verdict | 2 (0/1/1) |", text)
            self.assertIn("| text | 1 | 0 | 0 | 1 | 1 |", text, "backticks around a category are dropped")
            self.assertIn("- occurrences from the source (finding 1)", text)
            self.assertIn("`run-b`", text.split("## Not evaluated yet")[1])
            rows = (root / "ev.csv").read_text().splitlines()
            self.assertEqual(len(rows), 2, "only evaluated runs go into the CSV")


if __name__ == "__main__":
    unittest.main()
