#!/usr/bin/env python3
"""Tests for release.yml's publication guards.

The recovery-dispatch guard decides whether a `workflow_dispatch` run may
publish the tag it asks for, and the asset-name check decides whether what the
release publishes is what a desktop downloads. Both get discriminating tests
rather than smoke tests.

The dispatch guard's shell is `scripts/require-tag-ref-dispatch.sh` itself --
the same file the workflow step runs. The asset-name check has no script of its
own, so its tests decode the step's `run` with `yaml.safe_load`, the way GitHub
reads it, and execute that over a stub checkout. Nothing here re-types either.
"""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
GUARD = ROOT / "scripts/require-tag-ref-dispatch.sh"
WORKFLOW = ROOT / ".github/workflows/release.yml"
CONTRACT = ROOT / "packaging/desktop-platforms.txt"


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


class DesktopDownloadNameTests(unittest.TestCase):
    """The release must publish the filenames the desktop shell downloads.

    The built artifacts and the upload list are both publisher-side spellings:
    rename a pair in both and they still agree while the canonical URL 404s.
    native_engine.rs fetches `phase-server-slim-<triple>`, plus `.exe` on
    windows, and that name plus `.minisig`; the contract's os/triple columns
    are what CI already holds that mapping to.
    """

    @classmethod
    def setUpClass(cls) -> None:
        steps = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))["jobs"]["release"]["steps"]
        runs = [str(step.get("run", "")) for step in steps]
        cls.check = cls._only(runs, "artifacts/phase-server-slim-*/*")
        listing = re.search(r"cat <<'EOF'\n(.*?)\n *EOF\n", cls._only(runs, "files<<EOF"), re.S)
        assert listing, "release.yml's asset list is no longer a heredoc"
        cls.assets = listing.group(1).split()

    @staticmethod
    def _only(runs: list[str], needle: str) -> str:
        found = [run for run in runs if needle in run]
        assert len(found) == 1, f"{len(found)} release steps contain {needle!r}"
        return found[0]

    def publish(self, assets: list[str]) -> subprocess.CompletedProcess:
        """Runs the shipped check over a checkout whose built artifacts and
        upload list are both `assets` -- the agreement it has to see past."""
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            (work / CONTRACT.relative_to(ROOT).parent).mkdir()
            (work / CONTRACT.relative_to(ROOT)).write_bytes(CONTRACT.read_bytes())
            for asset in assets:
                (work / asset).parent.mkdir(parents=True, exist_ok=True)
                (work / asset).write_text("", encoding="utf-8")
            return subprocess.run(
                ["bash", "-c", self.check], cwd=work,
                env={"PATH": os.environ["PATH"], "RELEASE_FILES": "\n".join(assets)},
                capture_output=True, text=True, timeout=60,
            )

    def test_the_shipped_asset_list_publishes_every_canonical_download(self) -> None:
        # The names are printed, so this passing cannot be a check that never ran.
        result = self.publish(self.assets)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        printed = re.search(r"desktop download names: (.*)", result.stdout)
        self.assertIsNotNone(printed, result.stdout)
        rows = [line.split("#")[0].split() for line in CONTRACT.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(
            sorted(printed.group(1).split()),
            sorted(f"phase-server-slim-{triple}{'.exe' if platform == 'windows' else ''}"
                   for platform, _arch, triple in (row for row in rows if row)),
        )

    def test_a_renamed_pair_the_upload_list_agrees_with_is_rejected(self) -> None:
        # What the artifact/list check cannot see: both publisher-side spellings
        # agree, and every desktop on that platform asks for a name nobody
        # published. The release is immutable, so it has to fail before create.
        canonical = "phase-server-slim-aarch64-apple-darwin"
        result = self.publish([a.replace(canonical, f"{canonical}-renamed") for a in self.assets])
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        for missing in (canonical, f"{canonical}.minisig"):
            self.assertIn(f"::error::no release asset is published as {missing}, which", result.stdout)


if __name__ == "__main__":
    unittest.main()
