#!/usr/bin/env python3
"""Tests for check_action_pins.py.

The script is a supply-chain gate, so what matters is not that it runs but that
it refuses what it should. Each case builds a throwaway repository, copies the
real script into it, and runs it there.
"""

from __future__ import annotations

import shutil
import sys
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_action_pins.py"
SHA = "0" * 40
ENTRY = ".github/workflows/entry.yml"


class PinGate:
    """A throwaway repo. The script cds to its own parent's parent, so copying
    it into <root>/scripts makes <root> the repository it inspects."""

    def __init__(self, root: Path) -> None:
        self.root = root
        (root / "scripts").mkdir(parents=True)
        shutil.copy2(SCRIPT, root / "scripts" / SCRIPT.name)

    def write(self, rel: str, *uses: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        body = "\n".join(f"      - uses: {u}" for u in uses)
        p.write_text(f"jobs:\n  j:\n    steps:\n{body}\n", encoding="utf-8")

    def mkdir(self, rel: str) -> None:
        (self.root / rel).mkdir(parents=True, exist_ok=True)

    def run(self) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(self.root / "scripts" / SCRIPT.name), ENTRY],
            capture_output=True, text=True, cwd=self.root,
        )


class CheckActionPinsTests(unittest.TestCase):
    def gate(self) -> PinGate:
        d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        return PinGate(Path(d))

    def test_all_pinned_passes(self) -> None:
        # The accepting case. Without it a gate that refuses everything would
        # look identical to a gate that works.
        g = self.gate()
        g.write(ENTRY, f"actions/checkout@{SHA} # v4.4.0")
        r = g.run()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_unpinned_external_action_fails(self) -> None:
        g = self.gate()
        g.write(ENTRY, "actions/checkout@v4")
        self.assertEqual(g.run().returncode, 1)

    def test_a_ref_shorter_than_a_full_sha_fails(self) -> None:
        g = self.gate()
        g.write(ENTRY, f"actions/checkout@{'0' * 39}")
        self.assertEqual(g.run().returncode, 1)

    def test_unpinned_ref_inside_a_called_workflow_fails(self) -> None:
        # The property the gate exists for: reachability, not file membership.
        g = self.gate()
        g.write(ENTRY, "./.github/workflows/child.yml")
        g.write(".github/workflows/child.yml", "actions/setup-node@v4")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        self.assertIn("child.yml", r.stderr)

    def test_unpinned_ref_inside_a_composite_action_fails(self) -> None:
        g = self.gate()
        g.write(ENTRY, "./.github/actions/thing")
        g.write(".github/actions/thing/action.yml", "actions/cache@v4")
        self.assertEqual(g.run().returncode, 1)

    def test_composite_action_without_a_manifest_fails(self) -> None:
        # Skipping it silently would leave the gate green while the runner fails
        # to resolve the action.
        g = self.gate()
        g.write(ENTRY, "./.github/actions/empty")
        g.mkdir(".github/actions/empty")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        self.assertIn("no action.yml", r.stderr)

    def test_remote_reusable_workflow_is_refused_even_when_pinned(self) -> None:
        # Its own `uses:` refs live in another repository, so a pinned parent
        # says nothing about the mutable children it may call.
        g = self.gate()
        g.write(ENTRY, f"other/repo/.github/workflows/build.yml@{SHA}")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        self.assertIn("remote reusable workflow", r.stderr)

    def test_missing_referenced_file_fails(self) -> None:
        g = self.gate()
        g.write(ENTRY, "./.github/workflows/gone.yml")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        self.assertIn("does not exist", r.stderr)

    def test_a_trusted_copy_audits_another_tree_through_PIN_CHECK_ROOT(self) -> None:
        # How the base-branch audit reads a pull request's workflows: the script
        # is the reviewed one, the tree it walks is the untrusted one.
        audited = self.gate()
        audited.write(".github/workflows/release.yml", "actions/checkout@v4")
        audited.write(".github/workflows/deploy.yml", f"actions/cache@{SHA} # v4.3.0")
        r = subprocess.run(
            [sys.executable, str(SCRIPT)],
            env={"PATH": "/usr/bin:/bin", "PIN_CHECK_ROOT": str(audited.root)},
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 1)
        self.assertIn("actions/checkout@v4", r.stderr)

    def test_a_checker_stubbed_in_the_audited_tree_is_never_consulted(self) -> None:
        # The bypass this exists to close: a pull request that defeats the gate
        # and edits a privileged workflow in one commit. The audited tree's own
        # copy of the script is inert because it is never executed.
        audited = self.gate()
        audited.write(".github/workflows/release.yml", "actions/checkout@v4")
        audited.write(".github/workflows/deploy.yml", f"actions/cache@{SHA} # v4.3.0")
        stub = audited.root / "scripts" / SCRIPT.name
        stub.write_text("import sys\nsys.exit(0)\n", encoding="utf-8")
        self.assertEqual(subprocess.run([sys.executable, str(stub)]).returncode, 0,
                         "the stub must pass, or this test proves nothing")
        r = subprocess.run(
            [sys.executable, str(SCRIPT)],
            env={"PATH": "/usr/bin:/bin", "PIN_CHECK_ROOT": str(audited.root)},
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 1)

    def test_quoted_and_flow_style_keys_do_not_evade_the_walk(self) -> None:
        # YAML accepts these as `uses`, so a gate that matched the text of
        # `uses:` reported success while three mutable refs went through.
        g = self.gate()
        (g.root / ENTRY).parent.mkdir(parents=True, exist_ok=True)
        (g.root / ENTRY).write_text(
            'jobs:\n  j:\n    steps:\n'
            '      - "uses": actions/checkout@v4\n'
            '      - {uses: actions/cache@v4}\n'
            "      - 'uses': actions/setup-node@v4\n",
            encoding="utf-8")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        for action in ("actions/checkout@v4", "actions/cache@v4", "actions/setup-node@v4"):
            self.assertIn(action, r.stderr)

    def test_a_container_action_on_a_mutable_tag_fails(self) -> None:
        # A tag moves, so it is the thing this gate exists to stop. There is no
        # separate policy covering container actions; this is where they are
        # checked.
        g = self.gate()
        g.write(ENTRY, "docker://alpine:3.20")
        r = g.run()
        self.assertEqual(r.returncode, 1)
        self.assertIn("pin it by digest", r.stderr)

    def test_a_container_action_pinned_by_digest_passes(self) -> None:
        g = self.gate()
        g.write(ENTRY, f"docker://alpine@sha256:{'a' * 64}")
        self.assertEqual(g.run().returncode, 0)

    def test_a_local_ref_climbing_out_of_the_tree_is_refused(self) -> None:
        # The outside action is fully pinned, so a checker that follows the ref
        # reports a clean tree. That is the bypass: the audited tree borrows the
        # workspace above it, which holds the trusted checkout.
        g = self.gate()
        outside = g.root.parent / "outside" / "act"
        outside.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, outside.parent, ignore_errors=True)
        (outside / "action.yml").write_text(
            f"runs:\n  steps:\n    - uses: actions/cache@{SHA} # v4\n", encoding="utf-8"
        )
        g.write(ENTRY, f"./../{outside.parent.name}/act")
        r = g.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("resolves outside the checked tree", r.stderr)

    def test_a_local_ref_through_a_symlink_out_of_the_tree_is_refused(self) -> None:
        # Same class, different mechanism: the ref names a contained path and
        # the filesystem does the climbing.
        g = self.gate()
        outside = g.root.parent / "linked" / "act"
        outside.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, outside.parent, ignore_errors=True)
        (outside / "action.yml").write_text(
            f"runs:\n  steps:\n    - uses: actions/cache@{SHA} # v4\n", encoding="utf-8"
        )
        g.mkdir(".github/actions")
        (g.root / ".github/actions/act").symlink_to(outside, target_is_directory=True)
        g.write(ENTRY, "./.github/actions/act")
        r = g.run()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("resolves outside the checked tree", r.stderr)


if __name__ == "__main__":
    unittest.main()
