#!/usr/bin/env python3
from __future__ import annotations

import argparse
import contextlib
import copy
import gzip
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from datetime import UTC, datetime
from pathlib import Path
from unittest import mock

import pr_flow_metrics as pfm
import pr_review

SCRIPT = Path(pfm.__file__).resolve()
TEMPLATE = pr_review.REPO_ROOT / ".github/PULL_REQUEST_TEMPLATE.md"
ACTING = "maint"
REPO = "phase-rs/phase"
NOW = datetime(2026, 11, 9, 12, 0, tzinfo=UTC)
LATER = datetime(2026, 12, 1, 12, 0, tzinfo=UTC)
H = [c * 40 for c in "0123456789abcdef"]


# ─── GitHub and event-log fixtures ───────────────────────────────────────────


def connection(nodes: list, total: int | None = None, next_cursor: str | None = None) -> dict:
    return {"totalCount": len(nodes) if total is None else total, "nodes": nodes,
            "pageInfo": {"hasNextPage": next_cursor is not None, "endCursor": next_cursor}}


def review(author: str, state: str, oid: str | None, at: str, db_id: int = 1, body: str = "", inline: int = 0) -> dict:
    return {"databaseId": db_id, "author": {"login": author}, "state": state, "submittedAt": at,
            "commit": {"oid": oid} if oid else None, "body": body, "comments": {"totalCount": inline}}


def commit(oid: str, at: str, parents: int = 1, headline: str = "work") -> dict:
    return {"__typename": "PullRequestCommit",
            "commit": {"oid": oid, "committedDate": at, "parents": {"totalCount": parents}, "messageHeadline": headline}}


def force_push(oid: str, at: str) -> dict:
    return {"__typename": "HeadRefForcePushedEvent", "createdAt": at, "afterCommit": {"oid": oid}}


def dismissal(db_id: int, previous: str, at: str = "2026-10-28T09:00:00Z") -> dict:
    return {"__typename": "ReviewDismissedEvent", "createdAt": at, "previousReviewState": previous, "review": {"databaseId": db_id}}


def assignment(kind: str, login: str, at: str) -> dict:
    return {"__typename": kind, "createdAt": at, "assignee": {"login": login}}


def pr_node(number: int, author: str = "alice", created: str = "2026-10-28T10:00:00Z", **fields) -> dict:
    node = {
        "number": number, "author": {"login": author, "__typename": "User"}, "createdAt": created,
        "mergedAt": None, "closedAt": None, "state": "OPEN", "isDraft": False, "additions": 100, "deletions": 10,
        "changedFiles": 3, "body": "", "headRefOid": H[1], "commits": {"totalCount": 1},
        "reviews": connection([]), "comments": connection([]), "timelineItems": connection([]),
    }
    node.update(fields)
    if node["mergedAt"]:
        node["state"], node["closedAt"] = "MERGED", node["closedAt"] or node["mergedAt"]
    return node


def event(pr: int, event_type: str, at: str, head: str = H[1], outcome: str | None = None, **extra) -> dict:
    row = {"pr": pr, "event_type": event_type, "timestamp": at, "head_sha": head, "summary": f"{event_type} {at}"}
    if outcome:
        row["outcome"] = outcome
    return row | extra


class FakeGitHub:
    """Answers the module's named GraphQL operations from fixtures; records every argv."""

    def __init__(self, prs: list[dict], count_delta: int = 0, followups: dict | None = None, fail_op: str | None = None) -> None:
        self.prs, self.count_delta, self.followups, self.fail_op = prs, count_delta, followups or {}, fail_op
        self.argv_log: list[list[str]] = []

    def queries(self) -> list[str]:
        return [next(a for a in argv if a.startswith("query=")) for argv in self.argv_log]

    def __call__(self, argv: list[str]):
        self.argv_log.append(argv)
        text = next(a for a in argv if a.startswith("query="))[len("query="):]
        variables = {}
        for flag, pair in zip(argv, argv[1:]):
            if flag in ("-f", "-F") and not pair.startswith("query="):
                key, _, value = pair.partition("=")
                variables[key] = int(value) if flag == "-F" else value
        if "viewer" in text:
            return {"data": {"viewer": {"login": ACTING}}}
        op = re.match(r"query (\w+)", text).group(1)
        if op == self.fail_op:
            raise subprocess.CalledProcessError(1, argv, stderr="boom")
        return {"data": getattr(self, f"op_{op}")(variables)}

    def op_Head(self, _variables: dict) -> dict:
        return {"repository": {"defaultBranchRef": {"target": {"oid": H[15]}}}}

    def op_Count(self, variables: dict) -> dict:
        if "is:open author:" in variables["q"]:
            return {"search": {"issueCount": 2}}
        return {"search": {"issueCount": len(self.prs) + self.count_delta}}

    def op_Search(self, variables: dict) -> dict:
        start = int(variables.get("c", 0))
        page = self.prs[start:start + 20]
        more = start + 20 < len(self.prs)
        return {"search": {"issueCount": len(self.prs) + self.count_delta, "nodes": copy.deepcopy(page),
                           "pageInfo": {"hasNextPage": more, "endCursor": str(start + 20) if more else None}}}

    def followup(self, field: str, variables: dict) -> dict:
        nodes, next_cursor = self.followups[(variables["n"], field)][variables["c"]]
        return {"repository": {"pullRequest": {field: {"nodes": copy.deepcopy(nodes),
                "pageInfo": {"hasNextPage": next_cursor is not None, "endCursor": next_cursor}}}}}

    def op_Reviews(self, variables: dict) -> dict:
        return self.followup("reviews", variables)

    def op_Comments(self, variables: dict) -> dict:
        return self.followup("comments", variables)

    def op_Timeline(self, variables: dict) -> dict:
        return self.followup("timelineItems", variables)


class Harness(unittest.TestCase):
    now = NOW

    def setUp(self) -> None:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.data_dir, self.state_dir = self.root / "data", self.root / "state"
        self.github = FakeGitHub([])
        self.clock = self.now
        for patch in (mock.patch.object(pr_review, "run_json", side_effect=lambda argv: self.github(argv)),
                      mock.patch.object(pfm, "utcnow", side_effect=lambda: self.clock)):
            patch.start()
            self.addCleanup(patch.stop)

    def log(self, *events: dict) -> None:
        for e in events:
            pr_review.append_event(self.state_dir, e)

    def live_args(self, *tail: str) -> list[str]:
        return [*tail, "--data-dir", str(self.data_dir), "--state-dir", str(self.state_dir), "--repo", REPO,
                "--acting-login", ACTING]

    def run_cli(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = pfm.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def append_week(self, week: str, *tail: str) -> tuple[int, str, str]:
        return self.run_cli(*self.live_args("append", "--week", week, *tail))

    def history(self) -> list[dict]:
        return pfm.read_jsonl(self.data_dir / "history.jsonl")

    def snapshots(self) -> list[str]:
        directory = self.data_dir / "snapshots"
        return sorted(p.name for p in directory.iterdir()) if directory.exists() else []

    def week_record(self, prs: list[dict], events: list[dict], week: str = "2026-W44") -> dict:
        """Run a live append of one week over fixtures; return its metrics record."""
        self.github = FakeGitHub(prs)
        self.log(*events, event(prs[0]["number"], "observation", "2026-10-20T00:00:00Z"))
        code, _, err = self.append_week(week)
        self.assertEqual(code, 0, err)
        return self.history()[-1]


# ─── T1: rounds_v2 ───────────────────────────────────────────────────────────


class RoundsV2Tests(unittest.TestCase):
    def rounds(self, reviews: list[dict], timeline: list[dict] | None = None) -> dict:
        return pfm.effective_rounds(pr_node(1, reviews=connection(reviews), timelineItems=connection(timeline or [])), ACTING)

    def test_two_changes_requested_on_one_oid_are_one_round(self) -> None:
        out = self.rounds([review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:00:00Z", 1),
                           review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:05:00Z", 2)])
        self.assertEqual((len(out["heads"]), out["supplements"]), (1, 1))

    def test_dismissed_approval_is_not_a_round(self) -> None:
        out = self.rounds([review(ACTING, "DISMISSED", H[1], "2026-10-28T11:00:00Z", 7)], [dismissal(7, "APPROVED")])
        self.assertEqual((len(out["heads"]), out["approvals_excluded"]), (0, 1))

    def test_dismissed_change_request_is_a_round(self) -> None:
        out = self.rounds([review(ACTING, "DISMISSED", H[1], "2026-10-28T11:00:00Z", 7)], [dismissal(7, "CHANGES_REQUESTED")])
        self.assertEqual((len(out["heads"]), out["approvals_excluded"], out["state_unknown"]), (1, 0, 0))

    def test_unmatched_dismissed_review_counts_and_is_flagged(self) -> None:
        out = self.rounds([review(ACTING, "DISMISSED", H[1], "2026-10-28T11:00:00Z", 7)], [dismissal(8, "APPROVED")])
        self.assertEqual((len(out["heads"]), out["state_unknown"]), (1, 1))

    def test_non_maintainer_review_is_ignored_and_null_commit_is_counted(self) -> None:
        out = self.rounds([review("coderabbitai", "CHANGES_REQUESTED", H[2], "2026-10-28T11:00:00Z"),
                           review(ACTING, "CHANGES_REQUESTED", None, "2026-10-28T12:00:00Z")])
        self.assertEqual((len(out["heads"]), out["null_commit"]), (0, 1))

    def test_v2_never_exceeds_v1(self) -> None:
        policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
        reviews = [review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:00:00Z", 1),
                   review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:05:00Z", 2),
                   review(ACTING, "DISMISSED", H[2], "2026-10-28T12:00:00Z", 3),
                   review(ACTING, "CHANGES_REQUESTED", H[3], "2026-10-28T13:00:00Z", 4),
                   review(ACTING, "APPROVED", H[3], "2026-10-28T14:00:00Z", 5)]
        node = pr_node(1, reviews=connection(reviews), timelineItems=connection([dismissal(3, "APPROVED")]))
        f = pfm.analyze_pr(node, NOW, ACTING, policy)
        self.assertEqual((f["rounds_mw"], f["rounds_v2"]), (4, 2))


# ─── T2: pipeline levels ─────────────────────────────────────────────────────


class PipelineLevelTests(unittest.TestCase):
    policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)

    def level(self, body: str) -> str:
        return pfm.analyze_pr(pr_node(1, body=body), NOW, ACTING, self.policy)["level"]

    def template(self) -> str:
        return TEMPLATE.read_text(encoding="utf-8")

    def filled(self) -> str:
        return (self.template().replace("<40-hex-sha> base=<40-hex-sha>", f"{H[1]} base={H[2]}")
                .replace("PASS head=<40-hex-sha>", f"PASS head={H[1]}"))

    def test_unfilled_template_is_template_only(self) -> None:
        self.assertEqual(self.level(self.template()), "template_only")

    def test_template_with_real_shas_is_full(self) -> None:
        self.assertEqual(self.level(self.filled()), "full")

    def test_method_and_pass_lines_without_headings_are_none(self) -> None:
        body = f"Method: /engine-implementer\nGate A PASS head={H[1]} base={H[2]}\nFinal review-impl PASS head={H[1]}\n"
        self.assertEqual(self.level(body), "none")

    def test_real_gate_a_with_final_review_not_run_is_template_only(self) -> None:
        body = self.filled().replace(f"Final review-impl PASS head={H[1]}", "Final review-impl: Not run.")
        self.assertEqual(self.level(body), "template_only")

    def test_pass_line_outside_its_section_is_not_full(self) -> None:
        gate = f"Gate A PASS head={H[1]} base={H[2]}"
        body = self.filled().replace(gate, "").replace("## Summary\n", f"## Summary\n\n{gate}\n")
        self.assertEqual(self.level(body), "template_only")

    def test_module_has_no_pass_line_parser(self) -> None:
        self.assertIsNone(re.search(r"PASS\s+head=", SCRIPT.read_text(encoding="utf-8")))


# ─── T3: registry ────────────────────────────────────────────────────────────


class RegistryTests(Harness):
    def test_emitted_paths_match_registry_and_digest_follows_versions(self) -> None:
        record = self.week_record([pr_node(1, reviews=connection([review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:00:00Z")]))], [])
        leaves = pfm.leaf_paths({k: v for k, v in record.items() if k not in pfm.BOOKKEEPING_KEYS})
        for leaf in leaves:
            self.assertIsNotNone(pfm.registry_prefix(leaf), f"{leaf} is emitted but not registered")
        for path in pfm.REGISTRY:
            self.assertTrue(any(leaf == path or leaf.startswith(path + ".") for leaf in leaves), f"{path} is never emitted")
        self.assertEqual(record["definitions"], pfm.definitions())
        bumped = dict(pfm.REGISTRY, counts=(2, "GH", "changed"))
        with mock.patch.dict(pfm.REGISTRY, bumped):
            self.assertNotEqual(pfm.definitions_digest(pfm.definitions()), record["definitions_digest"])
            code, _, _ = self.append_week("2026-W44", "--input", str(self.data_dir / record["snapshot"]["path"]))
        self.assertEqual((code, len(self.history())), (0, 2))


# ─── T4 / T4b: ledger ────────────────────────────────────────────────────────


def target(metric: str, role: str = "primary", direction: str = "decrease", expected=1.5, version: int = 1,
           age_class: str = "settled", baseline=None, **extra) -> dict:
    return {"metric": metric, "version": version, "age_class": age_class, "role": role, "direction": direction,
            "baseline": baseline, "expected": expected, **extra}


def change(change_id: str = "C1", targets: list | None = None, **fields) -> dict:
    base = {"id": change_id, "date": "2026-11-01", "phase": "1", "hypothesis": "A holds.", "review_after": "2026-12-06",
            "switch": "git revert", "targets": targets or [target("rounds_v2.per_pr.mean")]}
    return base | fields


def metrics_record(week: str, value, version: int = 1, fetched: str | None = None, guard=None) -> dict:
    since, until = pfm.week_window(week)
    return {"kind": "metrics", "week": week, "age_class": "settled", "window": {"since": str(since), "until": str(until)},
            "fetched_at": fetched or "2026-11-08T00:00:00Z", "computed_at": "2026-11-08T00:00:01Z",
            "definitions": {"rounds_v2.per_pr": version, "abandonment": 1},
            "rounds_v2": {"per_pr": {"mean": value}}, "abandonment": {"count": guard}}


class LedgerTests(Harness):
    def write(self, name: str, payload) -> Path:
        path = self.root / name
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    def ledger(self, *argv: str) -> tuple[int, str, str]:
        return self.run_cli("ledger", *argv, "--data-dir", str(self.data_dir))

    def ledger_bytes(self) -> bytes | None:
        path = self.data_dir / "changes.jsonl"
        return path.read_bytes() if path.exists() else None

    def seed_history(self, records: list[dict]) -> None:
        self.data_dir.mkdir(parents=True, exist_ok=True)
        (self.data_dir / "history.jsonl").write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")

    def test_invalid_changes_are_rejected_and_leave_the_file_untouched(self) -> None:
        bad = {
            "no primary": change(targets=[target("rounds_v2.per_pr.mean", role="secondary")]),
            "primary without direction": change(targets=[{k: v for k, v in target("rounds_v2.per_pr.mean").items() if k != "direction"}]),
            "primary without numeric expected": change(targets=[target("rounds_v2.per_pr.mean", expected=None)]),
            "unregistered primary": change(targets=[target("no.such.metric")]),
            "wrong primary version": change(targets=[target("rounds_v2.per_pr.mean", version=9)]),
            "review_after before date": change(review_after="2026-10-01"),
            "unknown key": change(extra="x"),
            "unregistered secondary without measurable_from": change(targets=[target("rounds_v2.per_pr.mean"), target("future.metric", role="secondary")]),
        }
        for label, payload in bad.items():
            with self.subTest(label):
                code, _, _ = self.ledger("add", str(self.write("bad.json", payload)))
                self.assertEqual(code, 2)
                self.assertIsNone(self.ledger_bytes())
        self.assertEqual(self.ledger("add", str(self.write("ok.json", change())))[0], 0)
        before = self.ledger_bytes()
        self.assertEqual(self.ledger("add", str(self.write("dup.json", change())))[0], 2)
        self.assertEqual(self.ledger_bytes(), before)
        future = change("C2", targets=[target("rounds_v2.per_pr.mean"), target("future.metric", role="secondary", version=None, measurable_from="phase 4")])
        self.assertEqual(self.ledger("add", str(self.write("future.json", future)))[0], 0)

    def test_baseline_landed_and_verdict_gates(self) -> None:
        self.seed_history([metrics_record("2026-W41", 2.0), metrics_record("2026-W42", 4.0), metrics_record("2026-W43", 6.0)])
        self.assertEqual(self.ledger("add", str(self.write("c.json", change())))[0], 0)
        landed = ("landed", "C1", "--commit", H[1], "--date", "2026-11-09")
        verdict = {"id": "C1", "week": "2026-W44", "verdict": "improved", "note": "n"}
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", verdict)))[0], 2, "verdict on an unlanded entry")
        before = self.ledger_bytes()
        self.assertEqual(self.ledger(*landed)[0], 2, "landed with a null primary baseline")
        self.assertEqual(self.ledger_bytes(), before)
        self.assertEqual(self.ledger("baseline", "C1")[0], 0)
        self.assertEqual(self.ledger("show")[0], 0)
        shown = json.loads(self.run_cli("ledger", "show", "--data-dir", str(self.data_dir))[1])
        self.assertEqual(shown[0]["targets"][0]["baseline"], 4.0)
        self.assertIn("no null baselines", self.ledger("baseline", "C1")[1])
        self.assertEqual(self.ledger(*landed, "--review-after", "2026-12-01")[0], 2, "review_after may not move earlier")
        self.assertEqual(self.ledger(*landed)[0], 0)
        self.assertEqual(self.ledger("verdict", str(self.write("v2.json", dict(verdict, verdict="great")))) [0], 2)
        self.assertEqual(self.ledger("verdict", str(self.write("v3.json", verdict)))[0], 0)

    def test_baseline_refuses_without_eligible_weeks(self) -> None:
        self.assertEqual(self.ledger("add", str(self.write("c.json", change())))[0], 0)
        before = self.ledger_bytes()
        self.assertEqual(self.ledger("baseline", "C1")[0], 2)
        self.assertEqual(self.ledger_bytes(), before)

    def test_baseline_fills_what_has_data_and_reports_the_rest(self) -> None:
        self.seed_history([metrics_record("2026-W41", 2.0), metrics_record("2026-W42", 4.0)])
        guard = target("sample.post_r1.late_blocker_count", role="guard", direction="hold", expected=None)
        self.assertEqual(self.ledger("add", str(self.write("c.json", change(targets=[target("rounds_v2.per_pr.mean"), guard]))))[0], 0)
        code, out, _ = self.ledger("baseline", "C1")
        self.assertEqual(code, 0)
        self.assertIn("no data yet for sample.post_r1.late_blocker_count", out)
        shown = json.loads(self.ledger("show")[1])
        self.assertEqual([t["baseline"] for t in shown[0]["targets"]], [3.0, None])
        self.assertEqual(self.ledger("baseline", "C1")[0], 2, "only the sample target is left, and it has no data")

    def test_refusals_never_create_or_remove_the_ledger(self) -> None:
        ledger = self.data_dir / "changes.jsonl"
        self.assertEqual(self.ledger("add", str(self.write("bad.json", change(extra="x"))))[0], 2)
        self.assertEqual(self.ledger("baseline", "C9")[0], 2)
        self.assertFalse(ledger.exists(), "a refusal against a missing ledger creates no file")
        self.assertFalse(pfm.locked_append(ledger, lambda existing: None))
        self.assertFalse(ledger.exists(), "a no-op creates no file")
        self.assertEqual(self.ledger("add", str(self.write("ok.json", change())))[0], 0)
        before = ledger.read_bytes()
        self.assertEqual(self.ledger("add", str(self.write("dup.json", change())))[0], 2)
        self.assertEqual(self.ledger("add", str(self.write("bad.json", change("C2", extra="x"))))[0], 2)
        self.assertEqual(ledger.read_bytes(), before, "a refusal never removes or alters an existing ledger")

    def test_unregistered_descendants_are_not_registered_metrics(self) -> None:
        for metric in ("sample.nonexistent", "sample.post_r1.nonexistent", "rounds_v2.per_pr.nonexistent", "counts.nonexistent"):
            with self.subTest(metric):
                self.assertFalse(pfm.metric_known(metric))
                code, _, _ = self.ledger("add", str(self.write("bad.json", change(targets=[target(metric)]))))
                self.assertEqual((code, self.ledger_bytes()), (2, None))
        for metric in ("sample.post_r1.late_share", "sample.post_r1.location_share.IN_DIFF", "rounds_v2.per_pr.mean",
                       "rounds_v2.by_size_bucket.>=2000.mean_rounds", "log.held.over_72h_share", "pipeline_level.full.n"):
            with self.subTest(metric):
                self.assertTrue(pfm.metric_known(metric))

    def landed_change(self, targets: list) -> None:
        self.assertEqual(self.ledger("add", str(self.write("c.json", change(date="2026-10-01", review_after="2026-11-01", targets=targets))))[0], 0)
        self.assertEqual(self.ledger("landed", "C1", "--commit", H[1], "--date", "2026-10-19")[0], 0)

    def test_verdict_evidence_is_computed_and_departures_need_a_note(self) -> None:
        self.seed_history([metrics_record("2026-W41", 2.0), metrics_record("2026-W42", 4.0),
                           metrics_record("2026-W43", 1.0), metrics_record("2026-W44", 1.2)])
        self.landed_change([target("rounds_v2.per_pr.mean", baseline=3.0), target("future.metric", role="secondary", version=None, measurable_from="phase 9")])
        shown = json.loads(self.ledger("show", "--due")[1])[0]
        self.assertEqual(shown["rule_verdict"], "improved")
        verdict = {"id": "C1", "week": "2026-W44", "verdict": "improved", "note": ""}
        supplied = dict(verdict, evidence=[{"metric": "anything", "version": 999}])
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", supplied)))[0], 2, "supplied evidence is an unknown key")
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", {k: v for k, v in verdict.items() if k != "note"})))[0], 2)
        before = self.ledger_bytes()
        departure = dict(verdict, verdict="no_change")
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", departure)))[0], 2, "departure without a note")
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", dict(departure, note="  ")))) [0], 2, "blank note")
        self.assertEqual(self.ledger_bytes(), before)
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", dict(departure, note="guard moved"))))[0], 0)
        self.assertEqual(self.ledger("verdict", str(self.write("v.json", verdict)))[0], 0)
        stored = [json.loads(line) for line in self.ledger_bytes().decode().splitlines()][-2:]
        for record in stored:
            self.assertEqual((record["kind"], record["rule_verdict"], record["evidence"]), ("verdict", "improved", shown["targets"]))
        self.assertEqual([t["metric"] for t in stored[0]["evidence"]], ["rounds_v2.per_pr.mean"])
        self.assertEqual(stored[0]["evidence"][0]["baseline_values"], [2.0, 4.0])

    def test_required_axes(self) -> None:
        cases = {"sample.post_r1.late_share": ("late",), "sample.post_r1.late_blocker_count": ("late", "severity"),
                 "sample.post_r1.location_share.IN_DIFF": ("location",), "sample.severity_unsure": ("severity",),
                 "sample.post_r1.latent_blocking": (), "sample.n_findings": (), "rounds_v2.per_pr.mean": ()}
        for metric, axes in cases.items():
            with self.subTest(metric):
                self.assertEqual(pfm.required_axes(metric), axes)
                self.assertTrue(pfm.metric_known(metric))

    def test_unreliable_axes_exclude_weeks_from_verdicts_and_baselines(self) -> None:
        def sample(week: str, value: float, unreliable: list[str]) -> dict:
            since, until = pfm.week_window(week)
            return {"kind": "sample", "week": week, "age_class": "settled", "rubric_version": 1,
                    "window": {"since": str(since), "until": str(until)}, "fetched_at": "2026-11-08T00:00:00Z",
                    "computed_at": "2026-11-08T00:00:01Z", "sample": {"post_r1": {"late_share": value}}, "unreliable_axes": unreliable}

        def weeks(post_unreliable: list[str], pre_unreliable: list[str] = ()) -> list[dict]:
            return [sample("2026-W41", 0.5, list(pre_unreliable)), sample("2026-W42", 0.6, list(pre_unreliable)),
                    sample("2026-W43", 0.1, post_unreliable), sample("2026-W44", 0.2, post_unreliable)]

        self.seed_history(weeks([]))
        self.landed_change([target("sample.post_r1.late_share", baseline=0.55)])
        verdicts = {}
        for label, unreliable in {"reliable": [], "unrelated axis": ["severity"], "late unreliable": ["late"]}.items():
            self.seed_history(weeks(unreliable))
            shown = json.loads(self.ledger("show", "--due")[1])[0]
            verdicts[label] = (shown["rule_verdict"], shown["targets"][0]["post_values"])
        self.assertEqual(verdicts, {"reliable": ("improved", [0.1, 0.2]), "unrelated axis": ("improved", [0.1, 0.2]),
                                    "late unreliable": ("inconclusive", [])})
        unfilled = change("C2", date="2026-10-01", targets=[target("sample.post_r1.late_share")])
        self.assertEqual(self.ledger("add", str(self.write("c2.json", unfilled)))[0], 0)
        before = self.ledger_bytes()
        self.seed_history(weeks(["late"], ["late"]))
        self.assertEqual(self.ledger("baseline", "C2")[0], 2, "every week is excluded, so there is nothing to fill from")
        self.assertEqual(self.ledger_bytes(), before)
        self.seed_history(weeks([]))
        self.assertEqual(self.ledger("baseline", "C2")[0], 0)
        self.assertEqual(json.loads(self.ledger_bytes().decode().splitlines()[-1])["targets"][0]["weeks"], 4)

    def test_version_switch_comparison(self) -> None:
        v1_weeks = {"2026-W37": 1.0, "2026-W38": 1.1, "2026-W39": 1.2, "2026-W40": 1.3}
        v2_weeks = {"2026-W39": 9.0, "2026-W40": 9.1, "2026-W41": 9.2, "2026-W42": 9.3}
        history = [metrics_record(w, v, 1) for w, v in v1_weeks.items()]
        history += [metrics_record(w, v, 2) for w, v in v2_weeks.items()]
        history += [metrics_record("2026-W41", 99.0, 2, fetched="2026-11-08T05:00:00Z")]
        v2 = target("rounds_v2.per_pr.mean", version=2)
        values, _ = pfm.weekly_values(history, v2)
        self.assertEqual([(v["week"], v["value"]) for v in values],
                         [("2026-W39", 9.0), ("2026-W40", 9.1), ("2026-W41", 9.2), ("2026-W42", 9.3)], "earliest fetch wins; no v1 values")
        self.assertEqual(pfm.split_weeks(values, "2026-10-02"), ([9.0], [9.2, 9.3]))
        entry = {"change": change(targets=[v2]), "landed": {"date": "2026-10-02"}}
        self.assertEqual(pfm.rule_verdict(entry, history), "inconclusive")
        v1_values, _ = pfm.weekly_values(history, target("rounds_v2.per_pr.mean", version=1))
        self.assertEqual([v["value"] for v in v1_values], [1.0, 1.1, 1.2, 1.3])
        with mock.patch.dict(pfm.REGISTRY, {"rounds_v2.per_pr": (2, "GH", "v2")}):
            self.assertEqual(self.ledger("add", str(self.write("c2.json", change(targets=[v2]))))[0], 0)

    def test_verdict_branches_and_boundaries(self) -> None:
        def verdict(post: list[float], direction: str = "decrease", guard_post: list[float] | None = None) -> str:
            base = {"2026-W37": 10.0, "2026-W38": 12.0, "2026-W39": 14.0}
            after = dict(zip(["2026-W41", "2026-W42"], post))
            history = [metrics_record(w, v, guard=1) for w, v in base.items()]
            history += [metrics_record(w, v, guard=(guard_post or [1, 1])[i]) for i, (w, v) in enumerate(after.items())]
            targets = [target("rounds_v2.per_pr.mean", direction=direction),
                       target("abandonment.count", role="guard", direction="hold", expected=None)]
            return pfm.rule_verdict({"change": change(targets=targets), "landed": {"date": "2026-10-02"}}, history)

        self.assertEqual(verdict([8, 9]), "improved")
        self.assertEqual(verdict([10, 10]), "no_change", "post median at the range minimum is inside the range")
        self.assertEqual(verdict([14, 14]), "no_change", "post median at the range maximum is inside the range")
        self.assertEqual(verdict([15, 16]), "regressed", "primary moved the wrong way")
        self.assertEqual(verdict([8, 9], guard_post=[3, 3]), "regressed", "a guard outside its range vetoes the improvement")
        self.assertEqual(verdict([15, 16], direction="increase"), "improved")
        self.assertEqual(verdict([8]), "inconclusive", "one post week")


# ─── T5 / T6 / T8 / T9 / T10: append ─────────────────────────────────────────


class AppendTests(Harness):
    def setUp(self) -> None:
        super().setUp()
        reviews = connection([review(ACTING, "CHANGES_REQUESTED", H[1], "2026-10-28T11:00:00Z", 1)])
        self.prs = [pr_node(1, reviews=reviews), pr_node(2, author="bob")]
        self.events = [event(1, "held", "2026-10-28T12:00:00Z", outcome="held"), event(2, "observation", "2026-10-28T12:30:00Z")]

    def prepare(self, **fake) -> None:
        self.github = FakeGitHub(self.prs, **fake)
        self.log(*self.events)

    def assert_refused_without_writes(self, code: int) -> None:
        self.assertEqual(code, 2)
        self.assertEqual((self.snapshots(), self.history()), ([], []))

    def test_append_is_idempotent_and_settles_later(self) -> None:
        self.prepare()
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        calls = len(self.github.argv_log)
        code, out, _ = self.append_week("2026-W44")
        self.assertEqual((code, len(self.github.argv_log), len(self.history())), (0, calls, 1))
        self.assertIn("already recorded", out)
        self.clock = LATER
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        self.assertEqual([r["age_class"] for r in self.history()], ["early", "settled"])
        self.assertGreater(self.history()[1]["age_days"], 28)

    def test_week_that_has_not_ended_is_refused(self) -> None:
        self.prepare()
        self.clock = datetime(2026, 11, 8, 12, 0, tzinfo=UTC)
        self.assert_refused_without_writes(self.append_week("2026-W45")[0])

    def test_incomplete_fetches_are_refused(self) -> None:
        reviews_next = connection([review(ACTING, "COMMENTED", H[1], "2026-10-28T11:00:00Z")], total=3, next_cursor="c1")
        cases = {
            "issueCount above nodes": dict(count_delta=1),
            "reviews follow-up fails": dict(prs=[pr_node(1, reviews=reviews_next)], fail_op="Reviews"),
            "reviews cursor does not advance": dict(prs=[pr_node(1, reviews=reviews_next)], followups={(1, "reviews"): {"c1": ([], "c1")}}),
            "reviews total above nodes": dict(prs=[pr_node(1, reviews=connection([], total=4))]),
            "first call fails": dict(fail_op="Head"),
        }
        for label, fake in cases.items():
            with self.subTest(label):
                self.prs, self.events = fake.pop("prs", self.prs), [event(1, "held", "2026-10-28T12:00:00Z", outcome="held")]
                self.prepare(**fake)
                self.assert_refused_without_writes(self.append_week("2026-W44")[0])
                (self.state_dir / "review-events.jsonl").unlink()
        with self.subTest("null node"):
            self.prepare()
            self.github.op_Search = lambda v: {"search": {"issueCount": 2, "nodes": [None, pr_node(1)], "pageInfo": {"hasNextPage": False, "endCursor": None}}}
            self.assert_refused_without_writes(self.append_week("2026-W44")[0])

    def test_missing_or_empty_log_is_refused(self) -> None:
        self.github = FakeGitHub(self.prs)
        self.assert_refused_without_writes(self.append_week("2026-W44")[0])
        self.state_dir.mkdir()
        (self.state_dir / "review-events.jsonl").write_text("")
        self.assert_refused_without_writes(self.append_week("2026-W44")[0])

    def test_follow_up_pages_are_merged_and_filtered_timeline_total_is_ignored(self) -> None:
        first = review("alice", "COMMENTED", H[1], "2026-10-28T11:00:00Z", 1)
        second = review(ACTING, "CHANGES_REQUESTED", H[2], "2026-10-28T12:00:00Z", 2)
        paged = pr_node(1, reviews=connection([first], total=2, next_cursor="c1"),
                        timelineItems=dict(connection([], total=175), nodes=[commit(H[1], "2026-10-28T10:30:00Z")]))
        self.prs = [paged, pr_node(2, author="bob")]
        self.prepare(followups={(1, "reviews"): {"c1": ([second], None)}})
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        record = self.history()[0]
        self.assertEqual((record["truncation_repaired_prs"], record["rounds_v2"]["per_pr"]["max"]), ([1], 1))

    def test_no_github_or_log_writes(self) -> None:
        self.prepare()
        log = self.state_dir / "review-events.jsonl"
        before = (log.read_bytes(), log.stat().st_mtime_ns)
        with mock.patch.object(pr_review, "append_event", side_effect=AssertionError("log write")):
            self.assertEqual(self.append_week("2026-W44")[0], 0)
            path = self.root / "c.json"
            path.write_text(json.dumps(change()), encoding="utf-8")
            self.assertEqual(self.run_cli("ledger", "add", str(path), "--data-dir", str(self.data_dir))[0], 0)
        self.assertEqual((log.read_bytes(), log.stat().st_mtime_ns), before)
        self.assertTrue(self.github.argv_log)
        for argv, text in zip(self.github.argv_log, self.github.queries()):
            self.assertEqual(argv[:3], ["gh", "api", "graphql"])
            self.assertNotIn("mutation", text)

    def test_replay_reads_only_the_snapshot(self) -> None:
        self.prepare()
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        r1 = self.history()[0]
        snapshot_path = self.data_dir / r1["snapshot"]["path"]
        original = snapshot_path.read_bytes()
        self.log(event(1, "held", "2026-10-28T13:00:00Z", H[2], outcome="held"))  # backdated, inside the cohort week
        calls = len(self.github.argv_log)
        code, out, _ = self.append_week("2026-W44", "--input", str(snapshot_path))
        self.assertEqual((code, len(self.history())), (0, 1))
        self.assertIn("already recorded", out)
        with mock.patch.dict(pfm.REGISTRY, {"counts": (2, "GH", "changed")}):
            self.assertEqual(self.append_week("2026-W44", "--input", str(snapshot_path))[0], 0)
            r2 = self.history()[1]
        for key in set(r1) - {"computed_at", "definitions", "definitions_digest"}:
            self.assertEqual(r1[key], r2[key], key)
        self.assertEqual((r2["snapshot"], snapshot_path.read_bytes(), len(self.github.argv_log)), (r1["snapshot"], original, calls))
        self.assertEqual(r2["log"]["coverage"]["events_used"], r1["log"]["coverage"]["events_used"])
        with mock.patch.dict(pfm.REGISTRY, {"counts": (3, "GH", "changed again")}):
            self.assertEqual(self.append_week("2026-W44")[0], 2, "a live append whose snapshot name collides")
        self.assertEqual(snapshot_path.read_bytes(), original)

    def test_snapshot_keeps_complete_events_and_replay_can_read_any_field(self) -> None:
        self.events.append(event(1, "review", "2026-10-28T13:00:00Z", summary="promote me", signals=["s1"], tracker={"verdict": "x", "extra": 1}))
        self.prepare()
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        r1 = self.history()[0]
        snapshot_path = self.data_dir / r1["snapshot"]["path"]
        stored = pfm.read_snapshot(snapshot_path)["log"]["events"]
        self.assertEqual(stored, [e for e in pr_review.all_events(self.state_dir) if int(e["pr"]) in (1, 2)])
        self.assertEqual(stored[-1]["summary"], "promote me")
        self.assertEqual((r1["log"]["held"]["spells"], r1["log"]["warned"]["spells"]), (1, 0))
        original_state = pfm.event_state
        reads_summary = lambda e: "warned" if "promote" in (e.get("summary") or "") else original_state(e)
        with mock.patch.object(pfm, "event_state", side_effect=reads_summary), \
                mock.patch.dict(pfm.REGISTRY, {"log.warned": (2, "LOG", "summary-aware")}):
            self.assertEqual(self.append_week("2026-W44", "--input", str(snapshot_path))[0], 0)
        r2 = self.history()[1]
        self.assertEqual((r2["log"]["held"]["hours"]["total"], r2["log"]["warned"]["spells"]), (1.0, 1),
                         "the replay read the summary that a field whitelist would have dropped")

    def test_last_week_and_settled_due_selection(self) -> None:
        def due(**flags) -> list[str]:
            defaults = {"data_dir": self.data_dir, "week": None, "last_week": False, "settled_due": False}
            return pfm.weeks_for(argparse.Namespace(**(defaults | flags)))

        for today, week in {(2027, 1, 5): "2026-W53", (2026, 11, 9): "2026-W45", (2026, 1, 5): "2026-W01"}.items():
            self.clock = datetime(*today, 12, 0, tzinfo=UTC)
            self.assertEqual(due(last_week=True), [week])
        digest = pfm.definitions_digest(pfm.definitions())
        row = lambda week, age, d=digest: {"kind": "metrics", "week": week, "age_class": age, "definitions_digest": d}
        self.data_dir.mkdir(parents=True)
        history = [row("2026-W37", "early"), row("2026-W38", "settled"), row("2026-W39", "settled", "old-digest")]
        (self.data_dir / "history.jsonl").write_text("".join(json.dumps(r) + "\n" for r in history), encoding="utf-8")
        # W37 ends 09-13, so it settles at 28 days after 09-14T00:00, i.e. 10-12T00:00
        boundaries = {datetime(2026, 10, 11, 23, 59, 59, tzinfo=UTC): [], datetime(2026, 10, 12, tzinfo=UTC): ["2026-W37"],
                      datetime(2026, 10, 25, 23, 59, 59, tzinfo=UTC): ["2026-W37"],
                      datetime(2026, 10, 26, tzinfo=UTC): ["2026-W37", "2026-W39"]}
        for clock, weeks in boundaries.items():
            self.clock = clock
            self.assertEqual(due(settled_due=True), weeks, clock.isoformat())
        self.clock = NOW
        self.assertEqual(due(settled_due=True), ["2026-W37", "2026-W39", "2026-W40", "2026-W41"], "W38 is recorded; W42 is not 28 days old")

    def test_last_week_append_is_idempotent(self) -> None:
        self.prepare()
        self.assertEqual(self.run_cli(*self.live_args("append", "--last-week"))[0], 0)
        calls = len(self.github.argv_log)
        code, out, _ = self.run_cli(*self.live_args("append", "--last-week"))
        self.assertEqual((code, [r["week"] for r in self.history()], len(self.github.argv_log)), (0, ["2026-W45"], calls))
        self.assertIn("2026-W45 early: already recorded", out)

    def test_input_refusals(self) -> None:
        self.prepare()
        self.assertEqual(self.append_week("2026-W44")[0], 0)
        record = self.history()[0]
        source = self.data_dir / record["snapshot"]["path"]
        envelope = json.loads(gzip.decompress(source.read_bytes()))

        def variant(name: str, **changes) -> Path:
            path = self.root / name
            path.write_bytes(gzip.compress(json.dumps(envelope | changes).encode()))
            return path

        cases = {
            "other week": ("2026-W45", source, REPO),
            "window not a full week": ("2026-W44", variant("a.json.gz", window={"since": "2026-10-26", "until": "2026-10-31"}), REPO),
            "incomplete": ("2026-W44", variant("b.json.gz", complete=False), REPO),
            "no completeness flag": ("2026-W44", variant("c.json.gz", complete=None), REPO),
            "repo mismatch": ("2026-W44", source, "other/repo"),
            "wrong kind": ("2026-W44", variant("d.json.gz", kind="x"), REPO),
        }
        before = (self.history(), self.snapshots())
        for label, (week, path, repo) in cases.items():
            with self.subTest(label):
                code, _, _ = self.run_cli("append", "--week", week, "--input", str(path), "--data-dir", str(self.data_dir),
                                          "--repo", repo, "--state-dir", str(self.state_dir), "--acting-login", ACTING)
                self.assertEqual((code, (self.history(), self.snapshots())), (2, before))
        source.write_bytes(gzip.compress(json.dumps(envelope | {"fetched_at": "2026-11-09T11:00:00Z"}).encode()))
        self.assertEqual(self.append_week("2026-W44", "--input", str(source))[0], 2, "hash differs from the recorded one")
        self.assertEqual(self.history(), before[0])


# ─── T7: log-derived fields ──────────────────────────────────────────────────


class LogFieldTests(Harness):
    def log_block(self, prs: list[dict], *events: dict) -> dict:
        return self.week_record(prs, list(events))["log"]

    def test_held_spells_censoring_and_silence(self) -> None:
        prs = [pr_node(n) for n in range(1, 7)]
        e = [
            # 1: lone held, then silence until fetched_at (324 h, censored)
            event(1, "held", "2026-10-27T00:00:00Z", outcome="held"),
            # 2: held, an observation 1 h later, then a tail of silence
            event(2, "held", "2026-10-27T00:00:00Z", outcome="held"), event(2, "observation", "2026-10-27T01:00:00Z"),
            # 3: a 96 h gap between two observations (98 h spell)
            event(3, "held", "2026-10-27T00:00:00Z", outcome="held"), event(3, "observation", "2026-10-27T01:00:00Z"),
            event(3, "observation", "2026-10-31T01:00:00Z"), event(3, "changes_requested", "2026-10-31T02:00:00Z", outcome="changes_requested"),
            # 4: exactly 72 h, daily observations
            event(4, "held", "2026-10-27T00:00:00Z", outcome="held"), event(4, "observation", "2026-10-28T00:00:00Z"),
            event(4, "observation", "2026-10-29T00:00:00Z"), event(4, "changes_requested", "2026-10-30T00:00:00Z", outcome="changes_requested"),
            # 5: an unclassified material event neither ends nor starts a spell (12 h)
            event(5, "held", "2026-10-27T00:00:00Z", outcome="held"), event(5, "review", "2026-10-27T10:00:00Z", outcome="review"),
            event(5, "changes_requested", "2026-10-27T12:00:00Z", outcome="changes_requested"),
            # 6: 100 h spell kept chatty by daily observations
            event(6, "held", "2026-10-27T00:00:00Z", outcome="held"),
            *[event(6, "observation", f"2026-10-{d}T00:00:00Z") for d in (28, 29, 30, 31)],
            event(6, "changes_requested", "2026-10-31T04:00:00Z", outcome="changes_requested"),
        ]
        held = self.log_block(prs, *e)["held"]
        self.assertEqual((held["spells"], held["censored"], held["hours"]["total"]), (6, 2, 324 + 324 + 98 + 72 + 12 + 100))
        self.assertEqual((held["over_72h_share"], held["silence_over_72h_share"]), (0.667, 0.5))

    def test_warned_spells(self) -> None:
        prs = [pr_node(1), pr_node(2)]
        warned = self.log_block(
            prs, event(1, "requested_changes_warning", "2026-10-27T00:00:00Z"),
            event(1, "stale_changes_closed", "2026-10-28T00:00:00Z"), event(2, "requested_changes_warning", "2026-11-01T00:00:00Z"),
        )["warned"]
        self.assertEqual((warned["spells"], warned["censored"], warned["hours"]["total"]), (2, 1, 24 + 204))

    def test_untimed_events_are_dropped_and_counted(self) -> None:
        coverage = self.log_block([pr_node(1)], event(1, "held", "", outcome="held"), event(1, "held", "2026-10-28T12:00:00Z", outcome="held"))["coverage"]
        self.assertEqual((coverage["untimed_events_dropped"], coverage["cohort_prs"]), (1, 1))

    def test_queue_exits_return_via_merge_and_never(self) -> None:
        prs = [pr_node(1), pr_node(2, mergedAt="2026-10-29T10:00:00Z"), pr_node(3)]
        queue = self.log_block(
            prs,
            event(1, "approved_enqueued", "2026-10-27T00:00:00Z", outcome="approved_enqueued"), event(1, "held", "2026-10-27T05:00:00Z", outcome="held"),
            event(1, "approved_enqueued", "2026-10-27T08:00:00Z", outcome="approved_enqueued"),
            event(2, "approved_enqueued", "2026-10-28T00:00:00Z", outcome="approved_enqueued"), event(2, "held", "2026-10-28T10:00:00Z", outcome="held"),
            event(3, "approved_enqueued", "2026-10-30T00:00:00Z", outcome="approved_enqueued"), event(3, "held", "2026-10-30T06:00:00Z", outcome="held"),
        )["queue_exit"]
        self.assertEqual((queue["exits"], queue["returned"], queue["return_hours"]["total"]), (3, 2, 3 + 24))
        self.assertEqual((queue["returned_over_72h"], queue["unreturned"], queue["unreturned_over_72h"]), (0, 1, 1))

    def test_assignee_gate(self) -> None:
        prs = [
            pr_node(1, author="alice", created="2026-10-26T00:00:00Z",
                    timelineItems=connection([assignment("AssignedEvent", "Alice", "2026-10-27T00:00:00Z")])),
            pr_node(2, author="bob", created="2026-10-26T00:00:00Z",
                    timelineItems=connection([assignment("AssignedEvent", "carol", "2026-10-27T00:00:00Z"),
                                              assignment("UnassignedEvent", "carol", "2026-10-27T12:00:00Z")])),
            pr_node(3, author="dave", created="2026-10-26T00:00:00Z",
                    timelineItems=connection([assignment("AssignedEvent", ACTING, "2026-10-27T00:00:00Z")])),
        ]
        gate = self.log_block(
            prs, *[event(1, "observation", f"2026-10-2{d}T01:00:00Z") for d in (7, 8, 9)], event(1, "held", "2026-10-27T05:00:00Z", outcome="held"),
            event(2, "observation", "2026-10-27T06:00:00Z"), event(2, "observation", "2026-10-27T13:00:00Z"),
            event(3, "observation", "2026-10-27T06:00:00Z"),
        )["assignee_gate"]
        self.assertEqual(gate, {"prs": 2, "author_assigned_prs": 1, "skip_events": 4, "author_assigned_skip_events": 3,
                                "material_events_in_foreign": 1})

    def test_head_pickup_latency(self) -> None:
        timeline = [commit(H[1], "2026-10-27T00:00:00Z"), force_push(H[2], "2026-10-28T00:00:00Z"), force_push(H[3], "2026-10-29T00:00:00Z")]
        pickup = self.log_block(
            [pr_node(1, created="2026-10-27T00:30:00Z", timelineItems=connection(timeline))],
            event(1, "observation", "2026-10-27T01:00:00Z", H[1]), event(1, "observation", "2026-10-28T02:00:00Z", H[2]),
            event(1, "observation", "2026-10-28T23:00:00Z", H[3]), event(1, "observation", "2026-10-30T00:00:00Z", H[4]),
        )["head_pickup"]
        self.assertEqual((pickup["heads"], pickup["latency_hours"]["n"], pickup["latency_hours"]["total"]), (2, 1, 2.0))
        self.assertEqual((pickup["negative_dropped"], pickup["without_arrival"], pickup["over_24h"]), (1, 1, 0))

    def test_head_pickup_counts_the_first_logged_head_when_the_opening_head_is_unlogged(self) -> None:
        timeline = [commit(H[1], "2026-10-27T00:00:00Z"), force_push(H[2], "2026-10-28T00:00:00Z")]
        pickup = self.log_block(
            [pr_node(1, created="2026-10-27T00:30:00Z", timelineItems=connection(timeline))],
            event(1, "observation", "2026-10-28T02:00:00Z", H[2]),
        )["head_pickup"]
        self.assertEqual((pickup["heads"], pickup["latency_hours"]["total"]), (1, 2.0))

    def test_head_pickup_ignores_commit_dates_when_choosing_the_opening_head(self) -> None:
        # 1: opening commit dated after the PR opened (clock skew) is still the opening head.
        # 2: a commit made before the PR opened but pushed later is still a pickup.
        prs = [
            pr_node(1, created="2026-10-27T10:00:00Z", timelineItems=connection([commit(H[1], "2026-10-27T10:05:00Z")])),
            pr_node(2, created="2026-10-27T10:00:00Z", timelineItems=connection(
                [commit(H[1], "2026-10-27T08:00:00Z"), commit(H[2], "2026-10-27T09:00:00Z")])),
        ]
        pickup = self.log_block(
            prs, event(1, "observation", "2026-10-27T11:00:00Z", H[1]),
            event(2, "observation", "2026-10-27T10:30:00Z", H[1]), event(2, "observation", "2026-10-28T12:00:00Z", H[2]),
        )["head_pickup"]
        self.assertEqual((pickup["heads"], pickup["latency_hours"]["total"]), (1, 27.0))

    def test_request_changes_ends_a_hold_as_blocked(self) -> None:
        held = self.log_block(
            [pr_node(1)], event(1, "held", "2026-10-27T00:00:00Z", outcome="held"),
            event(1, "request_changes", "2026-10-27T03:00:00Z"),
        )["held"]
        self.assertEqual((held["spells"], held["censored"], held["hours"]["total"]), (1, 0, 3.0))

    def test_spells_end_at_closure_and_later_transitions_are_ignored(self) -> None:
        prs = [pr_node(1, state="CLOSED", closedAt="2026-10-27T01:00:00Z"),
               pr_node(2, state="CLOSED", closedAt="2026-10-27T01:00:00Z")]
        held = self.log_block(
            prs, event(1, "held", "2026-10-27T00:00:00Z", outcome="held"),
            event(1, "pruned_merged", "2026-11-01T01:00:00Z", outcome="pruned_merged"),  # five days after the close
            event(2, "held", "2026-10-27T00:00:00Z", outcome="held"),
            event(2, "changes_requested", "2026-10-30T00:00:00Z", outcome="changes_requested"),
        )["held"]
        self.assertEqual((held["spells"], held["censored"], held["hours"]["total"], held["over_72h_share"]), (2, 0, 2.0, 0.0))

    def test_spell_ends_when_a_state_neutral_event_arrives_on_a_new_head(self) -> None:
        held = self.log_block(
            [pr_node(1)], event(1, "held", "2026-10-27T00:00:00Z", H[1], outcome="held"),
            event(1, "observation", "2026-10-27T00:30:00Z", H[1]),  # same head: the hold continues
            event(1, "observation", "2026-10-27T01:00:00Z", H[2]),
        )["held"]
        self.assertEqual((held["spells"], held["censored"], held["hours"]["total"]), (1, 0, 1.0))

    def test_queue_exit_survives_a_state_neutral_event_on_a_new_head(self) -> None:
        queue = self.log_block(
            [pr_node(1)], event(1, "approved_enqueued", "2026-10-27T00:00:00Z", H[1], outcome="approved_enqueued"),
            event(1, "observation", "2026-10-27T01:00:00Z", H[2]),
            event(1, "held", "2026-10-27T02:00:00Z", H[2], outcome="held"),
            event(1, "approved_enqueued", "2026-10-27T05:00:00Z", H[2], outcome="approved_enqueued"),
        )["queue_exit"]
        self.assertEqual((queue["exits"], queue["returned"], queue["return_hours"]["total"]), (1, 1, 4.0))

    def test_assignee_intervals_are_clipped_to_the_open_interval(self) -> None:
        assigned = connection([assignment("AssignedEvent", "carol", "2026-10-27T00:00:00Z")])
        gate = self.log_block(
            [pr_node(1, author="bob", created="2026-10-26T00:00:00Z", state="CLOSED", closedAt="2026-10-27T12:00:00Z", timelineItems=assigned)],
            event(1, "observation", "2026-10-27T06:00:00Z"), event(1, "observation", "2026-10-28T06:00:00Z"),
            event(1, "held", "2026-10-29T06:00:00Z", outcome="held"),
        )["assignee_gate"]
        self.assertEqual((gate["prs"], gate["skip_events"], gate["material_events_in_foreign"]), (1, 1, 0))

    def test_post_close_observation_is_not_an_assignee_skip_after_explicit_unassignment(self) -> None:
        explicit = connection([assignment("AssignedEvent", "carol", "2026-10-27T00:00:00Z"),
                               assignment("UnassignedEvent", "carol", "2026-10-30T00:00:00Z")])
        gate = self.log_block(
            [pr_node(2, author="bob", created="2026-10-26T00:00:00Z", state="CLOSED", closedAt="2026-10-27T12:00:00Z", timelineItems=explicit)],
            event(2, "observation", "2026-10-28T06:00:00Z"),
        )["assignee_gate"]
        self.assertEqual((gate["prs"], gate["skip_events"]), (1, 0), "a post-close observation is not an assignee skip")

    def test_fixup_pushes_are_counted(self) -> None:
        fixups = self.log_block([pr_node(1)], event(1, "fixup_push", "2026-10-28T12:00:00Z"), event(1, "fixup_push", "2026-10-28T13:00:00Z"))
        self.assertEqual(fixups["maintainer_fixup_pushes"], 2)


# ─── T11: sample ─────────────────────────────────────────────────────────────


def rating(pr: int, round_: int, **fields) -> dict:
    row = {"pr": str(pr), "round": str(round_), "fid": f"R{round_}-F1", "location": "IN_DIFF", "origin": "ORIGINAL",
           "late": "-" if round_ == 1 else "NOT_LATE", "severity": "SHOULD", "latent": "no", "bot_treatment": "blocking",
           "outcome": "fixed", "late_evidence": "" if round_ == 1 else "git diff a b -- f: changed",
           "anchor": "f.rs:1", "finding": "a finding"}
    return row | {k: str(v) for k, v in fields.items()}


def write_tsv(path: Path, rows: list[dict], columns: tuple[str, ...]) -> Path:
    path.write_text("\t".join(columns) + "\n" + "".join("\t".join(r[c] for c in columns) + "\n" for r in rows), encoding="utf-8")
    return path


class SampleTests(Harness):
    week = "2026-W41"

    def prepare_week(self, n_prs: int = 12) -> None:
        prs = []
        for n in range(1, n_prs + 1):
            reviews = [review(ACTING, "CHANGES_REQUESTED", H[k], f"2026-10-06T{10 + k:02d}:00:00Z", n * 10 + k) for k in range(1, n % 4 + 2)]
            prs.append(pr_node(n, created="2026-10-06T09:00:00Z", reviews=connection(reviews)))
        self.github = FakeGitHub(prs)
        self.log(event(1, "observation", "2026-10-06T10:00:00Z"))
        self.assertEqual(self.append_week(self.week)[0], 0)
        self.assertEqual(self.history()[0]["age_class"], "settled")

    def printed(self) -> list[tuple[int, int]]:
        selected, _ = pfm.sample_for_week(self.data_dir, self.week)
        return [(pr["number"], h["round"]) for pr in selected for h in pr["v2_heads"][:pfm.SAMPLE_ROUND_CAP]]

    def cli(self, *argv: str) -> tuple[int, str, str]:
        return self.run_cli(*argv, "--data-dir", str(self.data_dir))

    def test_selection_is_deterministic_per_label_and_takes_all_when_few(self) -> None:
        candidates = [{"number": n, "rounds_v2": 1 + n % 5} for n in range(1, 41)]
        first = [c["number"] for c in pfm.select_sample("2026-W41", candidates)]
        self.assertEqual(first, [c["number"] for c in pfm.select_sample("2026-W41", candidates)])
        self.assertEqual(len(first), 8)
        self.assertNotEqual(first, [c["number"] for c in pfm.select_sample("2026-W42", candidates)])
        self.assertEqual(len(pfm.select_sample("2026-W41", candidates[:5] + [{"number": 99, "rounds_v2": 0}])), 5)

    def test_sample_and_drift_are_identical_across_processes(self) -> None:
        self.prepare_week()
        rows = [rating(pr, r) for pr, r in self.printed()]
        ratings = write_tsv(self.root / "r.tsv", rows, pfm.RATING_COLUMNS)
        outputs = []
        for seed in ("1", "2"):
            env = dict(os.environ, PYTHONHASHSEED=seed)
            run = lambda *argv: subprocess.run([sys.executable, str(SCRIPT), *argv, "--data-dir", str(self.data_dir)],
                                               capture_output=True, text=True, check=True, env=env).stdout
            outputs.append((run("sample", "--week", self.week), run("sample-drift", "--week", self.week, "--ratings", str(ratings))))
        self.assertEqual(outputs[0], outputs[1])
        self.assertIn("round 1", outputs[0][0])

    def test_post_r1_arithmetic_with_unknown_values_and_zero_denominators(self) -> None:
        rows = [
            rating(1, 1, origin="FIX_INDUCED"),
            rating(1, 2, late="LATE", severity="BLOCKER", origin="FIX_INDUCED", location="ADJACENT", bot_treatment="blocking"),
            rating(1, 2, late="NOT_LATE", severity="NIT", origin="ORIGINAL", location="IN_DIFF", bot_treatment="blocking") | {"fid": "R2-F2"},
            rating(2, 3, late="UNSURE", severity="UNSURE", origin="UNSURE", location="UNSURE", latent="yes", bot_treatment="blocking") | {"late_evidence": ""},
            rating(2, 3, late="LATE", severity="SHOULD", origin="FIX_INDUCED", location="IN_DIFF", bot_treatment="non-blocking") | {"fid": "R3-F2"},
        ]
        stats = pfm.post_r1_stats(rows)
        self.assertEqual((stats["late_share"], stats["fix_induced_share"], stats["blocker_share"]), (0.667, 0.667, 0.333))
        self.assertEqual(stats["location_share"], {"ADJACENT": 0.333, "ELSEWHERE": 0.0, "IN_DIFF": 0.667, "PROCESS": 0.0})
        self.assertEqual((stats["late_blocker_count"], stats["fix_induced_blocker_count"], stats["nit_blocking"], stats["latent_blocking"]), (1, 1, 1, 1))
        empty = pfm.post_r1_stats([rating(1, 1)])
        self.assertEqual((empty["late_share"], empty["blocker_share"], empty["location_share"]["IN_DIFF"]), (None, None, None))

    def test_sample_drift_prints_identification_only(self) -> None:
        self.prepare_week()
        rows = [rating(pr, r, severity="BLOCKER", origin="MAIN_DRIFT") for pr, r in self.printed()]
        ratings = write_tsv(self.root / "r.tsv", rows, pfm.RATING_COLUMNS)
        code, out, _ = self.cli("sample-drift", "--week", self.week, "--ratings", str(ratings))
        lines = [line.split("\t") for line in out.splitlines()]
        self.assertEqual((code, lines[0]), (0, ["pr", "round", "fid", "anchor", "finding"]))
        self.assertTrue(all(len(line) == 5 and line[3:] == ["f.rs:1", "a finding"] for line in lines[1:]))
        self.assertNotIn("BLOCKER", out)
        self.assertNotIn("MAIN_DRIFT", out)

    def test_ratings_without_identity_columns_or_finding_are_refused(self) -> None:
        self.prepare_week()
        rows = [rating(pr, r) for pr, r in self.printed()]
        drift = write_tsv(self.root / "d.tsv", rows[:1], pfm.DRIFT_COLUMNS)
        args = lambda ratings: ("sample-record", "--week", self.week, "--ratings", str(ratings), "--drift", str(drift))
        no_columns = write_tsv(self.root / "no-cols.tsv", rows, pfm.RATING_COLUMNS[:-2])
        empty = write_tsv(self.root / "empty.tsv", [dict(rows[0], finding="")] + rows[1:], pfm.RATING_COLUMNS)
        for ratings in (no_columns, empty):
            self.assertEqual(self.cli(*args(ratings))[0], 2)
        self.assertEqual(len(self.history()), 1)

    def test_ratings_validation_rejects(self) -> None:
        self.prepare_week()
        printed = self.printed()
        first, later = next(p for p in printed if p[1] == 1), next(p for p in printed if p[1] >= 2)
        bad = {
            "duplicate key": [rating(*first), rating(*first)],
            "round not printed": [rating(999, 1)],
            "latent blocker": [rating(*later, latent="yes", severity="BLOCKER")],
            "late without evidence": [rating(*later, late="LATE", late_evidence="")],
            "round 1 not '-'": [rating(*first, late="NOT_LATE", late_evidence="x")],
            "outside the vocabulary": [rating(*later, origin="MAYBE")],
        }
        for label, rows in bad.items():
            with self.subTest(label):
                ratings = write_tsv(self.root / "bad.tsv", rows, pfm.RATING_COLUMNS)
                drift = write_tsv(self.root / "bad-drift.tsv", rows[:1], pfm.DRIFT_COLUMNS)
                self.assertEqual(self.cli("sample-record", "--week", self.week, "--ratings", str(ratings), "--drift", str(drift))[0], 2)
        self.assertEqual(len(self.history()), 1)

    def test_drift_rows_must_match_the_selection_and_record_is_idempotent(self) -> None:
        self.prepare_week()
        rows = [rating(pr, r) | {"fid": f"R{r}-F{i}"} for pr, r in self.printed() for i in (1, 2)]
        self.assertGreater(len(rows), pfm.DRIFT_SIZE)
        ratings = write_tsv(self.root / "r.tsv", rows, pfm.RATING_COLUMNS)
        keys = pfm.drift_keys(self.week, rows)
        by_key = {pfm.row_key(r): r for r in rows}
        drift_rows = [by_key[k] for k in keys]
        args = lambda drift: ("sample-record", "--week", self.week, "--ratings", str(ratings), "--drift", str(drift))
        for label, subset in {"missing row": drift_rows[1:], "extra row": drift_rows + [r for r in rows if r not in drift_rows][:1]}.items():
            with self.subTest(label):
                self.assertEqual(self.cli(*args(write_tsv(self.root / "d.tsv", subset, pfm.DRIFT_COLUMNS)))[0], 2)
        good = write_tsv(self.root / "good.tsv", drift_rows, pfm.DRIFT_COLUMNS)
        self.assertEqual(self.cli(*args(good))[0], 0)
        self.assertIn("already recorded", self.cli(*args(good))[1])
        record = self.history()[-1]
        self.assertEqual((record["kind"], record["unreliable_axes"], len(self.history())), ("sample", [], 2))
        values, _ = pfm.weekly_values(self.history(), {"metric": "sample.post_r1.late_share", "version": 1, "age_class": "settled"})
        self.assertEqual([v["week"] for v in values], [self.week])

    def test_drift_axis_values_are_validated_and_a_refusal_leaves_history_unchanged(self) -> None:
        self.prepare_week()
        rows = [rating(pr, r) | {"fid": f"R{r}-F{i}"} for pr, r in self.printed() for i in (1, 2)]
        ratings = write_tsv(self.root / "r.tsv", rows, pfm.RATING_COLUMNS)
        by_key = {pfm.row_key(r): r for r in rows}
        drift_rows = [by_key[k] for k in pfm.drift_keys(self.week, rows)]
        late = next(i for i, r in enumerate(drift_rows) if r["round"] != "1")
        first = next(i for i, r in enumerate(drift_rows) if r["round"] == "1")
        bad = {"severity outside the vocabulary": (0, {"severity": "BANANA"}), "late on a round-1 row": (first, {"late": "NOT_LATE"}),
               "'-' on a later round": (late, {"late": "-"}), "origin outside the vocabulary": (0, {"origin": "MAYBE"})}
        before = self.history()
        for label, (index, change_) in bad.items():
            with self.subTest(label):
                redo = [dict(r, **change_) if i == index else r for i, r in enumerate(drift_rows)]
                drift = write_tsv(self.root / "bad-drift.tsv", redo, pfm.DRIFT_COLUMNS)
                code, _, err = self.cli("sample-record", "--week", self.week, "--ratings", str(ratings), "--drift", str(drift))
                self.assertEqual((code, self.history()), (2, before))
                self.assertIn("drift", err)

    def test_axis_without_comparable_rows_is_unreliable(self) -> None:
        unsure = {a: "UNSURE" for a in pfm.DRIFT_THRESHOLDS}
        stats = pfm.drift_stats({(1, 2, "F"): {a: "X" for a in pfm.DRIFT_THRESHOLDS}}, {(1, 2, "F"): unsure})
        self.assertEqual([(a, s["m"], s["pass"]) for a, s in stats.items()], [(a, 0, False) for a in pfm.DRIFT_THRESHOLDS])

    def test_drift_threshold_boundary(self) -> None:
        def stats(agreements: int, total: int) -> dict:
            primary = {(1, i, "F"): {a: "X" for a in pfm.DRIFT_THRESHOLDS} for i in range(total)}
            redo = {k: {a: "X" if i < agreements else "Y" for a in pfm.DRIFT_THRESHOLDS} for i, k in enumerate(primary)}
            return pfm.drift_stats(primary, redo)["location"]

        self.assertEqual(stats(8, 10)["needed"], 8)  # floor(16 * 10 / 20)
        self.assertTrue(stats(8, 10)["pass"])
        self.assertFalse(stats(7, 10)["pass"])
        self.assertTrue(stats(16, 20)["pass"])
        self.assertFalse(stats(15, 20)["pass"])
        unsure = pfm.drift_stats({(1, 1, "F"): {a: "UNSURE" for a in pfm.DRIFT_THRESHOLDS}}, {(1, 1, "F"): {a: "X" for a in pfm.DRIFT_THRESHOLDS}})
        self.assertEqual(unsure["severity"]["m"], 0)


if __name__ == "__main__":
    unittest.main()
