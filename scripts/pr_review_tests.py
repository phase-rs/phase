#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from datetime import UTC
from pathlib import Path

import pr_review


class PrReviewTests(unittest.TestCase):
    def test_event_record_is_idempotent_and_compacts(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            event = {
                "event_type": "tracker_row",
                "timestamp": "2026-06-28T00:00:00Z",
                "pr": 4495,
                "author": "contributor",
                "head_sha": "abc123",
                "tracker": {"verdict": "HELD-stale-approval-superseded"},
            }

            self.assertTrue(pr_review.append_event(state_dir, event))
            self.assertFalse(pr_review.append_event(state_dir, event))

            args = type("Args", (), {"state_dir": state_dir})()
            pr_review.command_compact(args)

            summary = json.loads((state_dir / "review-summary.json").read_text())
            self.assertEqual(summary["prs"][0]["pr"], 4495)
            self.assertEqual(summary["prs"][0]["verdict"], "HELD-stale-approval-superseded")
            self.assertEqual(summary["contributors"][0]["login"], "contributor")

    def test_hard_stop_takes_precedence(self) -> None:
        policy = pr_review.Policy(
            {
                "hard_stops": {"patterns": [".claude/skills/**"]},
                "path_classes": {"frontend": {"patterns": ["client/**"]}},
            }
        )

        classification = pr_review.classify_files(
            [".claude/skills/pr-review-loop/SKILL.md", "client/src/App.tsx"],
            policy,
        )

        self.assertEqual(classification["surface"], "hard_stop")
        self.assertEqual(classification["gate"], "hard_stop")
        self.assertEqual(
            classification["hard_stop_paths"],
            [".claude/skills/pr-review-loop/SKILL.md"],
        )

    def test_stale_approval_recommends_dequeue_when_queued(self) -> None:
        packet = {
            "pr": {
                "number": 4495,
                "headRefOid": "new-head",
                "reviewDecision": "APPROVED",
                "isInMergeQueue": True,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "old-head",
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "dequeue_stale_for_handler")
        self.assertEqual(recommendation["reason"], "stale_approval")

    def test_frontend_policy_defers_only_when_no_harder_blocker(self) -> None:
        packet = {
            "pr": {
                "number": 4405,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "frontend"},
            "latest_maintainer_review_commit": None,
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "defer")
        self.assertEqual(recommendation["reason"], "frontend_policy")

    def test_current_head_hold_does_not_suppress_green_review(self) -> None:
        packet = {
            "pr": {
                "number": 4574,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "held",
                "outcome": "held",
                "head_sha": "head",
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "needs_review")

    def test_merged_pr_recommends_prune(self) -> None:
        packet = {
            "pr": {
                "number": 4495,
                "state": "MERGED",
                "headRefOid": "head",
                "reviewDecision": "APPROVED",
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "merged_prune")
        self.assertEqual(recommendation["reason"], "merged")

    def test_quality_import_extracts_bounded_entry(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "quality.md"
            path.write_text(
                "### author-one — standing: watch\n"
                "signals: false-green x1 · runtime-test-gap x1\n"
                "long body\n"
                "### author-two — standing: trusted\n"
                "clean recovery\n",
                encoding="utf-8",
            )

            events = pr_review.quality_import_events(path)

            self.assertEqual([event["author"] for event in events], ["author-one", "author-two"])
            self.assertIn("false-green", events[0]["quality"]["signals"])
            self.assertIn("runtime-test-gap", events[0]["quality"]["signals"])

    def test_canonical_outcome_maps_tracker_and_unknown_values(self) -> None:
        accepted = pr_review.canonical_outcome(
            {"event_type": "tracker_row", "tracker": {"verdict": "ENQUEUED"}}
        )
        unknown = pr_review.canonical_outcome(
            {"event_type": "custom_event", "tracker": {"verdict": "SURPRISE"}}
        )

        self.assertEqual(accepted.state, "accepted")
        self.assertEqual(unknown.state, "unknown")

    def test_analytics_uses_latest_head_terminal_state_for_success(self) -> None:
        events = [
            {
                "event_type": "changes_requested",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "old-head",
            },
            {
                "event_type": "approved_enqueued",
                "timestamp": "2026-06-28T01:00:00Z",
                "event_id": "b",
                "pr": 1,
                "author": "contributor",
                "head_sha": "new-head",
            },
        ]

        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=False,
        )
        contributor = model["contributors"][0]

        self.assertEqual(contributor["accepted_or_enqueued"], 1)
        self.assertEqual(contributor["blocks"], 1)
        self.assertEqual(contributor["observed_success_rate"], 1.0)
        self.assertEqual(model["prs"][0]["observed_heads"], 2)

    def test_quality_entry_affects_signals_not_pr_activity(self) -> None:
        events = [
            {
                "event_type": "quality_entry",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "author": "contributor",
                "quality": {"login": "contributor", "signals": ["wrong-seam"]},
            }
        ]

        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=True,
        )
        contributor = model["contributors"][0]

        self.assertEqual(contributor["prs"], 0)
        self.assertEqual(contributor["quality_signals"], {"wrong-seam": 1})
        self.assertEqual(contributor["confidence"], "low")

    def test_quality_entry_without_login_does_not_attach_to_pr_activity(self) -> None:
        events = [
            {
                "event_type": "approved_enqueued",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "head",
            },
            {
                "event_type": "quality_entry",
                "timestamp": "2026-06-28T01:00:00Z",
                "event_id": "b",
                "pr": 1,
                "quality": {"signals": ["wrong-seam"]},
            },
        ]

        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=True,
        )

        self.assertEqual(model["prs"][0]["event_count"], 1)
        self.assertEqual(model["contributors"][0]["quality_signals"], {})

    def test_parse_event_datetime_rejects_non_string_and_normalizes_naive_time(self) -> None:
        parsed = pr_review.parse_event_datetime("2026-06-28T00:00:00")

        self.assertIsNone(pr_review.parse_event_datetime(123))
        self.assertEqual(parsed.tzinfo, UTC)

    def test_low_sample_size_gets_insufficient_data_label(self) -> None:
        events = [
            {
                "event_type": "approved_enqueued",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "head",
            }
        ]

        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=3,
            include_open=False,
        )
        contributor = model["contributors"][0]

        self.assertEqual(contributor["confidence"], "low")
        self.assertEqual(contributor["score_label"], "Insufficient Data")

    def test_ascii_renderer_uses_json_model(self) -> None:
        events = [
            {
                "event_type": "approved_enqueued",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "head",
            }
        ]
        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=False,
        )
        args = type("Args", (), {"author": None, "sort": "score", "limit": None})()

        rendered = pr_review.render_analytics_ascii(model, args)

        self.assertIn("Local Observed Review Analytics", rendered)
        self.assertIn("contributor", rendered)

    def test_filter_open_prs_recomputes_contributors_after_refresh(self) -> None:
        events = [
            {
                "event_type": "hold_ci",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "head",
            }
        ]
        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=True,
        )
        model["prs"][0]["terminal_state"] = "merged"
        model["prs"][0]["is_open_or_pending"] = False

        pr_review.filter_open_prs(model, min_prs=1, author=None, refreshed=True)

        self.assertEqual(model["contributors"][0]["terminal_prs"], 1)
        self.assertEqual(model["contributors"][0]["accepted_or_enqueued"], 1)

    def test_github_refresh_warns_on_empty_response(self) -> None:
        events = [
            {
                "event_type": "approved_enqueued",
                "timestamp": "2026-06-28T00:00:00Z",
                "event_id": "a",
                "pr": 1,
                "author": "contributor",
                "head_sha": "head",
            }
        ]
        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=1,
            include_open=True,
        )
        original = pr_review.gh_pr_analytics_state
        pr_review.gh_pr_analytics_state = lambda _repo, _pr_number: None
        try:
            pr_review.apply_github_refresh(model, "phase-rs/phase", min_prs=1, author=None)
        finally:
            pr_review.gh_pr_analytics_state = original

        self.assertEqual(
            model["warnings"],
            ["failed to refresh PR 1: empty or invalid response"],
        )

    def test_command_analytics_sorts_json_without_limit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            events = [
                {
                    "event_type": "changes_requested",
                    "timestamp": "2026-06-28T00:00:00Z",
                    "event_id": "a",
                    "pr": 1,
                    "author": "low-score",
                    "head_sha": "head",
                },
                {
                    "event_type": "approved_enqueued",
                    "timestamp": "2026-06-28T00:01:00Z",
                    "event_id": "b",
                    "pr": 2,
                    "author": "high-score",
                    "head_sha": "head",
                },
            ]
            for event in events:
                pr_review.append_event(state_dir, event)
            args = type(
                "Args",
                (),
                {
                    "state_dir": state_dir,
                    "days": None,
                    "author": None,
                    "min_prs": 1,
                    "include_open": False,
                    "refresh_github": False,
                    "repo": "phase-rs/phase",
                    "limit": None,
                    "sort": "score",
                    "format": "json",
                },
            )()

            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                pr_review.command_analytics(args)
            model = json.loads(output.getvalue())

        self.assertEqual(
            [contributor["login"] for contributor in model["contributors"]],
            ["high-score", "low-score"],
        )

    def test_wrapper_script_exists_and_is_executable(self) -> None:
        wrapper = Path(__file__).resolve().parent / "pr-analytics"

        self.assertTrue(wrapper.exists())
        self.assertTrue(wrapper.stat().st_mode & 0o111)


if __name__ == "__main__":
    unittest.main()
