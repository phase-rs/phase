#!/usr/bin/env python3
"""Tests for the preview manifest's wait on the data files it names.

preview-server.yml's `publish` job signs the desktop preview manifest, and that
manifest entry names two content-addressed data objects that a job of the
calling workflow uploads. A reusable workflow cannot `needs:` a caller's job, so
the ordering is enforced inside `publish`: it waits until both URLs are served
before it signs anything.

The shell under test is `scripts/wait-for-preview-data.sh` itself, driven
against a local HTTP server that answers HEAD the way the data endpoint does.
The wiring tests decode the workflows with `yaml.safe_load`, the way GitHub
reads them, and assert over the decoded structure -- not over workflow text.
"""

from __future__ import annotations

import functools
import http.server
import re
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Iterator

import yaml

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/wait-for-preview-data.sh"
PREVIEW_SERVER_WORKFLOW = ROOT / ".github/workflows/preview-server.yml"
DEPLOY_WORKFLOW = ROOT / ".github/workflows/deploy.yml"

CARD_DATA = "card-data.json"
DRAFT_POOLS = "draft-pools.json"

GATE_IF = "steps.publication-gate.outputs.already_published != 'true'"
WAIT_ENV = {"DATA_DEADLINE_EPOCH": "${{ needs.gate.outputs.data_deadline_epoch }}"}
WAIT_RUN = re.compile(r'bash scripts/wait-for-preview-data\.sh( "\$[A-Z_]+")+')
URL_ARG = re.compile(r'--arg (\w+)_url "\$([A-Z_]+)"')
# GitHub applies success() to an `if` without a status-check function, and its
# detection and function lookup are both case-insensitive.
STATUS_FUNCTION = re.compile(r"(?i)\b(success|always|failure|cancelled)\s*\(")
DEADLINE_LINE = (
    'echo "data_deadline_epoch=$(( $(date +%s) + CARD_DATA_TIMEOUT_SECONDS'
    ' + DATA_POLL_SECONDS ))" >> "$GITHUB_OUTPUT"'
)
STAGING_PREFIX = "https://data.phase-rs.dev/staging/"


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args: object) -> None:
        pass


class DataWaitScriptTests(unittest.TestCase):
    """The stub answers HEAD 200 for an object that exists and 404 for one that
    does not, which is the status contract measured on the data endpoint."""

    @classmethod
    def setUpClass(cls) -> None:
        cls._objects = tempfile.TemporaryDirectory()
        handler = functools.partial(_QuietHandler, directory=cls._objects.name)
        cls._server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=cls._server.serve_forever, daemon=True).start()
        cls.base = f"http://127.0.0.1:{cls._server.server_address[1]}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls._server.shutdown()
        cls._objects.cleanup()

    def put(self, name: str) -> None:
        Path(self._objects.name, name).write_text("{}", encoding="utf-8")

    def uploaded(self, *names: str) -> None:
        for name in (CARD_DATA, DRAFT_POOLS):
            Path(self._objects.name, name).unlink(missing_ok=True)
        for name in names:
            self.put(name)

    def wait(self, deadline_in: int, poll: int) -> subprocess.CompletedProcess:
        # The timeout turns a script that ignores its deadline into an error
        # rather than a hang.
        return subprocess.run(
            ["bash", str(SCRIPT), f"{self.base}/{CARD_DATA}", f"{self.base}/{DRAFT_POOLS}"],
            env={
                "PATH": "/usr/bin:/bin",
                "DATA_DEADLINE_EPOCH": str(int(time.time()) + deadline_in),
                "DATA_POLL_SECONDS": str(poll),
            },
            capture_output=True,
            text=True,
            timeout=60,
        )

    @staticmethod
    def errors(result: subprocess.CompletedProcess) -> str:
        return "".join(
            line for line in result.stdout.splitlines(True) if line.startswith("::error::")
        )

    def test_both_files_served_passes(self) -> None:
        self.uploaded(CARD_DATA, DRAFT_POOLS)
        result = self.wait(deadline_in=2, poll=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("::error::", result.stdout)
        for name in (CARD_DATA, DRAFT_POOLS):
            self.assertIn(f"Available: {self.base}/{name}", result.stdout)

    def test_each_missing_file_fails_and_is_named(self) -> None:
        for absent, present in ((CARD_DATA, DRAFT_POOLS), (DRAFT_POOLS, CARD_DATA)):
            with self.subTest(absent=absent):
                self.uploaded(present)
                result = self.wait(deadline_in=2, poll=1)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                errors = self.errors(result)
                self.assertIn(absent, errors)
                self.assertNotIn(present, errors)

    def test_a_file_uploaded_during_the_wait_passes(self) -> None:
        self.uploaded(DRAFT_POOLS)
        upload = threading.Timer(1.5, self.put, [CARD_DATA])
        upload.start()
        try:
            result = self.wait(deadline_in=10, poll=1)
        finally:
            upload.join()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The first round missed it, so exit 0 came from a later round.
        self.assertIn("Waiting (HTTP 404)", result.stdout)
        self.assertIn(f"Available: {self.base}/{CARD_DATA}", result.stdout)

    def test_a_past_deadline_still_checks_once(self) -> None:
        # A re-run of publish alone reuses the original deadline, and a manual
        # dispatch can start after it: the existence check still has to happen.
        self.uploaded(CARD_DATA, DRAFT_POOLS)
        result = self.wait(deadline_in=-60, poll=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        self.uploaded(DRAFT_POOLS)
        result = self.wait(deadline_in=-60, poll=30)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(CARD_DATA, self.errors(result))
        self.assertEqual(result.stdout.count("Waiting (HTTP 404)"), 1)


def _env_scopes(workflow: dict) -> Iterator[tuple[str, object]]:
    yield "env", workflow.get("env")
    for name, job in workflow.get("jobs", {}).items():
        yield f"jobs.{name}.env", job.get("env")
        for index, step in enumerate(job.get("steps", [])):
            yield f"jobs.{name}.steps[{index}].env", step.get("env")


def _run_strings(workflow: dict) -> list[str]:
    return [
        str(step.get("run", ""))
        for job in workflow.get("jobs", {}).values()
        for step in job.get("steps", [])
    ]


class PublishWiringTests(unittest.TestCase):
    """Reads possibly-absent keys with `.get` and coerces with `str` so a
    failure names the property it checks."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.preview = yaml.safe_load(PREVIEW_SERVER_WORKFLOW.read_text(encoding="utf-8"))
        cls.deploy = yaml.safe_load(DEPLOY_WORKFLOW.read_text(encoding="utf-8"))

    def bound_once(self, variable: str, scope: str) -> None:
        scopes = [
            name
            for name, env in _env_scopes(self.preview)
            if isinstance(env, dict) and variable in env
        ]
        self.assertEqual(scopes, [scope], f"{variable} must be bound only at {scope}")
        for run in _run_strings(self.preview):
            self.assertNotRegex(run, rf"\b{variable}=", f"a run step rebinds {variable}")

    def wait_and_sign(self) -> tuple[list[dict], int, int]:
        steps = self.preview["jobs"]["publish"]["steps"]
        waits = [
            index
            for index, step in enumerate(steps)
            if WAIT_RUN.fullmatch(str(step.get("run", "")).strip())
        ]
        self.assertEqual(len(waits), 1, "publish must run the wait script in exactly one step")
        signs = [
            index for index, step in enumerate(steps) if URL_ARG.search(str(step.get("run", "")))
        ]
        self.assertEqual(len(signs), 1, "publish must sign manifest data URLs in exactly one step")
        return steps, waits[0], signs[0]

    def test_the_wait_step_carries_nothing_but_its_deadline(self) -> None:
        steps, wait_index, _ = self.wait_and_sign()
        wait = steps[wait_index]
        # A `continue-on-error`, `shell`, or `defaults.run.shell` key would let
        # publish proceed past an unavailable-data failure.
        self.assertEqual(set(wait), {"name", "if", "env", "run"})
        self.assertEqual(wait.get("if"), GATE_IF)
        self.assertEqual(wait.get("env"), WAIT_ENV)
        self.assertNotIn("defaults", self.preview)
        self.assertNotIn("defaults", self.preview["jobs"]["publish"])

    def test_no_step_after_the_wait_overrides_its_failure(self) -> None:
        steps, wait_index, _ = self.wait_and_sign()
        later = steps[wait_index + 1 :]
        self.assertTrue(later, "signing and publishing must follow the wait")
        for step in later:
            with self.subTest(step=step.get("name", step.get("uses"))):
                self.assertNotRegex(str(step.get("if", "")), STATUS_FUNCTION)

    def test_publish_signs_only_after_a_wait_on_every_manifest_data_url(self) -> None:
        steps, wait_index, sign_index = self.wait_and_sign()
        self.assertLess(wait_index, sign_index, "the wait must precede signing")
        sign_run = str(steps[sign_index]["run"])
        args = URL_ARG.findall(sign_run)
        variables = {variable for _, variable in args}
        self.assertTrue(variables, "the signing step must take its data URLs from the environment")
        probed = set(re.findall(r'"\$([A-Z_]+)"', str(steps[wait_index]["run"])))
        self.assertEqual(probed, variables, "the wait must probe every signed data URL")

        for variable in sorted(variables):
            with self.subTest(variable=variable):
                self.bound_once(variable, "jobs.publish.env")
                value = str((self.preview["jobs"]["publish"].get("env") or {}).get(variable, ""))
                self.assertTrue(value.startswith(STAGING_PREFIX), f"{variable} is {value!r}")

        block = re.search(r"data: \[(.*?)\n\s*\]", sign_run, re.S)
        self.assertIsNotNone(block, "the signing step must build a manifest data array")
        urls = re.findall(r"url: (.*)", block.group(1))
        self.assertTrue(urls, "the manifest data array must name data URLs")
        names = {name for name, _ in args}
        for url in urls:
            with self.subTest(url=url):
                signed = re.fullmatch(r"\$(\w+)_url", url.strip())
                self.assertIsNotNone(signed, "each data URL must be a probed argument")
                self.assertIn(signed.group(1), names)

    def test_the_data_deadline_is_card_datas_own_timeout(self) -> None:
        card_data = self.deploy["jobs"]["card-data"]
        self.assertEqual(
            (self.preview.get("env") or {}).get("CARD_DATA_TIMEOUT_SECONDS"),
            card_data["timeout-minutes"] * 60,
        )
        self.bound_once("CARD_DATA_TIMEOUT_SECONDS", "env")
        self.bound_once("DATA_POLL_SECONDS", "env")

    def test_the_gate_publishes_one_deadline_for_the_wait_to_read(self) -> None:
        gate = self.preview["jobs"]["gate"]
        steps = [step for step in gate.get("steps", []) if step.get("id") == "gate"]
        self.assertEqual(len(steps), 1)
        self.assertIn(DEADLINE_LINE, str(steps[0].get("run", "")))
        writes = sum(
            len(re.findall(r"\bdata_deadline_epoch=", run)) for run in _run_strings(self.preview)
        )
        self.assertEqual(writes, 1, "the deadline must be written once")
        self.assertEqual(
            gate.get("outputs", {}).get("data_deadline_epoch"),
            "${{ steps.gate.outputs.data_deadline_epoch }}",
        )

    def test_deploy_releases_card_data_no_later_than_the_gate(self) -> None:
        # The deadline counts from the gate, so card-data must not start after
        # it; a need on card-data alone would leave the wait expiring first.
        def needs(job: str) -> set[str]:
            value = self.deploy["jobs"][job].get("needs")
            return {value} if isinstance(value, str) else set(value or [])

        self.assertTrue(
            needs("card-data") <= needs("preview-server"),
            f"card-data needs {needs('card-data')}, preview-server needs {needs('preview-server')}",
        )


if __name__ == "__main__":
    unittest.main()
