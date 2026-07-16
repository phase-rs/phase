#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import re
import subprocess
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
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

            args = type("Args", (), {"state_dir": state_dir, "days": None})()
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

    def test_packet_exposes_quality_label_from_policy(self) -> None:
        policy = pr_review.Policy({"labels": {"quality": "quality"}})
        packet = pr_review.make_packet(
            {
                "number": 5200,
                "state": "OPEN",
                "headRefOid": "head",
                "author": {"login": "contributor"},
                "files": [],
            },
            policy,
            "maintainer",
            "full",
            {},
        )

        self.assertEqual(packet["policy"]["labels"]["quality"], "quality")

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

    def test_missing_hard_required_proof_blocks_approved_pr(self) -> None:
        packet = {
            "pr": {
                "number": 5041,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "APPROVED",
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "proof": {
                "proof_required": True,
                "proof_satisfied": False,
                "proof_gap": True,
                "risk_flags": ["verification-skipped-or-delegated"],
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "request_changes")
        self.assertEqual(recommendation["reason"], "proof_required_missing")
        self.assertEqual(recommendation["proof"]["risk_flags"], ["verification-skipped-or-delegated"])

    def test_conflicting_pr_routes_to_update_branch_handler(self) -> None:
        packet = {
            "pr": {
                "number": 5098,
                "state": "OPEN",
                "headRefOid": "head",
                "mergeStateStatus": "DIRTY",
                "reviewDecision": None,
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "policy_trace": [],
            "proof": {
                "proof_required": True,
                "proof_satisfied": False,
                "proof_gap": True,
                "risk_flags": ["missing-ai-contributor-template"],
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "update_branch_for_handler")
        self.assertEqual(recommendation["reason"], "conflicting")

    def test_requested_changes_recent_current_head_stays_blocked(self) -> None:
        packet = {
            "pr": {
                "number": 5099,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(1),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "blocked")
        self.assertEqual(recommendation["reason"], "changes_requested_current_head")

    def test_expiry_clock_survives_repeated_blocked_recordings(self) -> None:
        """A re-recorded `blocked` event must not reset the requested-changes clock.

        Every sweep appends a fresh `blocked` event for a still-blocked PR, and
        `event_id` hashes the timestamp, so each pass is a distinct row. Anchoring
        the expiry clock on the newest local event pinned `blocker_age` at ~0 and
        silently disabled warn/close for every PR the loop had ever blocked.
        """
        packet = {
            "pr": {
                "number": 4132,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(8),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            # The sweep re-blocked this head one minute ago. Under the old anchor
            # this made blocker_age ~0 and the PR stayed `blocked` forever.
            "local_current_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._minutes_ago(1),
            },
            "local_first_block_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._days_ago(8),
            },
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "warn_stale_changes_for_handler")
        self.assertEqual(recommendation["reason"], "requested_changes_warning_due")

    def test_expiry_clock_falls_back_to_first_local_block_without_formal_review(self) -> None:
        """No formal CHANGES_REQUESTED: age from the FIRST local block, not the latest."""
        packet = {
            "pr": {
                "number": 4589,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": None,
                "isInMergeQueue": False,
                "comments": [],
                "reviews": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._minutes_ago(1),
            },
            "local_first_block_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._days_ago(9),
            },
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "warn_stale_changes_for_handler")
        self.assertEqual(recommendation["reason"], "requested_changes_warning_due")

    def test_posted_warning_does_not_orphan_a_local_only_block(self) -> None:
        """The warning event must not erase the block it was posted about.

        `local_block` reads only the newest event. Once the stale-changes warning is
        recorded it becomes the newest event, so a head blocked only in the local log
        (no formal CHANGES_REQUESTED) would flip `active` to False and its warning
        could never mature into a close.
        """
        packet = {
            "acting_login": "maintainer",
            "pr": {
                "number": 4805,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": None,
                "isInMergeQueue": False,
                "comments": [],
                "reviews": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            # Newest event is the warning we posted 8 days ago — NOT a block.
            "local_current_event": {
                "event_type": "requested_changes_warning",
                "outcome": "requested_changes_warning",
                "head_sha": "head",
                "timestamp": self._days_ago(8),
            },
            "local_first_block_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._days_ago(16),
            },
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        state = pr_review.requested_changes_expiry_state(packet, local_block=False, author_followup_after_local_event=False)

        self.assertTrue(state["active"], "a local-only block must survive its own warning")
        self.assertEqual(state["blocker_timestamp"], packet["local_first_block_event"]["timestamp"])
        self.assertTrue(state["close_due"], "warning older than close_after_warning_days must close")

        recommendation = pr_review.recommend_from_packet(packet)
        self.assertEqual(recommendation["advisory_action"], "close_stale_changes_for_handler")

    def test_fresh_formal_review_restarts_expiry_clock(self) -> None:
        """A NEW CHANGES_REQUESTED on the same head restarts the window (GitHub wins)."""
        packet = {
            "pr": {
                "number": 4133,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(1),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_first_block_event": {
                "event_type": "blocked",
                "outcome": "blocked",
                "head_sha": "head",
                "timestamp": self._days_ago(30),
            },
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "blocked")

    def test_first_block_events_by_pr_head_keeps_earliest_and_ignores_non_blocks(self) -> None:
        events = [
            {"pr": 1, "head_sha": "h", "outcome": "review", "timestamp": "2026-07-01T00:00:00Z"},
            {"pr": 1, "head_sha": "h", "outcome": "blocked", "timestamp": "2026-07-02T00:00:00Z"},
            {"pr": 1, "head_sha": "h", "outcome": "blocked", "timestamp": "2026-07-09T00:00:00Z"},
            {"pr": 1, "head_sha": "other", "outcome": "blocked", "timestamp": "2026-07-05T00:00:00Z"},
        ]

        first = pr_review.first_block_events_by_pr_head(events)

        self.assertEqual(first[(1, "h")]["timestamp"], "2026-07-02T00:00:00Z")
        self.assertEqual(first[(1, "other")]["timestamp"], "2026-07-05T00:00:00Z")

    def test_is_block_event_covers_every_routing_block_shape(self) -> None:
        """The expiry anchor must recognize exactly the events routing calls blocking."""
        for event_type in pr_review.LOCAL_BLOCK_EVENT_TYPES:
            self.assertTrue(pr_review.is_block_event({"event_type": event_type}), event_type)
        for outcome in pr_review.LOCAL_BLOCK_OUTCOMES:
            self.assertTrue(pr_review.is_block_event({"outcome": outcome}), outcome)

        self.assertFalse(pr_review.is_block_event({"event_type": "blocked", "outcome": "ci_failed"}))
        self.assertFalse(pr_review.is_block_event({"event_type": "review"}))
        self.assertFalse(pr_review.is_block_event({"outcome": "approved_enqueued"}))
        self.assertFalse(pr_review.is_block_event(None))

    def test_review_blocked_event_anchors_expiry(self) -> None:
        """`review_blocked` routes as a block, so it must also anchor the expiry clock."""
        events = [
            {
                "pr": 7,
                "head_sha": "h",
                "event_type": "review_blocked",
                "timestamp": "2026-07-01T00:00:00Z",
            },
        ]

        self.assertIn((7, "h"), pr_review.first_block_events_by_pr_head(events))

    def test_requested_changes_warns_after_configured_age(self) -> None:
        packet = {
            "pr": {
                "number": 5100,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_current_event": {
                "event_type": "changes_requested",
                "outcome": "changes_requested",
                "head_sha": "head",
                "timestamp": self._days_ago(8),
            },
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "warn_stale_changes_for_handler")
        self.assertEqual(recommendation["reason"], "requested_changes_warning_due")
        self.assertEqual(
            recommendation["requested_changes_expiry"]["warning_marker"],
            pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
        )

    def test_requested_changes_warning_expires_to_close_handler(self) -> None:
        packet = {
            "acting_login": "maintainer",
            "pr": {
                "number": 5101,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [
                    {
                        "author": "maintainer",
                        "createdAt": self._days_ago(8),
                        "body_excerpt": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                    }
                ],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(20),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "close_stale_changes_for_handler")
        self.assertEqual(recommendation["reason"], "requested_changes_expired")

    def test_requested_changes_warning_marker_survives_comment_excerpt(self) -> None:
        pr = {
            "number": 5103,
            "state": "OPEN",
            "headRefOid": "head",
            "author": {"login": "contributor"},
            "reviewDecision": "CHANGES_REQUESTED",
            "comments": [
                {
                    "author": {"login": "maintainer"},
                    "createdAt": self._days_ago(8),
                    "body": ("x" * 350) + pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            ],
            "reviews": [
                {
                    "author": {"login": "maintainer"},
                    "state": "CHANGES_REQUESTED",
                    "submittedAt": self._days_ago(20),
                    "commit": {"oid": "head"},
                }
            ],
        }
        packet = {
            "acting_login": "maintainer",
            "pr": pr_review.compact_pr_view(pr, "maintainer"),
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertNotIn(
            pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
            packet["pr"]["comments"][0]["body_excerpt"],
        )
        self.assertEqual(recommendation["advisory_action"], "close_stale_changes_for_handler")
        self.assertEqual(recommendation["reason"], "requested_changes_expired")

    def test_author_followup_after_expiry_warning_resurfaces_review(self) -> None:
        packet = {
            "acting_login": "maintainer",
            "pr": {
                "number": 5102,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [
                    {
                        "author": "maintainer",
                        "createdAt": self._days_ago(8),
                        "body_excerpt": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                    },
                    {
                        "author": "contributor",
                        "createdAt": self._days_ago(1),
                        "body_excerpt": "Addressed the requested changes.",
                    },
                ],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(20),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(
            recommendation["reason"], "author_followup_after_requested_changes_warning"
        )

    def test_author_review_after_expiry_warning_resurfaces_review(self) -> None:
        packet = {
            "acting_login": "maintainer",
            "pr": {
                "number": 5104,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [
                    {
                        "author": "maintainer",
                        "createdAt": self._days_ago(8),
                        "body_excerpt": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                    }
                ],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(20),
                        "commit": "head",
                    },
                    {
                        "author": "contributor",
                        "state": "COMMENTED",
                        "submittedAt": self._days_ago(1),
                        "commit": "head",
                    },
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(
            recommendation["reason"], "author_followup_after_requested_changes_warning"
        )

    def test_warning_followup_with_conflict_routes_to_update_branch(self) -> None:
        packet = {
            "acting_login": "maintainer",
            "pr": {
                "number": 5105,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "mergeStateStatus": "DIRTY",
                "isInMergeQueue": False,
                "comments": [
                    {
                        "author": "maintainer",
                        "createdAt": self._days_ago(8),
                        "body_excerpt": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                    },
                    {
                        "author": "contributor",
                        "createdAt": self._days_ago(1),
                        "body_excerpt": "Updated, but now there is a conflict.",
                    },
                ],
                "reviews": [
                    {
                        "author": "maintainer",
                        "state": "CHANGES_REQUESTED",
                        "submittedAt": self._days_ago(20),
                        "commit": "head",
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
            "policy": {
                "requested_changes": {
                    "warning_after_days": 7,
                    "close_after_warning_days": 7,
                    "warning_marker": pr_review.REQUESTED_CHANGES_EXPIRY_MARKER,
                }
            },
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "update_branch_for_handler")
        self.assertEqual(recommendation["reason"], "conflicting_after_author_followup")

    def test_proof_profile_flags_agent_coauthored_incomplete_template(self) -> None:
        profile = pr_review.proof_profile(
            {
                "body": (
                    "## Summary\nFixes admin auth.\n\n"
                    "## Test plan\n"
                    "- [ ] Manual: verify endpoint auth\n"
                    "- [ ] `cargo test` (no Rust toolchain in agent env)\n"
                ),
                "commits": [
                    {
                        "authors": [
                            {"login": "RealDiligent"},
                            {"login": "cursoragent"},
                        ]
                    }
                ],
            },
            {"scrutiny": "maintainer_attention"},
        )

        self.assertTrue(profile["proof_gap"])
        self.assertTrue(profile["agent_coauthored_all_commits"])
        self.assertIn("missing-ai-contributor-template", profile["risk_flags"])
        self.assertIn("unchecked-verification-items", profile["risk_flags"])
        self.assertIn("verification-skipped-or-delegated", profile["risk_flags"])
        self.assertIn("contributor-scrutiny-maintainer_attention", profile["risk_flags"])

    def test_checked_test_evidence_satisfies_proof_despite_manual_items(self) -> None:
        profile = pr_review.proof_profile(
            {
                "body": (
                    "## Summary\nFixes admin auth.\n\n"
                    "## Test plan\n"
                    "- [x] `cargo test -p server-core draft_session`\n"
                    "- [ ] Manual: verify endpoint auth over nginx\n"
                ),
                "commits": [
                    {
                        "authors": [
                            {"login": "RealDiligent"},
                            {"login": "cursoragent"},
                        ]
                    }
                ],
            },
            {"scrutiny": "maintainer_attention"},
        )

        self.assertTrue(profile["proof_required"])
        self.assertTrue(profile["proof_satisfied"])
        self.assertFalse(profile["proof_gap"])
        self.assertIn("unchecked-verification-items", profile["risk_flags"])
        self.assertEqual(
            profile["checked_test_evidence"],
            ["- [x] `cargo test -p server-core draft_session`"],
        )

    def test_template_and_unchecked_items_are_tracked_not_blocking(self) -> None:
        profile = pr_review.proof_profile(
            {
                "body": (
                    "## Problem\nParser fix.\n\n"
                    "## Implementation method (required)\n"
                    "- [ ] Produced via the `/engine-implementer` pipeline\n"
                ),
                "commits": [
                    {
                        "authors": [
                            {"login": "RiskyContributor"},
                        ]
                    }
                ],
            },
            {"scrutiny": "elevated"},
        )

        self.assertFalse(profile["proof_required"])
        self.assertFalse(profile["proof_gap"])
        self.assertIn("missing-ai-contributor-template", profile["risk_flags"])
        self.assertIn("unchecked-verification-items", profile["risk_flags"])
        self.assertIn("contributor-scrutiny-elevated", profile["risk_flags"])
        self.assertEqual(
            profile["tracking_signals"],
            ["ai-template-gap", "unchecked-engine-implementer"],
        )
        self.assertEqual(
            profile["unchecked_items"],
            ["- [ ] Produced via the `/engine-implementer` pipeline"],
        )

    def test_missing_template_alone_is_not_a_proof_gap(self) -> None:
        profile = pr_review.proof_profile(
            {"body": "## Summary\nLegacy PR body.\n", "commits": []},
            {"scrutiny": "normal"},
        )

        self.assertFalse(profile["proof_required"])
        self.assertFalse(profile["proof_gap"])
        self.assertIn("missing-ai-contributor-template", profile["risk_flags"])

    def test_low_risk_parser_refactor_clears_agent_coauthor_proof_gap(self) -> None:
        policy = pr_review.Policy(
            {"path_classes": {"engine": {"patterns": ["crates/engine/**"]}}}
        )
        packet = pr_review.make_packet(
            {
                "number": 5302,
                "title": "refactor(parser): nom P/T remainder combinator",
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "APPROVED",
                "author": {"login": "contributor"},
                "labels": [{"name": "refactor"}],
                "files": [
                    {"path": "crates/engine/src/parser/oracle_static/grammar.rs"},
                    {"path": "crates/engine/src/parser/oracle_static/tests.rs"},
                    {"path": "crates/engine/src/parser/oracle_static/type_change.rs"},
                ],
                "comments": [
                    {
                        "author": {"login": "github-actions"},
                        "body": (
                            f"{pr_review.PARSE_DIFF_MARKER}\n"
                            "### Parse changes introduced by this PR\n\n"
                            "✓ No card-parse changes detected.\n"
                        ),
                        "updatedAt": "2026-07-08T17:14:15Z",
                    }
                ],
                "statusCheckRollup": [
                    {"name": "Rust tests", "status": "COMPLETED", "conclusion": "SUCCESS"}
                ],
                "commits": [
                    {
                        "authors": [
                            {"login": "contributor"},
                            {"login": "cursoragent"},
                        ]
                    }
                ],
            },
            policy,
            "maintainer",
            "full",
            {},
            None,
            {"scrutiny": "maintainer_attention"},
        )

        self.assertTrue(packet["proof"]["proof_required"])
        self.assertTrue(packet["proof"]["proof_satisfied"])
        self.assertFalse(packet["proof"]["proof_gap"])
        self.assertEqual(
            packet["proof"]["proof_override"],
            "low_risk_parser_refactor_green_no_parse_changes",
        )
        self.assertEqual(
            packet["recommendation"]["advisory_action"],
            "approve_ready_for_handler",
        )

    def test_low_risk_refactor_override_does_not_clear_skipped_verification(self) -> None:
        policy = pr_review.Policy(
            {"path_classes": {"engine": {"patterns": ["crates/engine/**"]}}}
        )
        packet = pr_review.make_packet(
            {
                "number": 5303,
                "title": "refactor(parser): nom helper",
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "APPROVED",
                "author": {"login": "contributor"},
                "labels": [{"name": "refactor"}],
                "body": "## Summary\nRefactor parser.\n\n- [ ] `cargo test` (local verification skipped)\n",
                "files": [
                    {"path": "crates/engine/src/parser/oracle_static/type_change.rs"},
                ],
                "comments": [
                    {
                        "author": {"login": "github-actions"},
                        "body": (
                            f"{pr_review.PARSE_DIFF_MARKER}\n"
                            "✓ No card-parse changes detected.\n"
                        ),
                        "updatedAt": "2026-07-08T17:14:15Z",
                    }
                ],
                "statusCheckRollup": [
                    {"name": "Rust tests", "status": "COMPLETED", "conclusion": "SUCCESS"}
                ],
                "commits": [
                    {
                        "authors": [
                            {"login": "contributor"},
                            {"login": "cursoragent"},
                        ]
                    }
                ],
            },
            policy,
            "maintainer",
            "full",
            {},
            None,
            {"scrutiny": "maintainer_attention"},
        )

        self.assertTrue(packet["proof"]["proof_gap"])
        self.assertNotIn("proof_override", packet["proof"])
        self.assertEqual(
            packet["recommendation"]["advisory_action"],
            "request_changes",
        )
        self.assertEqual(
            packet["recommendation"]["reason"],
            "proof_required_missing",
        )

    def test_gittensor_closed_heavy_feeds_proof_risk(self) -> None:
        records = [
            {"author": "Risky", "repository": f"owner/repo{i}", "prState": "CLOSED", "hotkey": "hk"}
            for i in range(20)
        ]
        records += [
            {"author": "Risky", "repository": "owner/good", "prState": "MERGED", "hotkey": "hk"}
            for _ in range(5)
        ]

        index = pr_review.build_gittensor_index(records)
        summary = pr_review.gittensor_summary("risky", index, None)
        profile = pr_review.proof_profile({"body": "", "commits": []}, None, summary)

        self.assertTrue(summary["present"])
        self.assertEqual(summary["states"]["CLOSED"], 20)
        self.assertEqual(summary["risk_flag"], "gittensor-closed-heavy")
        self.assertIn("gittensor-closed-heavy", profile["risk_flags"])
        self.assertTrue(profile["proof_gap"])

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

    def test_author_followup_after_local_block_resurfaces_same_head(self) -> None:
        packet = {
            "pr": {
                "number": 5014,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [
                    {
                        "author": "contributor",
                        "createdAt": self._minutes_ago(1),
                    }
                ],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_current_event": {
                "event_type": "changes_requested",
                "outcome": "changes_requested",
                "head_sha": "head",
                "timestamp": self._minutes_ago(2),
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(
            recommendation["reason"], "author_followup_after_maintainer_activity"
        )

    def test_edited_author_followup_is_not_acknowledged_by_later_hold_event(self) -> None:
        local_event = {
            "event_type": "held",
            "outcome": "held",
            "head_sha": "head",
            "timestamp": self._minutes_ago(1),
        }
        pr = {
            "number": 5015,
            "state": "OPEN",
            "headRefOid": "head",
            "author_login": "contributor",
            "reviewDecision": "CHANGES_REQUESTED",
            "isInMergeQueue": False,
            "commentsComplete": True,
            "comments": [
                {
                    "author": "maintainer",
                    "createdAt": self._days_ago(1),
                    "updatedAt": self._days_ago(1),
                },
                {
                    "author": "contributor",
                    "createdAt": self._days_ago(2),
                    "updatedAt": self._minutes_ago(2),
                },
            ],
            "reviews": [],
        }
        packet = {
            "pr": pr,
            "acting_login": "maintainer",
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_current_event": local_event,
            "freshness": pr_review.review_freshness(
                pr, "maintainer", local_event, local_event
            ),
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(
            recommendation["reason"], "author_followup_after_maintainer_activity"
        )

    def test_changed_head_never_inherits_previous_hold(self) -> None:
        previous_event = {
            "event_type": "held",
            "outcome": "held",
            "head_sha": "old-head",
            "timestamp": self._minutes_ago(5),
        }
        pr = {
            "number": 5016,
            "state": "OPEN",
            "headRefOid": "new-head",
            "author_login": "contributor",
            "reviewDecision": "CHANGES_REQUESTED",
            "isInMergeQueue": False,
            "commentsComplete": True,
            "comments": [],
            "reviews": [],
        }
        packet = {
            "pr": pr,
            "acting_login": "maintainer",
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "old-head",
            "local_current_event": None,
            "freshness": pr_review.review_freshness(
                pr, "maintainer", None, previous_event
            ),
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "head_changed_since_local_event")

    def test_incomplete_author_history_does_not_preserve_hold(self) -> None:
        packet = {
            "pr": {
                "number": 5017,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_current_event": {
                "event_type": "held",
                "outcome": "held",
                "head_sha": "head",
                "timestamp": self._minutes_ago(2),
            },
            "freshness": {"comment_history_incomplete": True},
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "author_activity_history_incomplete")

    def test_author_followup_resurfaces_queued_pr(self) -> None:
        packet = {
            "pr": {
                "number": 5018,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "APPROVED",
                "isInMergeQueue": True,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "freshness": {"author_followup_after_maintainer_activity": True},
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(
            recommendation["reason"], "author_followup_after_maintainer_activity"
        )

    def test_local_block_without_author_followup_stays_blocked(self) -> None:
        packet = {
            "pr": {
                "number": 5014,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "local_current_event": {
                "event_type": "changes_requested",
                "outcome": "changes_requested",
                "head_sha": "head",
                "timestamp": self._minutes_ago(2),
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "blocked")
        self.assertEqual(recommendation["reason"], "local_block_current_head")

    def test_parse_diff_after_local_block_resurfaces_same_head(self) -> None:
        packet = {
            "pr": {
                "number": 5019,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "",
                "isInMergeQueue": False,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "changes_requested",
                "outcome": "changes_requested",
                "head_sha": "head",
                "timestamp": self._minutes_ago(2),
            },
            "parse_diff": {
                "present": True,
                "state": "no_changes",
                "updated_at": self._minutes_ago(1),
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "parse_diff_after_local_block")

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

    def test_finalize_recomputes_contributors_after_open_filter(self) -> None:
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
        model["prs"] = [pr for pr in model["prs"] if not pr["is_open_or_pending"]]

        pr_review.finalize_contributor_model(model, min_prs=1, author=None, refreshed=True)

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
        original = pr_review.gh_pr_refresh_chunk
        pr_review.gh_pr_refresh_chunk = lambda _repo, _numbers: {"1": None}
        try:
            pr_review.apply_github_refresh(model, "phase-rs/phase")
        finally:
            pr_review.gh_pr_refresh_chunk = original

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

    def test_canonical_from_text_covers_every_mapping_branch(self) -> None:
        cases = [
            ("changes-requested", ("changes_requested", "negative_review")),
            ("request-changes", ("changes_requested", "negative_review")),
            ("reviewed-request-changes", ("changes_requested", "negative_review")),
            ("still-blocked", ("blocked", "blocked")),
            ("blocked", ("blocked", "blocked")),
            ("blocked-on-author", ("blocked", "blocked")),
            ("hard-stop", ("blocked", "hard_stop")),
            ("merged", ("merged", "merged")),
            ("pruned-as-merged", ("merged", "merged")),
            ("pruned-merged", ("merged", "merged")),
            ("defer-fe", ("deferred", "deferred")),
            ("defer", ("deferred", "deferred")),
            ("deferred", ("deferred", "deferred")),
            ("ci-failed", ("changes_requested", "ci_failed")),
            ("pending-ci", ("held_ci", "ci_pending")),
            ("hold-ci", ("held_ci", "ci_pending")),
            ("hold", ("held", "held")),
            ("held", ("held", "held")),
            ("held-for-author", ("held", "held")),
            ("approved-enqueued", ("accepted", "approved_enqueued")),
            ("approved-labeled-enqueued", ("accepted", "approved_enqueued")),
            ("enqueued", ("accepted", "enqueued")),
            ("handler-enqueue", ("accepted", "enqueued")),
            ("approved", ("accepted", "approved")),
            ("approve", ("accepted", "approved")),
            ("approve-pending", ("held_ci", "approval_pending_ci")),
            ("content-clean-pending", ("held_ci", "approval_pending_ci")),
            ("review", ("review", "review")),
            ("review-needed", ("review", "review")),
            ("pending", ("pending", "pending")),
            ("pending-author", ("pending", "pending")),
            ("closed", ("closed", "closed")),
            ("superseded", ("closed", "closed")),
            ("queued", ("accepted", "queued")),
            ("pruned", ("accepted", "pruned")),
        ]
        for value, expected in cases:
            with self.subTest(value=value):
                self.assertEqual(pr_review.canonical_from_text(value), expected)
        self.assertIsNone(pr_review.canonical_from_text("totally-unknown-value"))
        self.assertIsNone(pr_review.canonical_from_text(""))
        self.assertIsNone(pr_review.canonical_from_text(None))

    def _record_args(self, state_dir: Path, event_path: Path, force: bool = False):
        return type(
            "Args",
            (),
            {"state_dir": state_dir, "event_json": str(event_path), "force": force},
        )()

    def test_record_validates_vocabulary_and_lowercases_outcome(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            valid_path = state_dir / "valid.json"
            valid_path.write_text(
                json.dumps(
                    {"event_type": "approved_enqueued", "pr": 5, "head_sha": "h", "outcome": "APPROVED"}
                ),
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, valid_path))
            result = json.loads(output.getvalue())
            self.assertEqual(code, 0)
            self.assertTrue(result["inserted"])
            events = pr_review.all_events(state_dir)
            self.assertEqual(events[0]["outcome"], "approved")

            bad_path = state_dir / "bad.json"
            bad_path.write_text(
                json.dumps({"event_type": "not_a_real_type", "pr": 6}), encoding="utf-8"
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, bad_path))
            rejected = json.loads(output.getvalue())
            self.assertEqual(code, 1)
            self.assertFalse(rejected["inserted"])
            self.assertIn("not_a_real_type", rejected["error"])
            self.assertIn("observation", rejected["allowed_event_types"])
            self.assertIn("approved", rejected["allowed_outcomes"])

            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, bad_path, force=True))
            forced = json.loads(output.getvalue())
            self.assertEqual(code, 0)
            self.assertTrue(forced["inserted"])
            self.assertTrue(forced["forced"])

    def test_append_event_is_idempotent_under_flock(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            event = {
                "event_type": "review",
                "timestamp": "2026-06-28T00:00:00Z",
                "pr": 42,
                "author": "contributor",
                "head_sha": "abc",
            }
            self.assertTrue(pr_review.append_event(state_dir, event))
            self.assertFalse(pr_review.append_event(state_dir, dict(event)))
            lines = (state_dir / "review-events.jsonl").read_text().strip().splitlines()
            self.assertEqual(len(lines), 1)

    def test_event_skeleton_lists_expiry_event_types(self) -> None:
        skeleton = pr_review.event_skeleton(
            5103, {"headRefOid": "head", "author_login": "contributor"}
        )

        self.assertIn("requested_changes_warning", skeleton["event_type"])
        self.assertIn("stale_changes_closed", skeleton["event_type"])

    def test_candidate_sort_orders_by_action_priority(self) -> None:
        candidates = [
            {"advisory_action": "skip", "pr": 10},
            {"advisory_action": "review", "created_at": "2026-06-02T00:00:00Z", "pr": 3},
            {"advisory_action": "dequeue_stale_for_handler", "updated_at": "2026-06-01T00:00:00Z", "pr": 8},
            {"advisory_action": "review", "created_at": "2026-06-01T00:00:00Z", "pr": 4},
        ]
        ordered = [c["advisory_action"] for c in sorted(candidates, key=pr_review.candidate_sort_key)]
        self.assertEqual(
            ordered,
            ["dequeue_stale_for_handler", "review", "review", "skip"],
        )
        # review is ordered by created-date: the 06-01 review precedes the 06-02 review.
        review_prs = [
            c["pr"]
            for c in sorted(candidates, key=pr_review.candidate_sort_key)
            if c["advisory_action"] == "review"
        ]
        self.assertEqual(review_prs, [4, 3])

    def test_files_truncated_forces_manual_review(self) -> None:
        policy = pr_review.Policy(
            {"path_classes": {"frontend": {"patterns": ["client/**"]}}}
        )
        pr = {
            "number": 4600,
            "state": "OPEN",
            "headRefOid": "head",
            "changedFiles": 150,
            "files": [{"path": f"client/src/f{i}.tsx"} for i in range(3)],
        }
        packet = pr_review.make_packet(pr, policy, "maintainer", "full", {})

        self.assertTrue(packet["classification"]["files_truncated"])
        self.assertEqual(packet["classification"]["surface"], "files_truncated")
        self.assertEqual(packet["recommendation"]["advisory_action"], "review")
        self.assertEqual(
            packet["recommendation"]["reason"], "files_truncated_needs_manual_classification"
        )

    def test_files_truncated_honors_current_head_local_block(self) -> None:
        packet = {
            "pr": {
                "number": 5155,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "CHANGES_REQUESTED",
                "isInMergeQueue": False,
                "comments": [],
            },
            "ci": {"state": "green"},
            "classification": {
                "files_truncated": True,
                "hard_stop_paths": [],
                "surface": "files_truncated",
            },
            "latest_maintainer_review_commit": "head",
            "local_current_event": {
                "event_type": "blocked",
                "outcome": "changes_requested",
                "head_sha": "head",
                "timestamp": "2026-07-05T20:26:49Z",
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "blocked")
        self.assertEqual(recommendation["reason"], "local_block_current_head")

    def test_normalize_graphql_pr_maps_status_contexts(self) -> None:
        node = {
            "number": 1,
            "author": {"login": "contributor"},
            "commits": {
                "nodes": [
                    {
                        "commit": {
                            "statusCheckRollup": {
                                "contexts": {
                                    "nodes": [
                                        {
                                            "__typename": "CheckRun",
                                            "name": "clippy",
                                            "status": "COMPLETED",
                                            "conclusion": "SUCCESS",
                                        },
                                        {
                                            "__typename": "StatusContext",
                                            "context": "legacy-ci",
                                            "state": "FAILURE",
                                        },
                                    ]
                                }
                            }
                        }
                    }
                ]
            },
        }
        normalized = pr_review.normalize_graphql_pr(node)
        summary = pr_review.status_summary(normalized["statusCheckRollup"])

        self.assertEqual(summary["state"], "failed")
        self.assertIn("legacy-ci", summary["failures"])
        self.assertIn("clippy", summary["successes"])

    def test_status_summary_ignores_non_required_failed_checks(self) -> None:
        summary = pr_review.status_summary(
            [
                {
                    "name": "Rust (fmt, clippy, test, coverage-gate)",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                },
                {
                    "name": "Frontend (lint, type-check, test)",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                },
                {
                    "name": "Contributor trust",
                    "status": "COMPLETED",
                    "conclusion": "ACTION_REQUIRED",
                },
            ],
            {
                "Rust (fmt, clippy, test, coverage-gate)",
                "Frontend (lint, type-check, test)",
            },
        )

        self.assertEqual(summary["state"], "green")
        self.assertEqual(summary["failures"], [])
        self.assertEqual(
            summary["advisory"],
            [
                {
                    "name": "Contributor trust",
                    "status": "COMPLETED",
                    "conclusion": "ACTION_REQUIRED",
                }
            ],
        )

    def test_status_summary_waits_for_missing_required_check(self) -> None:
        summary = pr_review.status_summary(
            [
                {
                    "name": "Rust (fmt, clippy, test, coverage-gate)",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                }
            ],
            {
                "Rust (fmt, clippy, test, coverage-gate)",
                "Frontend (lint, type-check, test)",
            },
        )

        self.assertEqual(summary["state"], "pending")
        self.assertEqual(summary["pending"], ["Frontend (lint, type-check, test)"])

    def test_recommend_defer_fe_is_case_insensitive(self) -> None:
        for outcome in ("DEFER-FE", "defer-fe"):
            packet = {
                "pr": {
                    "number": 4700,
                    "state": "OPEN",
                    "headRefOid": "head",
                    "reviewDecision": "",
                    "isInMergeQueue": False,
                },
                "ci": {"state": "green"},
                "classification": {"hard_stop_paths": [], "surface": "backend"},
                "latest_maintainer_review_commit": None,
                "local_current_event": {"event_type": "deferred", "outcome": outcome},
                "policy_trace": [],
            }
            recommendation = pr_review.recommend_from_packet(packet)
            self.assertEqual(recommendation["advisory_action"], "defer")
            self.assertEqual(recommendation["reason"], "local_defer_fe_current_head")

    def test_parse_diff_comment_state_classifies_all_states(self) -> None:
        marker = pr_review.PARSE_DIFF_MARKER
        bot = {"login": "github-actions"}

        absent = pr_review.parse_diff_comment_state(
            [{"author": bot, "body": "just a normal comment", "updatedAt": "2026-06-30T00:00:00Z"}]
        )
        self.assertEqual(absent, {"present": False, "state": "absent", "updated_at": None})

        # A marker-shaped body from a non-bot author is a spoof and must not classify.
        spoofed = pr_review.parse_diff_comment_state(
            [
                {
                    "author": {"login": "contrib"},
                    "body": f"{marker}\n2 signature(s) changed",
                    "updatedAt": "2026-06-30T00:30:00Z",
                }
            ]
        )
        self.assertEqual(spoofed["state"], "absent")

        real = pr_review.parse_diff_comment_state(
            [{"author": bot, "body": f"{marker}\n2 signature(s) changed", "updatedAt": "2026-06-30T01:00:00Z"}]
        )
        self.assertEqual(real["state"], "real_changes")
        self.assertTrue(real["present"])
        self.assertEqual(real["updated_at"], "2026-06-30T01:00:00Z")

        pending = pr_review.parse_diff_comment_state(
            [
                {
                    "author": bot,
                    "body": f"{marker}\nBaseline pending (R2 baseline not found)",
                    "updatedAt": "2026-06-30T02:00:00Z",
                }
            ]
        )
        self.assertEqual(pending["state"], "baseline_pending")
        self.assertEqual(pending["updated_at"], "2026-06-30T02:00:00Z")

        no_changes = pr_review.parse_diff_comment_state(
            [
                {
                    "author": bot,
                    "body": f"{marker}\nNo parse-detail changes in this diff.",
                    "updatedAt": "2026-06-30T03:00:00Z",
                }
            ]
        )
        self.assertEqual(no_changes["state"], "no_changes")
        self.assertTrue(no_changes["present"])

    def test_recommend_flags_engine_baseline_pending(self) -> None:
        base_packet = {
            "pr": {
                "number": 4900,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
            },
            "ci": {"state": "green"},
            "classification": {
                "hard_stop_paths": [],
                "surface": "backend",
                "path_classes": {"engine": ["crates/engine/src/x.rs"]},
            },
            "latest_maintainer_review_commit": None,
            "policy_trace": [],
            "parse_diff": {"present": True, "state": "baseline_pending", "updated_at": "t"},
        }

        recommendation = pr_review.recommend_from_packet(base_packet)
        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "review_parse_baseline_pending")

        repeated_maintainer_merge = copy.deepcopy(base_packet)
        repeated_maintainer_merge["pr"]["reviewDecision"] = "CHANGES_REQUESTED"
        repeated_maintainer_merge["pr"]["maintainer_merge_commit_count"] = 1
        repeated_maintainer_merge["latest_maintainer_review_commit"] = "old_head"
        ready = pr_review.recommend_from_packet(repeated_maintainer_merge)
        self.assertEqual(ready["advisory_action"], "approve_ready_for_handler")
        self.assertEqual(
            ready["reason"],
            "baseline_pending_after_maintainer_merge_ready",
        )

        needs_review = copy.deepcopy(base_packet)
        needs_review["pr"]["maintainer_merge_commit_count"] = 1
        maintainer_merge = pr_review.recommend_from_packet(needs_review)
        self.assertEqual(maintainer_merge["advisory_action"], "review")
        self.assertEqual(
            maintainer_merge["reason"],
            "review_parse_baseline_pending_after_maintainer_merge",
        )

        # Frontend-only surface (no engine path class) keeps the generic review reason.
        frontend_packet = copy.deepcopy(base_packet)
        frontend_packet["classification"] = {
            "hard_stop_paths": [],
            "surface": "backend",
            "path_classes": {"skill": ["docs/x.md"]},
        }
        frontend = pr_review.recommend_from_packet(frontend_packet)
        self.assertEqual(frontend["reason"], "needs_review")

    def test_wrapper_script_exists_and_is_executable(self) -> None:
        wrapper = Path(__file__).resolve().parent / "pr-analytics"

        self.assertTrue(wrapper.exists())
        self.assertTrue(wrapper.stat().st_mode & 0o111)

    @staticmethod
    def _days_ago(days: int) -> str:
        stamp = datetime.now(UTC).replace(microsecond=0) - timedelta(days=days)
        return stamp.isoformat().replace("+00:00", "Z")

    @staticmethod
    def _minutes_ago(minutes: int) -> str:
        stamp = datetime.now(UTC).replace(microsecond=0) - timedelta(minutes=minutes)
        return stamp.isoformat().replace("+00:00", "Z")

    @staticmethod
    def _signal_event(pr: int, author: str, signals: list[str], days_ago: int) -> dict:
        return {
            "event_type": "review",
            "timestamp": PrReviewTests._days_ago(days_ago),
            "event_id": f"sig-{pr}-{author}-{days_ago}",
            "pr": pr,
            "author": author,
            "head_sha": f"head-{pr}",
            "signals": signals,
        }

    def _summary_for(
        self,
        events: list[dict],
        author: str,
        current_pr: int | None,
        overrides: dict | None = None,
    ) -> dict:
        model = pr_review.build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=pr_review.ANALYTICS_DEFAULT_MIN_PRS,
            include_open=True,
        )
        return pr_review.build_contributor_summary(
            author,
            current_pr,
            model,
            pr_review.collect_signal_occurrences(events),
            overrides or {},
        )

    def test_first_contribution_excludes_current_pr(self) -> None:
        events = [
            {
                "event_type": "review",
                "timestamp": self._days_ago(1),
                "event_id": "a",
                "pr": 10,
                "author": "newbie",
                "head_sha": "h1",
            }
        ]

        same_pr = self._summary_for(events, "newbie", current_pr=10)
        self.assertTrue(same_pr["first_contribution"])
        self.assertEqual(same_pr["prior_prs"], 0)

        other_pr = self._summary_for(events, "newbie", current_pr=11)
        self.assertFalse(other_pr["first_contribution"])
        self.assertEqual(other_pr["prior_prs"], 1)

        unseen = self._summary_for(events, "brand-new", current_pr=1)
        self.assertTrue(unseen["first_contribution"])
        self.assertIsNone(unseen["score"])

    def test_recurrence_window_counts_distinct_prs(self) -> None:
        events = [
            self._signal_event(1, "repeat", ["false-green"], days_ago=5),
            self._signal_event(2, "repeat", ["false-green"], days_ago=10),
            # Outside RECURRENCE_WINDOW_DAYS: must not count toward the window.
            self._signal_event(3, "repeat", ["false-green"], days_ago=200),
        ]

        summary = self._summary_for(events, "repeat", current_pr=99)
        entry = next(r for r in summary["recurrence"] if r["signal"] == "false-green")
        self.assertEqual(entry["distinct_prs_window"], 2)
        self.assertEqual(summary["scrutiny"], "elevated")
        self.assertTrue(
            any(reason.startswith("recurrence_false-green") for reason in summary["scrutiny_reasons"])
        )
        self.assertFalse(summary["light_touch_eligible"])

        events.append(self._signal_event(4, "repeat", ["false-green"], days_ago=2))
        attention = self._summary_for(events, "repeat", current_pr=99)
        self.assertEqual(attention["scrutiny"], "maintainer_attention")

    def test_legacy_quality_entry_excluded_from_recurrence(self) -> None:
        events = [
            {
                "event_type": "quality_entry",
                "timestamp": self._days_ago(1),
                "event_id": "q",
                "author": "legacy",
                "quality": {"login": "legacy", "signals": ["wrong-seam"]},
            }
        ]

        summary = self._summary_for(events, "legacy", current_pr=1)
        self.assertEqual(summary["recurrence"], [])
        self.assertEqual(summary["scrutiny"], "normal")
        # Lifetime aggregation still sees the legacy signal.
        self.assertEqual(summary["top_signals"], [{"signal": "wrong-seam", "count": 1}])

    def test_standing_override_skip_recommends_skip_and_traces(self) -> None:
        overrides = {"contributor_standing": {"Dale053": {"standing": "skip", "note": "probation"}}}
        summary = self._summary_for([], "dale053", current_pr=1, overrides=overrides)
        self.assertEqual(summary["standing"], "skip")
        self.assertEqual(summary["standing_source"], "override")

        policy = pr_review.Policy({"hard_stops": {"patterns": [".claude/skills/**"]}})
        pr = {
            "number": 1,
            "state": "OPEN",
            "author": {"login": "dale053"},
            "files": [{"path": "crates/engine/src/lib.rs"}],
            "changedFiles": 1,
        }
        packet = pr_review.make_packet(pr, policy, "maintainer", "full", overrides, None, summary)
        self.assertEqual(packet["recommendation"]["advisory_action"], "skip")
        self.assertEqual(packet["recommendation"]["reason"], "contributor_standing_skip")
        self.assertIn("matched:standing_skip", packet["policy_trace"])
        self.assertEqual(packet["recommendation"]["contributor"]["standing"], "skip")

        # Safety ordering: a guarded path still wins over the skip standing, but the
        # matched standing pattern stays visible in the trace.
        hard_stop_pr = dict(pr)
        hard_stop_pr["files"] = [{"path": ".claude/skills/pr-review-loop/SKILL.md"}]
        hard_stop_packet = pr_review.make_packet(
            hard_stop_pr, policy, "maintainer", "full", overrides, None, summary
        )
        self.assertEqual(hard_stop_packet["recommendation"]["advisory_action"], "request_changes")
        self.assertEqual(hard_stop_packet["recommendation"]["reason"], "hard_stop")
        self.assertIn("matched:standing_skip", hard_stop_packet["policy_trace"])

    def test_standing_watch_forces_elevated_scrutiny(self) -> None:
        overrides = {"contributor_standing": {"jaso0n0818": {"standing": "watch"}}}
        summary = self._summary_for([], "jaso0n0818", current_pr=1, overrides=overrides)
        self.assertEqual(summary["standing"], "watch")
        self.assertEqual(summary["scrutiny"], "elevated")
        self.assertIn("standing_watch", summary["scrutiny_reasons"])
        self.assertFalse(summary["light_touch_eligible"])

        unknown_standing = {"contributor_standing": {"jaso0n0818": {"standing": "banished"}}}
        ignored = self._summary_for([], "jaso0n0818", current_pr=1, overrides=unknown_standing)
        self.assertEqual(ignored["standing"], "unknown")

    def test_derived_trusted_requires_clean_window(self) -> None:
        events = [
            {
                "event_type": "approved_enqueued",
                "timestamp": self._days_ago(30 + pr),
                "event_id": f"ok-{pr}",
                "pr": pr,
                "author": "solid",
                "head_sha": f"h{pr}",
            }
            for pr in range(1, 6)
        ]

        summary = self._summary_for(events, "solid", current_pr=99)
        self.assertEqual(summary["standing"], "trusted")
        self.assertEqual(summary["standing_source"], "derived")
        self.assertEqual(summary["scrutiny"], "normal")
        self.assertTrue(summary["light_touch_eligible"])

        # One windowed signal occurrence breaks the clean-window requirement.
        events.append(self._signal_event(6, "solid", ["fmt/clippy-slip"], days_ago=3))
        dirty = self._summary_for(events, "solid", current_pr=99)
        self.assertEqual(dirty["standing"], "unknown")
        self.assertFalse(dirty["light_touch_eligible"])

    def test_record_validates_signals_vocabulary(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            bad_path = state_dir / "bad.json"
            bad_path.write_text(
                json.dumps(
                    {"event_type": "review", "pr": 7, "head_sha": "h", "signals": ["bogus-signal"]}
                ),
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, bad_path))
            rejected = json.loads(output.getvalue())
            self.assertEqual(code, 1)
            self.assertIn("bogus-signal", rejected["error"])
            self.assertIn("wrong-seam", rejected["allowed_signals"])

            good_path = state_dir / "good.json"
            good_path.write_text(
                json.dumps(
                    {"event_type": "review", "pr": 7, "head_sha": "h", "signals": ["wrong-seam"]}
                ),
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, good_path))
            self.assertEqual(code, 0)
            self.assertTrue(json.loads(output.getvalue())["inserted"])

    def test_analytics_groups_logins_case_insensitively(self) -> None:
        events = [
            {
                "event_type": "review",
                "timestamp": self._days_ago(2),
                "event_id": "a",
                "pr": 1,
                "author": "Contrib",
                "head_sha": "h1",
            },
            {
                "event_type": "review",
                "timestamp": self._days_ago(1),
                "event_id": "b",
                "pr": 2,
                "author": "contrib",
                "head_sha": "h2",
            },
        ]

        model = pr_review.build_analytics_model(
            events, days=None, author=None, min_prs=1, include_open=True
        )
        self.assertEqual(len(model["contributors"]), 1)
        row = model["contributors"][0]
        self.assertEqual(row["login"], "Contrib")
        self.assertEqual(row["prs"], 2)

    def test_analytics_tolerates_explicit_null_quality(self) -> None:
        # A logged event can carry "quality": null (JSON round-trip or --force
        # record); signal aggregation must treat it like an absent block.
        events = [
            {
                "event_type": "quality_entry",
                "timestamp": self._days_ago(2),
                "event_id": "q1",
                "author": "contrib",
                "quality": None,
            },
            {
                "event_type": "review",
                "timestamp": self._days_ago(1),
                "event_id": "r1",
                "pr": 1,
                "author": "contrib",
                "head_sha": "h1",
                "quality": None,
            },
        ]
        model = pr_review.build_analytics_model(
            events, days=None, author=None, min_prs=1, include_open=True
        )
        self.assertEqual(len(model["contributors"]), 1)
        self.assertEqual(model["contributors"][0]["quality_signals"], {})

    def test_praise_signals_credit_score_and_skip_recurrence(self) -> None:
        # Same praise on five distinct PRs in-window: credit is capped, and praise
        # never reaches recurrence, top_signals, or scrutiny elevation.
        events = [
            self._signal_event(
                pr, "gooddev", ["right-seam", "discriminating-runtime-test"], days_ago=pr
            )
            for pr in range(1, 6)
        ]

        summary = self._summary_for(events, "gooddev", current_pr=99)
        self.assertEqual(summary["recurrence"], [])
        self.assertEqual(summary["scrutiny"], "normal")

        model = pr_review.build_analytics_model(
            events, days=None, author=None, min_prs=1, include_open=True
        )
        row = next(r for r in model["contributors"] if r["login"] == "gooddev")
        self.assertEqual(row["score_components"]["praise_credit"], pr_review.PRAISE_CREDIT_CAP)
        self.assertEqual(row["top_signals"], [])
        self.assertEqual(
            row["praise_signals"],
            {"discriminating-runtime-test": 5, "right-seam": 5},
        )

    def test_legacy_signal_aliases_normalize_and_strays_are_audited(self) -> None:
        # The two pre-validation stray events: aliasable tokens become canonical
        # praise; unintelligible tokens are dropped from metrics but audited.
        events = [
            self._signal_event(
                1,
                "ntindle",
                ["runtime-test-present", "gemini-case-finding-refuted", "strive-static-bypass"],
                days_ago=1,
            )
        ]

        model = pr_review.build_analytics_model(
            events, days=None, author=None, min_prs=1, include_open=True
        )
        row = next(r for r in model["contributors"] if r["login"] == "ntindle")
        self.assertEqual(
            row["praise_signals"],
            {"discriminating-runtime-test": 1, "evidence-backed-pushback": 1},
        )
        self.assertEqual(model["unknown_signals"], {"strive-static-bypass": 1})

        summary = self._summary_for(events, "ntindle", current_pr=2)
        self.assertEqual(summary["recurrence"], [])
        self.assertEqual(summary["praise_signals"]["evidence-backed-pushback"], 1)

    def test_record_accepts_praise_vocabulary(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            event_path = state_dir / "praise.json"
            event_path.write_text(
                json.dumps(
                    {"event_type": "review", "pr": 8, "head_sha": "h", "signals": ["right-seam"]}
                ),
                encoding="utf-8",
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = pr_review.command_record(self._record_args(state_dir, event_path))
            self.assertEqual(code, 0)
            self.assertTrue(json.loads(output.getvalue())["inserted"])

    def test_make_packet_without_summary_has_null_contributor(self) -> None:
        policy = pr_review.Policy({})
        pr = {
            "number": 1,
            "state": "OPEN",
            "author": {"login": "someone"},
            "files": [{"path": "crates/engine/src/lib.rs"}],
            "changedFiles": 1,
        }
        packet = pr_review.make_packet(pr, policy, "maintainer", "full", {})
        self.assertIsNone(packet["contributor"])
        self.assertNotIn("matched:standing_skip", packet["policy_trace"])
        self.assertNotEqual(packet["recommendation"]["reason"], "contributor_standing_skip")

    @staticmethod
    def _artifact_body(
        head: str,
        *,
        gate_head: str | None = None,
        review_head: str | None = None,
        anchor_count: int = 2,
        unchecked: bool = False,
    ) -> str:
        anchors = "\n".join(
            f"- crates/engine/src/example_{index}.rs:{index + 10} — analogous seam"
            for index in range(anchor_count)
        )
        verification_lines = [
            f"- [x] {label}" for label in pr_review.REQUIRED_VERIFICATION_CHECKBOX_LABELS
        ]
        if unchecked:
            verification_lines[0] = verification_lines[0].replace("- [x]", "- [ ]", 1)
        verification_lines.append("- `cargo test` — clean")
        verification = "\n".join(verification_lines)
        return (
            "## Summary\nChange.\n\n"
            "## Implementation method (required)\nMethod: /engine-implementer\n\n"
            f"## Verification\n{verification}\n\n"
            "## Gate A\n"
            f"Gate A PASS head={gate_head or head} base={'b' * 40}\n\n"
            f"## Anchored on\n{anchors}\n\n"
            "## Final review-impl\n"
            f"Final review-impl PASS head={review_head or head}\n\n"
            "## Claimed parse impact\n- Test Card\n"
        )

    def test_artifact_profile_is_sha_bound_and_audit_only(self) -> None:
        head = "a" * 40
        policy = pr_review.Policy(
            {"admission": {"mode": "audit", "enforced_after": "2026-07-01T00:00:00Z"}}
        )
        passing = pr_review.artifact_profile(
            {
                "headRefOid": head,
                "createdAt": "2026-07-02T00:00:00Z",
                "body": self._artifact_body(head),
            },
            policy,
        )
        self.assertTrue(passing["passes"])
        self.assertFalse(passing["would_decline"])
        self.assertFalse(passing["enforced"])
        self.assertEqual(passing["claimed_cards"], ["Test Card"])

        failing = pr_review.artifact_profile(
            {
                "headRefOid": head,
                "createdAt": "2026-07-02T00:00:00Z",
                "body": self._artifact_body(
                    head,
                    gate_head="c" * 40,
                    review_head="d" * 40,
                    anchor_count=1,
                    unchecked=True,
                ),
            },
            policy,
        )
        self.assertEqual(
            failing["failures"],
            [
                "stale_gate_a_head",
                "stale_final_review_impl_head",
                "fewer_than_two_anchors",
                "unchecked_required_verification",
                "invalid_required_verification_checkboxes",
            ],
        )
        self.assertTrue(failing["would_decline"])
        self.assertFalse(failing["decline"])

    def test_artifact_enforcement_requires_immutable_cutoff(self) -> None:
        head = "a" * 40
        policy = pr_review.Policy(
            {"admission": {"mode": "enforce", "enforced_after": "2026-07-10T00:00:00Z"}}
        )
        before = pr_review.artifact_profile(
            {"headRefOid": head, "createdAt": "2026-07-09T00:00:00Z", "body": ""},
            policy,
        )
        after = pr_review.artifact_profile(
            {"headRefOid": head, "createdAt": "2026-07-11T00:00:00Z", "body": ""},
            policy,
        )
        self.assertFalse(before["decline"])
        self.assertTrue(after["decline"])

    def test_architecture_policy_uses_supplied_patterns_and_accepted_label(self) -> None:
        policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
        self.assertEqual(
            policy.raw["admission"],
            {"mode": "audit", "enforced_after": "", "accepted_issue_label": "accepted"},
        )
        expected = [
            "crates/engine/src/types/format.rs",
            "crates/engine/src/types/card_type.rs",
            "crates/engine/src/game/mod.rs",
            "crates/engine/src/game/deck_loading.rs",
            "crates/engine/src/game/deck_validation.rs",
            "crates/engine/src/game/match_flow.rs",
            "crates/engine/src/game/mulligan.rs",
            "crates/server-core/**",
            "crates/phase-server/src/main.rs",
        ]
        self.assertEqual(policy.architecture_scope_patterns, expected)
        self.assertEqual(policy.architecture_scope_mode, "enforce")
        self.assertEqual(policy.architecture_accepted_issue_label, "accepted")
        for pattern in expected:
            path = "crates/server-core/src/session.rs" if pattern.endswith("/**") else pattern
            with self.subTest(pattern=pattern):
                profile = pr_review.architecture_scope_profile(
                    {"author": {"login": "author"}}, [path], policy, {}
                )
                self.assertTrue(profile["triggered"])
                self.assertEqual(profile["evidence"]["matched_paths"], [path])

    def test_enforce_policy_requires_valid_cutoff_but_audit_allows_empty(self) -> None:
        pr_review.validate_policy(
            pr_review.Policy(
                {"admission": {"mode": "audit", "enforced_after": ""}}
            )
        )
        for value in ("", "not-a-date", "2026-07-10T00:00:00+00:00"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "requires a valid"):
                    pr_review.validate_policy(
                        pr_review.Policy(
                            {"admission": {"mode": "enforce", "enforced_after": value}}
                        )
                    )

    def test_artifact_rejects_duplicate_headings_empty_evidence_and_backend_not_applicable(self) -> None:
        head = "a" * 40
        policy = pr_review.Policy({"admission": {"mode": "audit"}})
        body = self._artifact_body(head).replace(
            "## Gate A\n", "## Gate A\nGate A PASS head=" + head + " base=" + "b" * 40 + "\n\n## Gate A\n", 1
        )
        duplicate = pr_review.artifact_profile({"headRefOid": head, "body": body}, policy)
        self.assertIn("duplicate_required_h2_headings", duplicate["failures"])

        verification = "\n".join(
            f"- [x] {label}" for label in pr_review.REQUIRED_VERIFICATION_CHECKBOX_LABELS
        ) + "\n- `cargo test` — clean"
        empty_body = self._artifact_body(head).replace(
            f"## Verification\n{verification}\n\n", "## Verification\n\n"
        )
        empty = pr_review.artifact_profile({"headRefOid": head, "body": empty_body}, policy)
        self.assertIn("missing_or_empty_verification", empty["failures"])

        not_applicable = self._artifact_body(head).replace(
            "Method: /engine-implementer", "Method: not-applicable — small change"
        )
        backend = pr_review.artifact_profile(
            {"headRefOid": head, "body": not_applicable},
            policy,
            ["crates/engine/src/parser/oracle.rs"],
            {"path_classes": {"engine": ["crates/engine/src/parser/oracle.rs"]}},
        )
        self.assertIn("invalid_implementation_method", backend["failures"])

    def test_artifact_requires_exact_h2_single_pass_method_and_fixed_checkboxes(self) -> None:
        head = "a" * 40
        policy = pr_review.Policy({"admission": {"mode": "audit"}})
        body = self._artifact_body(head)

        h3 = pr_review.artifact_profile(
            {"headRefOid": head, "body": body.replace("## Gate A", "### Gate A")},
            policy,
        )
        self.assertIn("missing_required_h2_headings", h3["failures"])
        self.assertIn("missing_gate_a_pass", h3["failures"])

        duplicate_pass = pr_review.artifact_profile(
            {
                "headRefOid": head,
                "body": body.replace(
                    f"Gate A PASS head={head} base={'b' * 40}",
                    f"Gate A PASS head={head} base={'b' * 40}\n"
                    f"Gate A PASS head={head} base={'c' * 40}",
                ),
            },
            policy,
        )
        self.assertIn("duplicate_gate_a_pass", duplicate_pass["failures"])

        both_methods = pr_review.artifact_profile(
            {
                "headRefOid": head,
                "body": body.replace(
                    "Method: /engine-implementer",
                    "Method: /engine-implementer\nMethod: not-applicable — docs only",
                ),
            },
            policy,
        )
        self.assertIn("invalid_implementation_method", both_methods["failures"])

        verification = "\n".join(
            f"- [x] {label}" for label in pr_review.REQUIRED_VERIFICATION_CHECKBOX_LABELS
        ) + "\n- `cargo test` — clean"
        arbitrary = pr_review.artifact_profile(
            {
                "headRefOid": head,
                "body": body.replace(verification, "- [x] `cargo test` — clean"),
            },
            policy,
        )
        self.assertIn("invalid_required_verification_checkboxes", arbitrary["failures"])

    def test_unknown_created_at_is_artifact_audit_evidence_or_enforcement_hold(self) -> None:
        head = "a" * 40
        audit_policy = pr_review.Policy({"admission": {"mode": "audit"}})
        audit = pr_review.artifact_profile(
            {"headRefOid": head, "body": self._artifact_body(head)}, audit_policy
        )
        self.assertTrue(audit["insufficient_admission_data"])
        self.assertFalse(audit["hold"])
        self.assertFalse(audit["decline"])

        enforce_policy = pr_review.Policy(
            {"admission": {"mode": "enforce", "enforced_after": "2026-07-10T00:00:00Z"}}
        )
        for created_at in (None, "malformed"):
            with self.subTest(created_at=created_at):
                pr = {
                    "headRefOid": head,
                    "body": self._artifact_body(head),
                    "createdAt": created_at,
                    "author": {"login": "contrib"},
                }
                artifact = pr_review.artifact_profile(pr, enforce_policy)
                self.assertTrue(artifact["hold"])
                self.assertFalse(artifact["decline"])
                recommendation = pr_review.recommend_from_packet(
                    {
                        "pr": {**pr, "number": 1, "state": "OPEN"},
                        "classification": {"hard_stop_paths": [], "surface": "backend"},
                        "artifacts": artifact,
                        "architecture_scope": {
                            "would_decline": False,
                            "decline": False,
                            "hold": False,
                        },
                        "ci": {"state": "green"},
                        "policy_trace": [],
                    }
                )
                self.assertEqual(recommendation["advisory_action"], "hold")
                self.assertEqual(recommendation["reason"], "insufficient_admission_data")
                self.assertFalse(
                    recommendation["hold_evidence"]["implementation_diff_review_allowed"]
                )

    def test_architecture_enforcement_is_independent_of_artifact_cutoff(self) -> None:
        policy = pr_review.Policy(
            {
                "admission": {"mode": "audit", "enforced_after": ""},
                "architecture_scope": {
                    "mode": "enforce",
                    "patterns": ["central.rs"],
                },
            }
        )
        pr = {
            "number": 1,
            "state": "OPEN",
            "headRefOid": "h",
            "author": {"login": "contrib"},
            "closingIssuesReferences": [],
            "closingIssuesReferencesComplete": True,
        }
        artifact = pr_review.artifact_profile(pr, policy)
        architecture = pr_review.architecture_scope_profile(
            pr, ["central.rs"], policy, {}
        )

        self.assertFalse(artifact["enforced"])
        self.assertTrue(artifact["would_decline"])
        self.assertFalse(artifact["decline"])
        self.assertTrue(architecture["enforced"])
        self.assertIsNone(architecture["enforcement_cutoff"])
        self.assertIsNone(architecture["post_cutoff"])
        self.assertTrue(architecture["decline"])

        recommendation = pr_review.recommend_from_packet(
            {
                "pr": pr,
                "classification": {"hard_stop_paths": [], "surface": "backend"},
                "artifacts": artifact,
                "architecture_scope": architecture,
                "ci": {"state": "green"},
                "policy_trace": [],
            }
        )
        self.assertEqual(recommendation["advisory_action"], "decline")
        self.assertEqual(recommendation["reason"], "architecture_scope_not_authorized")

    def test_architecture_enforcement_holds_when_closing_issue_evidence_is_incomplete(self) -> None:
        policy = pr_review.Policy(
            {
                "admission": {"accepted_issue_label": "accepted"},
                "architecture_scope": {
                    "mode": "enforce",
                    "patterns": ["central.rs"],
                },
            }
        )
        profile = pr_review.architecture_scope_profile(
            {
                "author": {"login": "contrib"},
                "closingIssuesReferences": [],
                "closingIssuesReferencesComplete": False,
            },
            ["central.rs"],
            policy,
            {},
        )

        self.assertTrue(profile["hold"])
        self.assertFalse(profile["decline"])

    def test_architecture_scope_uses_disjoint_spans_not_file_count(self) -> None:
        policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
        two_spans = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            ["crates/engine/src/game/casting.rs", "client/src/App.tsx"],
            policy,
            {},
        )
        three_spans = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            [
                "crates/engine/src/game/casting.rs",
                "client/src/App.tsx",
                "crates/draft-core/src/cube.rs",
            ],
            policy,
            {},
        )
        many_files = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            [f"crates/engine/src/parser/file_{index}.rs" for index in range(20)],
            policy,
            {},
        )
        self.assertFalse(two_spans["triggered"])
        self.assertTrue(three_spans["triggered"])
        self.assertFalse(many_files["triggered"])

    def test_known_pr_path_shapes_trigger_5618_but_pass_5552_and_5610(self) -> None:
        policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
        trigger_5618 = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            [
                "crates/server-core/src/session.rs",
                "crates/engine/src/game/deck_loading.rs",
                "client/src/services/deckParser.ts",
            ],
            policy,
            {},
        )
        pass_5552 = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            [
                "crates/engine/src/game/casting.rs",
                "crates/engine/src/types/ability.rs",
                "crates/phase-ai/src/policies/payment_selection.rs",
                "crates/mtgish-import/src/convert/action.rs",
            ],
            policy,
            {},
        )
        pass_5610 = pr_review.architecture_scope_profile(
            {"author": {"login": "author"}},
            [
                "crates/engine/src/parser/oracle_effect/mod.rs",
                "crates/engine/src/parser/oracle_effect/tests.rs",
            ],
            policy,
            {},
        )
        self.assertTrue(trigger_5618["triggered"])
        self.assertFalse(pass_5552["triggered"])
        self.assertFalse(pass_5610["triggered"])

    def test_architecture_scope_authorizes_only_private_author_or_accepted_closing_issue(self) -> None:
        policy = pr_review.Policy(
            {
                "admission": {"mode": "audit", "accepted_issue_label": "accepted"},
                "architecture_scope": {
                    "patterns": ["central.rs"],
                }
            }
        )
        base_pr = {
            "author": {"login": "contrib"},
            "labels": [{"name": "quality"}],
            "closingIssuesReferences": [],
            "closingIssuesReferencesComplete": True,
        }
        denied = pr_review.architecture_scope_profile(
            base_pr, ["central.rs"], policy, {"frontend_review_authors": ["contrib"]}
        )
        private = pr_review.architecture_scope_profile(
            base_pr, ["central.rs"], policy, {"architecture_scope_authors": ["Contrib"]}
        )
        issue_pr = copy.deepcopy(base_pr)
        issue_pr["closingIssuesReferences"] = [
            {"number": 42, "labels": [{"name": "accepted"}]}
        ]
        issue = pr_review.architecture_scope_profile(issue_pr, ["central.rs"], policy, {})
        incomplete_pr = copy.deepcopy(issue_pr)
        incomplete_pr["closingIssuesReferencesComplete"] = False
        incomplete = pr_review.architecture_scope_profile(
            incomplete_pr, ["central.rs"], policy, {}
        )
        self.assertTrue(denied["would_decline"])
        self.assertFalse(denied["authorized"])
        self.assertTrue(private["authorized"])
        self.assertTrue(issue["authorized"])
        self.assertEqual(issue["evidence"]["accepted_closing_issues"], [42])
        self.assertFalse(incomplete["authorized"])
        self.assertTrue(incomplete["would_decline"])

    def test_audit_reports_would_decline_without_preempting_review(self) -> None:
        packet = {
            "pr": {"number": 1, "state": "OPEN", "headRefOid": "h", "reviewDecision": None},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "artifacts": {"would_decline": True, "decline": False, "failures": ["missing"]},
            "architecture_scope": {"would_decline": False, "decline": False},
            "ci": {"state": "green"},
            "policy_trace": [],
        }
        result = pr_review.recommend_from_packet(packet)
        self.assertEqual(result["advisory_action"], "review")
        self.assertEqual(result["audit_would_decline"][0]["gate"], "artifacts")

    def test_enforced_artifact_and_scope_declines_preempt_standing_and_queue(self) -> None:
        base = {
            "pr": {
                "number": 1,
                "state": "OPEN",
                "headRefOid": "h",
                "reviewDecision": "APPROVED",
                "isInMergeQueue": True,
            },
            "classification": {"hard_stop_paths": [], "surface": "frontend"},
            "artifacts": {"would_decline": True, "decline": True, "failures": ["missing"]},
            "architecture_scope": {"would_decline": True, "decline": True, "decline_comment": "scope"},
            "contributor": {"standing": "skip"},
            "author_policy": {"frontend_review_allowed": True},
            "ci": {"state": "green"},
            "policy_trace": [],
        }
        artifact = pr_review.recommend_from_packet(base)
        self.assertEqual(artifact["advisory_action"], "decline")
        self.assertEqual(artifact["reason"], "required_artifacts_current_head")
        self.assertIn("Closed without implementation-diff review", artifact["decline_comment"])
        self.assertIn("fresh PR from current `main`", artifact["decline_comment"])
        self.assertIn("Merely including", artifact["decline_comment"])
        scope_packet = copy.deepcopy(base)
        scope_packet["artifacts"] = {"would_decline": False, "decline": False}
        scope = pr_review.recommend_from_packet(scope_packet)
        self.assertEqual(scope["advisory_action"], "decline")
        self.assertEqual(scope["reason"], "architecture_scope_not_authorized")
        self.assertEqual(scope["decline_comment"], "scope")

    def test_review_correction_tombstones_exact_signal_subset_everywhere(self) -> None:
        target = {
            "event_type": "review",
            "event_id": "target",
            "timestamp": self._days_ago(2),
            "pr": 7,
            "head_sha": "head",
            "author": "Contrib",
            "signals": ["wrong-seam", "false-green"],
        }
        correction = {
            "event_type": "review_correction",
            "event_id": "correction",
            "timestamp": self._days_ago(1),
            "pr": 7,
            "head_sha": "head",
            "author": "contrib",
            "corrects_event_id": "target",
            "signals": ["wrong-seam"],
        }
        events = [target, correction]
        self.assertIsNone(pr_review.event_validation_error(correction, [target]))
        self.assertIsNone(pr_review.event_validation_error(correction, events))
        duplicate_correction = dict(
            correction,
            event_id="correction-2",
            timestamp=self._minutes_ago(1),
        )
        self.assertIn(
            "already-retracted",
            pr_review.event_validation_error(duplicate_correction, events),
        )
        self.assertEqual(pr_review.effective_signals_by_event(events)["target"], ["false-green"])
        self.assertEqual(pr_review.latest_events_by_pr_head(events)[(7, "head")]["event_id"], "target")
        model = pr_review.build_analytics_model(
            events, days=None, author=None, min_prs=1, include_open=True
        )
        self.assertEqual(model["contributors"][0]["quality_signals"], {"false-green": 1})
        occurrences = pr_review.collect_signal_occurrences(events)["contrib"]
        self.assertEqual([entry["signal"] for entry in occurrences], ["false-green"])
        with tempfile.TemporaryDirectory() as temp:
            state_dir = Path(temp)
            self.assertTrue(pr_review.append_event(state_dir, target))
            self.assertTrue(pr_review.append_event(state_dir, correction))
            self.assertFalse(pr_review.append_event(state_dir, correction))
            args = type("Args", (), {"state_dir": state_dir, "days": None})()
            with contextlib.redirect_stdout(io.StringIO()):
                pr_review.command_compact(args)
            compact = json.loads((state_dir / "review-summary.json").read_text())
            self.assertEqual(compact["contributors"][0]["signals"], {"false-green": 1})
            self.assertEqual(compact["prs"][0]["latest_event"], "review")

    def test_review_correction_rejects_wrong_identity_and_non_subset(self) -> None:
        target = {
            "event_type": "review",
            "event_id": "target",
            "pr": 7,
            "head_sha": "head",
            "author": "contrib",
            "signals": ["wrong-seam"],
        }
        wrong_author = {
            "event_type": "review_correction",
            "pr": 7,
            "head_sha": "head",
            "author": "other",
            "corrects_event_id": "target",
            "signals": ["wrong-seam"],
        }
        wrong_signal = dict(wrong_author, author="contrib", signals=["false-green"])
        self.assertIn("author", pr_review.event_validation_error(wrong_author, [target]))
        self.assertIn("subset", pr_review.event_validation_error(wrong_signal, [target]))

    def test_new_defect_tokens_and_workflow_parent_contract(self) -> None:
        self.assertIn("wrong-or-stale-cr-annotation", pr_review.DEFECT_SIGNAL_WEIGHTS)
        self.assertIn("duplicated-domain-vocabulary", pr_review.DEFECT_SIGNAL_WEIGHTS)
        workflow = (pr_review.REPO_ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("fetch-depth: 2", workflow)
        parse_step = workflow.split("- name: Parse-detail diff vs base baseline", 1)[1]
        self.assertNotIn("PAYLOAD_BASE_SHA", parse_step)

    def test_gate_a_actual_success_output_is_sha_bound(self) -> None:
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=pr_review.REPO_ROOT,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
        result = subprocess.run(
            [str(pr_review.REPO_ROOT / "scripts/check-parser-combinators.sh"), head],
            cwd=pr_review.REPO_ROOT,
            check=True,
            text=True,
            capture_output=True,
        )
        self.assertRegex(
            result.stdout.strip(),
            rf"^Gate A PASS head={re.escape(head)} base={re.escape(head)}$",
        )

    def test_parse_diff_base_selects_non_head_parent_and_never_falls_back(self) -> None:
        script = pr_review.REPO_ROOT / "scripts/parse-diff-base.sh"
        env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "Test",
            "GIT_AUTHOR_EMAIL": "test@example.com",
            "GIT_COMMITTER_NAME": "Test",
            "GIT_COMMITTER_EMAIL": "test@example.com",
        }
        with tempfile.TemporaryDirectory() as temp:
            repo = Path(temp)
            subprocess.run(["git", "init", "-q"], cwd=repo, check=True, env=env)

            def git(*args: str, input_text: str | None = None) -> str:
                return subprocess.run(
                    ["git", *args],
                    cwd=repo,
                    check=True,
                    env=env,
                    text=True,
                    input=input_text,
                    capture_output=True,
                ).stdout.strip()

            tree = git("mktree", input_text="")
            root = git("commit-tree", tree, input_text="root\n")
            parent_one = git("commit-tree", tree, "-p", root, input_text="one\n")
            parent_two = git("commit-tree", tree, "-p", root, input_text="two\n")
            merge = git(
                "commit-tree",
                tree,
                "-p",
                parent_one,
                "-p",
                parent_two,
                input_text="merge\n",
            )

            for head, expected in ((parent_one, parent_two), (parent_two, parent_one)):
                selected = subprocess.run(
                    [str(script), head, merge],
                    cwd=repo,
                    check=True,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(selected.stdout.strip(), expected)

            neither = subprocess.run(
                [str(script), root, merge],
                cwd=repo,
                text=True,
                capture_output=True,
            )
            missing_parent = subprocess.run(
                [str(script), parent_one, parent_one],
                cwd=repo,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(neither.returncode, 0)
            self.assertNotEqual(missing_parent.returncode, 0)
            self.assertNotIn("payload", neither.stdout.casefold())
            self.assertNotIn("payload", missing_parent.stdout.casefold())

    def test_normalize_graphql_pr_preserves_complete_closing_issue_labels(self) -> None:
        normalized = pr_review.normalize_graphql_pr(
            {
                "closingIssuesReferences": {
                    "totalCount": 2,
                    "pageInfo": {"hasNextPage": False, "endCursor": "issue"},
                    "nodes": [
                        {
                            "number": 9,
                            "state": "CLOSED",
                            "labels": {
                                "totalCount": 3,
                                "pageInfo": {"hasNextPage": False, "endCursor": "label"},
                                "nodes": [
                                    {"name": "engine"},
                                    {"name": "accepted"},
                                    {"name": "accepted"},
                                ],
                            },
                        },
                        {
                            "number": 9,
                            "state": "CLOSED",
                            "labels": {
                                "totalCount": 3,
                                "pageInfo": {"hasNextPage": False, "endCursor": "label"},
                                "nodes": [
                                    {"name": "accepted"},
                                    {"name": "engine"},
                                    {"name": "accepted"},
                                ],
                            },
                        },
                    ]
                },
                "labels": {
                    "totalCount": 1,
                    "pageInfo": {"hasNextPage": False, "endCursor": "pr-label"},
                    "nodes": [{"name": "bug"}],
                },
                "files": {
                    "totalCount": 1,
                    "pageInfo": {"hasNextPage": False, "endCursor": "file"},
                    "nodes": [{"path": "crates/engine/src/lib.rs"}],
                },
            }
        )
        self.assertEqual(
            normalized["closingIssuesReferences"][0]["labels"],
            [{"name": "accepted"}, {"name": "engine"}],
        )
        self.assertTrue(normalized["closingIssuesReferencesComplete"])
        self.assertTrue(normalized["closingIssuesReferences"][0]["labelsComplete"])
        self.assertTrue(normalized["labelsComplete"])
        self.assertTrue(normalized["filesComplete"])
        self.assertEqual(normalized["closingIssuesReferencesCount"], 2)
        self.assertEqual(len(normalized["closingIssuesReferences"]), 1)
        policy = pr_review.Policy(
            {
                "admission": {"accepted_issue_label": "accepted"},
                "architecture_scope": {"patterns": ["central.rs"]},
            }
        )
        normalized["author"] = {"login": "contrib"}
        closed_accepted = pr_review.architecture_scope_profile(
            normalized, ["central.rs"], policy, {}
        )
        self.assertTrue(closed_accepted["authorized"])

        twenty_one = {
            "closingIssuesReferences": {
                "totalCount": 21,
                "pageInfo": {"hasNextPage": True, "endCursor": "issue"},
                "nodes": [
                    {
                        "number": number,
                        "state": "OPEN",
                        "labels": {
                            "totalCount": 1,
                            "pageInfo": {"hasNextPage": False, "endCursor": "label"},
                            "nodes": [{"name": "accepted"}],
                        },
                    }
                    for number in range(1, 21)
                ],
            }
        }
        incomplete = pr_review.normalize_graphql_pr(twenty_one)
        self.assertFalse(incomplete["closingIssuesReferencesComplete"])

        incomplete_labels = pr_review.normalize_graphql_pr(
            {
                "closingIssuesReferences": {
                    "totalCount": 1,
                    "pageInfo": {"hasNextPage": False, "endCursor": "issue"},
                    "nodes": [
                        {
                            "number": 1,
                            "state": "OPEN",
                            "labels": {
                                "totalCount": 2,
                                "pageInfo": {"hasNextPage": True, "endCursor": "label"},
                                "nodes": [{"name": "accepted"}],
                            },
                        }
                    ],
                }
            }
        )
        self.assertFalse(incomplete_labels["closingIssuesReferencesComplete"])


if __name__ == "__main__":
    unittest.main()
