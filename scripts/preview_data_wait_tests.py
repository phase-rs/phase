#!/usr/bin/env python3
"""Tests for the preview manifest's wait on the data files it names.

preview-server.yml's `publish` job signs the desktop preview manifest, and that
manifest entry names two content-addressed data objects that a job of the
calling workflow uploads. The deploy caller orders that upload ahead of the
whole preview-server workflow with a `needs:` edge; a manual dispatch has no
such edge, so `publish` waits until both URLs are served before it signs
anything.

The shell under test is `scripts/wait-for-preview-data.sh` itself, driven
against a local HTTP server that answers HEAD the way the data endpoint does.
The wiring tests decode the workflows with `yaml.safe_load`, the way GitHub
reads them, and assert over the decoded structure -- not over workflow text.
The publish step's own `run` is one such decoded value, which `publish` below
runs against a stubbed checkout, so the platform check inside it is measured
rather than read.
"""

from __future__ import annotations

import functools
import http.server
import os
import re
import subprocess
import tempfile
import threading
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
WAIT_RUN = re.compile(r'bash scripts/wait-for-preview-data\.sh( "\$[A-Z_]+")+')
URL_ARG = re.compile(r'--arg (\w+)_url "\$([A-Z_]+)"')
# GitHub applies success() to an `if` without a status-check function, and its
# detection and function lookup are both case-insensitive.
STATUS_FUNCTION = re.compile(r"(?i)\b(success|always|failure|cancelled)\s*\(")
SCRIPT_REQUIRED = re.compile(r"\$\{(\w+):\?[^}]*\}")
STAGING_PREFIX = "https://data.phase-rs.dev/staging/"
# The two objects a manifest entry names, as the deploy job puts them. Other
# jobs upload to the same staging prefix, so the key is the object, not it.
MANIFEST_DATA_PUTS = (
    "phase-rs-data/staging/$CARD_DATA_FILENAME",
    "phase-rs-data/staging/$DRAFT_POOLS_FILENAME",
)
CONTRACT = ROOT / "packaging/desktop-platforms.txt"
# Every command the publish step shells out to, as one stub dispatching on the
# name it was invoked by: minisign writes a signature, npx records the upload
# and fills a bucket, curl answers out of it. None of this is under test -- it
# is the least that lets the shipped step run somewhere it cannot publish.
COMMAND_STUB = """#!/usr/bin/env python3
import os, shutil, sys
from pathlib import Path

me, argv, bucket = Path(sys.argv[0]).name, sys.argv[1:], Path(os.environ["BUCKET"])
if me == "minisign":
    Path(argv[argv.index("-x") + 1]).write_text("untrusted signature\\n")
elif me == "npx":  # npx wrangler r2 object <verb> <key> [--file <path>]
    with open(os.environ["UPLOAD_LOG"], "a") as log:
        log.write(f"{argv[3]} {argv[4]}\\n")
    if argv[3] == "put":
        (bucket / argv[4]).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(argv[argv.index("--file") + 1], bucket / argv[4])
elif me == "curl" and "-w" in argv:  # the manifest this run would supersede
    print("404", end="")
elif me == "curl" and "--get" in argv:  # the object listing retention walks
    print('{"success":true,"result":[],"result_info":{"is_truncated":false}}')
elif me == "curl":  # a just-published object, read back for verification
    published = bucket / "phase-rs-data" / argv[-1].split(".dev/")[1]
    if not published.is_file():
        sys.exit(22)
    shutil.copy(published, argv[argv.index("-o") + 1])
"""


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

    def wait(self, window: int, poll: int) -> subprocess.CompletedProcess:
        # The timeout turns a script that ignores its window into an error
        # rather than a hang.
        return subprocess.run(
            ["bash", str(SCRIPT), f"{self.base}/{CARD_DATA}", f"{self.base}/{DRAFT_POOLS}"],
            env={
                "PATH": "/usr/bin:/bin",
                "DATA_WAIT_SECONDS": str(window),
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
        result = self.wait(window=2, poll=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("::error::", result.stdout)
        for name in (CARD_DATA, DRAFT_POOLS):
            self.assertIn(f"Available: {self.base}/{name}", result.stdout)

    def test_each_missing_file_fails_and_is_named(self) -> None:
        for absent, present in ((CARD_DATA, DRAFT_POOLS), (DRAFT_POOLS, CARD_DATA)):
            with self.subTest(absent=absent):
                self.uploaded(present)
                result = self.wait(window=2, poll=1)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                errors = self.errors(result)
                self.assertIn(absent, errors)
                self.assertNotIn(present, errors)

    def test_a_file_uploaded_during_the_wait_passes(self) -> None:
        self.uploaded(DRAFT_POOLS)
        upload = threading.Timer(1.5, self.put, [CARD_DATA])
        upload.start()
        try:
            result = self.wait(window=10, poll=1)
        finally:
            upload.join()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The first round missed it, so exit 0 came from a later round.
        self.assertIn("Waiting (HTTP 404)", result.stdout)
        self.assertIn(f"Available: {self.base}/{CARD_DATA}", result.stdout)

    def test_the_tightest_window_probes_before_giving_up(self) -> None:
        # Zero seconds is the least a caller can ask for: the existence check
        # still happens, and a served object answers on the first round.
        self.uploaded(CARD_DATA, DRAFT_POOLS)
        result = self.wait(window=0, poll=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        # Absent, the verdict costs a confirming probe: one HEAD is not proof.
        self.uploaded(DRAFT_POOLS)
        result = self.wait(window=0, poll=1)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        errors = self.errors(result)
        self.assertIn(CARD_DATA, errors)
        self.assertNotIn(DRAFT_POOLS, errors)
        self.assertEqual(result.stdout.count(f"Available: {self.base}/{DRAFT_POOLS}"), 1)
        self.assertEqual(result.stdout.count("Waiting (HTTP 404)"), 2)

    def test_a_late_upload_passes_after_the_window_is_spent(self) -> None:
        # An upload the first round misses must still reach the manifest, even
        # when the window leaves no room for a third round.
        self.uploaded(DRAFT_POOLS)
        upload = threading.Timer(1.5, self.put, [CARD_DATA])
        upload.start()
        try:
            result = self.wait(window=0, poll=3)
        finally:
            upload.join()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Waiting (HTTP 404)", result.stdout)
        self.assertIn(f"Available: {self.base}/{CARD_DATA}", result.stdout)


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

    def test_publish_checks_out_the_commit_it_publishes(self) -> None:
        # The scripts and the platform contract this job reads have to come from
        # the commit whose binaries it signs, not from whichever ref is running
        # the workflow. `commit` is a required input, so there is no fallback.
        checkouts = [
            step.get("with") or {}
            for step in self.preview["jobs"]["publish"]["steps"]
            if "actions/checkout@" in str(step.get("uses", ""))
        ]
        self.assertEqual(len(checkouts), 1, "publish must check out exactly once")
        self.assertEqual(checkouts[0].get("ref"), "${{ inputs.commit }}")

    def test_the_wait_step_carries_nothing_but_its_run(self) -> None:
        steps, wait_index, _ = self.wait_and_sign()
        wait = steps[wait_index]
        # A `continue-on-error`, `shell`, or `defaults.run.shell` key would let
        # publish proceed past an unavailable-data failure.
        self.assertEqual(set(wait), {"name", "if", "run"})
        self.assertEqual(wait.get("if"), GATE_IF)
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

    def test_every_setting_the_script_requires_reaches_the_wait_step(self) -> None:
        # The wait step binds no environment of its own, so a workflow-side
        # rename would otherwise surface only when publish runs.
        required = set(SCRIPT_REQUIRED.findall(SCRIPT.read_text(encoding="utf-8")))
        self.assertTrue(required, "the script must require its settings explicitly")
        for variable in sorted(required):
            with self.subTest(variable=variable):
                self.bound_once(variable, "env")

    def test_the_gate_publishes_no_deadline_for_the_wait_to_inherit(self) -> None:
        # Readiness comes from the caller's job graph and from the wait's own
        # clock; a gate timestamp would be aged by everything in between.
        gate = self.preview["jobs"]["gate"]
        self.assertEqual(set(gate.get("outputs") or {}), {"already_published"})
        for run in _run_strings(self.preview):
            self.assertNotRegex(run, r"data_deadline_epoch")

    def publish(self, contract: str) -> tuple[subprocess.CompletedProcess, list[str]]:
        """Runs the shipped publish step over a checkout holding `contract`, and
        returns it with every upload the step managed to make."""
        runs = [run for run in _run_strings(self.preview) if "binaries=(" in run]
        self.assertEqual(len(runs), 1, "one step must list the binaries to publish")
        with tempfile.TemporaryDirectory() as tmp:
            work, stubs, log = Path(tmp, "work"), Path(tmp, "bin"), Path(tmp, "uploads")
            (work / "packaging").mkdir(parents=True)
            (work / CONTRACT.relative_to(ROOT)).write_text(contract, encoding="utf-8")
            stubs.mkdir()
            for command in ("sudo", "apt-get", "minisign", "npx", "curl"):
                # sudo and apt-get fall off the end of the stub and exit 0.
                (stubs / command).write_text(COMMAND_STUB, encoding="utf-8")
                (stubs / command).chmod(0o755)
            for entry in self.preview["jobs"]["build"]["strategy"]["matrix"]["include"]:
                artifact = work / "artifacts" / entry["triple"]
                artifact.mkdir(parents=True)
                binary = f"phase-server-{entry['triple']}{entry['extension']}"
                (artifact / binary).write_text(entry["triple"], encoding="utf-8")
            env = {name: name.lower() for name in self.preview["jobs"]["publish"]["env"]}
            result = subprocess.run(
                ["bash", "-c", runs[0]],
                cwd=work,
                env={**env, "PATH": f"{stubs}:{os.environ['PATH']}", "TMPDIR": tmp,
                     "BUCKET": str(Path(tmp, "bucket")), "UPLOAD_LOG": str(log)},
                capture_output=True,
                text=True,
                timeout=300,
            )
            # Read inside the sandbox: an upload log collected after cleanup is
            # empty whether the step published or not.
            uploads = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        return result, uploads

    def printed_set(self, stdout: str, label: str) -> list[str]:
        lines = [line for line in stdout.splitlines() if line.startswith(label)]
        self.assertEqual(len(lines), 1, f"the publish step must print {label!r} once")
        return sorted(lines[0].split(":", 1)[1].split())

    def test_publish_holds_the_contract_against_what_it_uploads(self) -> None:
        # Both sets are printed, so a pass here cannot be a check that never ran.
        result, uploads = self.publish(CONTRACT.read_text(encoding="utf-8"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        built = sorted(
            entry["triple"]
            for entry in self.preview["jobs"]["build"]["strategy"]["matrix"]["include"]
        )
        self.assertEqual(self.printed_set(result.stdout, "desktop-platforms.txt"), built)
        self.assertEqual(self.printed_set(result.stdout, "this job publishes"), built)
        self.assertIn("put phase-rs-data/desktop/preview-server.json", uploads)

    def test_a_listed_triple_publish_does_not_carry_stops_before_publication(self) -> None:
        # A triple this job builds and the contract lists, absent from the step's
        # own `binaries`, would leave that desktop with no preview URL at all.
        # Publication is the only authority that can see both, so it must refuse.
        unpublished = "linux   riscv64 riscv64gc-unknown-linux-musl\n"
        result, uploads = self.publish(CONTRACT.read_text(encoding="utf-8") + unpublished)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("riscv64gc-unknown-linux-musl", result.stdout)
        self.assertIn("::error::packaging/desktop-platforms.txt", result.stdout)
        self.assertEqual(uploads, [], "a mismatch must publish nothing at all")

    def test_a_published_triple_with_no_contract_row_stops_before_publication(self) -> None:
        # The other direction the same error promises: a binary this job uploads
        # that the contract does not list is a preview no desktop ever asks for.
        dropped = "aarch64-apple-darwin"
        rows = CONTRACT.read_text(encoding="utf-8").splitlines(True)
        result, uploads = self.publish("".join(r for r in rows if dropped not in r))
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(f"> {dropped}", result.stdout)
        self.assertEqual(uploads, [], "a mismatch must publish nothing at all")

    def test_the_deploy_caller_needs_the_job_that_uploads_the_data(self) -> None:
        def needs(job: str) -> set[str]:
            value = self.deploy["jobs"][job].get("needs")
            return {value} if isinstance(value, str) else set(value or [])

        uploaders = {
            name
            for name, job in self.deploy["jobs"].items()
            for step in job.get("steps") or []
            if all(put in str(step.get("run", "")) for put in MANIFEST_DATA_PUTS)
        }
        self.assertEqual(uploaders, {"card-data"}, f"manifest data is uploaded by {uploaders}")
        self.assertIn(
            "card-data", needs("preview-server"), f"preview-server needs {needs('preview-server')}"
        )


if __name__ == "__main__":
    unittest.main()
