#!/usr/bin/env python3
"""Tests for the release recovery-dispatch guard.

The guard decides whether a `workflow_dispatch` run of release.yml may publish
the tag it asks for, so it gets a discriminating test rather than a smoke test.

The shell under test is `scripts/require-tag-ref-dispatch.sh` itself -- the same
file the workflow step runs. Nothing here re-types the guard, and nothing here
executes text pulled out of a YAML file.
"""

from __future__ import annotations

import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GUARD = ROOT / "scripts/require-tag-ref-dispatch.sh"
WORKFLOW = ROOT / ".github/workflows/release.yml"


def dispatch(**env: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(GUARD)],
        env={"PATH": "/usr/bin:/bin", **env},
        capture_output=True, text=True,
    )


class RecoveryDispatchGuardTests(unittest.TestCase):
    def test_branch_ref_is_rejected(self) -> None:
        r = dispatch(REF_TYPE="branch", REF_NAME="main", INPUT_TAG="v0.72.0")
        self.assertEqual(r.returncode, 1)
        self.assertIn("::error::", r.stdout)
        self.assertIn("not from branch 'main'", r.stdout)

    def test_tag_ref_releasing_a_different_tag_is_rejected(self) -> None:
        # The environment admits any policy-matching tag, but the steps that
        # follow release `inputs.tag`. Without this, a run started from v0.71.0
        # publishes v0.72.0.
        r = dispatch(REF_TYPE="tag", REF_NAME="v0.71.0", INPUT_TAG="v0.72.0")
        self.assertEqual(r.returncode, 1)
        self.assertIn("::error::", r.stdout)
        self.assertIn("v0.71.0", r.stdout)
        self.assertIn("v0.72.0", r.stdout)

    def test_tag_ref_matching_the_requested_release_is_accepted(self) -> None:
        r = dispatch(REF_TYPE="tag", REF_NAME="v0.72.0", INPUT_TAG="v0.72.0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn("::error::", r.stdout)

    def test_the_two_rejections_are_distinguishable(self) -> None:
        # Distinct messages, so neither failure mode hides behind the other and
        # an operator learns which precondition they tripped.
        branch = dispatch(REF_TYPE="branch", REF_NAME="main", INPUT_TAG="v0.72.0").stdout
        mismatch = dispatch(REF_TYPE="tag", REF_NAME="v0.71.0", INPUT_TAG="v0.72.0").stdout
        self.assertNotEqual(branch, mismatch)

    def test_a_missing_input_fails_rather_than_comparing_empty_strings(self) -> None:
        r = dispatch(REF_TYPE="tag", REF_NAME="v0.72.0")
        self.assertNotEqual(r.returncode, 0)

    def test_the_workflow_runs_this_guard_on_the_dispatch_path(self) -> None:
        # Without this the tests above could pass while the workflow no longer
        # calls the guard, or no longer gives it the values it branches on.
        text = WORKFLOW.read_text(encoding="utf-8")
        step = re.search(
            r"- name: Require a tag ref for recovery dispatches\n(.*?)(?=\n      - )",
            text, re.S)
        self.assertIsNotNone(step, "release.yml has no recovery-dispatch guard step")
        body = step.group(1)
        self.assertIn("if: github.event_name == 'workflow_dispatch'", body)
        self.assertIn("run: ./scripts/require-tag-ref-dispatch.sh", body)
        for var, expr in (
            ("REF_TYPE", "github.ref_type"),
            ("REF_NAME", "github.ref_name"),
            ("INPUT_TAG", "inputs.tag"),
        ):
            self.assertRegex(body, rf"{var}:\s*\$\{{\{{\s*{expr}\s*\}}\}}")


if __name__ == "__main__":
    unittest.main()
