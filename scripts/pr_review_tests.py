#!/usr/bin/env python3
from __future__ import annotations

import json
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
