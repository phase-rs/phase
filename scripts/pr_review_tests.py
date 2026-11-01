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
from unittest import mock
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pr_review
import pr_review_dashboard


class PrReviewTests(unittest.TestCase):
    def test_scan_candidate_keeps_github_url_for_dashboard_navigation(self) -> None:
        context = type(
            "DashboardContext",
            (),
            {
                "local_latest_events": {},
                "local_latest_observations": {},
                "local_latest_looks": {},
                "local_latest_actions": {},
            },
        )()
        candidate = pr_review.scan_candidate(
            {
                "number": 44,
                "title": "Useful change",
                "url": "https://github.com/phase-rs/phase/pull/44",
                "headRefOid": "head",
            },
            {
                "pr": {"author_login": "author", "self_authored": False},
                "classification": {"surface": "backend", "gate": "review", "hard_stop_paths": []},
                "ci": {"state": "green"},
                "parse_diff": {},
                "recommendation": {"advisory_action": "review", "reason": "unreviewed"},
                "policy_trace": [],
            },
            context,
        )

        self.assertEqual(candidate["url"], "https://github.com/phase-rs/phase/pull/44")

    def test_dashboard_history_separates_observations_from_material_actions(self) -> None:
        events = [
            {
                "event_type": "review",
                "pr": 52,
                "head_sha": "old-head",
                "timestamp": "2026-07-10T00:00:00Z",
                "summary": "Requested a regression test",
            },
            {
                "event_type": "observation",
                "pr": 52,
                "head_sha": "current-head",
                "timestamp": "2026-07-11T00:00:00Z",
                "summary": "Confirmed the author follow-up",
            },
        ]
        context = type(
            "DashboardContext",
            (),
            {
                "local_latest_events": pr_review.latest_events_by_pr(events),
                "local_latest_observations": pr_review.latest_observations_by_pr(events),
                "local_latest_looks": pr_review.latest_looks_by_pr(events),
                "local_latest_actions": pr_review.latest_material_actions_by_pr(events),
            },
        )()

        history = pr_review.dashboard_local_history(context, 52, "current-head")

        self.assertEqual(
            history["last_recorded_observation"]["summary"],
            "Confirmed the author follow-up",
        )
        self.assertEqual(
            history["last_recorded_look"]["summary"],
            "Confirmed the author follow-up",
        )
        self.assertEqual(
            history["last_material_action"]["summary"],
            "Requested a regression test",
        )
        self.assertFalse(history["last_material_action"]["head_matches_current"])

    def test_dashboard_material_action_also_counts_as_a_recorded_look(self) -> None:
        events = [
            {
                "event_type": "blocked",
                "pr": 52,
                "head_sha": "current-head",
                "timestamp": "2026-07-11T00:00:00Z",
                "summary": "Waiting for a rules fix",
            }
        ]
        context = type(
            "DashboardContext",
            (),
            {
                "local_latest_events": pr_review.latest_events_by_pr(events),
                "local_latest_observations": pr_review.latest_observations_by_pr(events),
                "local_latest_looks": pr_review.latest_looks_by_pr(events),
                "local_latest_actions": pr_review.latest_material_actions_by_pr(events),
            },
        )()

        history = pr_review.dashboard_local_history(context, 52, "current-head")

        self.assertIsNone(history["last_recorded_observation"])
        self.assertEqual(history["last_recorded_look"]["event_type"], "blocked")
        self.assertEqual(
            history["last_recorded_look"]["summary"],
            "Waiting for a rules fix",
        )

    def test_observation_does_not_replace_current_head_hold_or_review_label(self) -> None:
        held = {
            "event_type": "held",
            "outcome": "held",
            "pr": 52,
            "head_sha": "current-head",
            "timestamp": "2026-07-11T00:00:00Z",
            "review_routing_label": "pr:approved-for-review",
        }
        observation = {
            "event_type": "observation",
            "pr": 52,
            "head_sha": "current-head",
            "timestamp": "2026-07-11T01:00:00Z",
        }
        current = pr_review.latest_events_by_pr_head([held, observation])
        self.assertIs(current[(52, "current-head")], held)
        self.assertIs(pr_review.latest_events_by_pr([held, observation])[52], observation)

        recommendation = pr_review.recommend_from_packet(
            {
                "pr": {
                    "number": 52,
                    "state": "OPEN",
                    "headRefOid": "current-head",
                    "reviewDecision": "CHANGES_REQUESTED",
                    "labels": ["pr:approved-for-review"],
                },
                "classification": {"hard_stop_paths": [], "surface": "backend"},
                "ci": {"state": "failed"},
                "parse_diff": {"state": "baseline_pending"},
                "policy": {"labels": {"approved_for_review": "pr:approved-for-review"}},
                "local_current_event": current[(52, "current-head")],
            }
        )
        self.assertEqual(recommendation["advisory_action"], "hold_ci")

        new_head = pr_review.latest_events_by_pr_head(
            [held, {**observation, "head_sha": "new-head"}]
        )
        self.assertNotIn((52, "new-head"), new_head)

    def test_dashboard_terminal_sections_keep_old_closed_prs_in_archive(self) -> None:
        reference = datetime(2026, 7, 15, tzinfo=UTC)
        sections = pr_review.dashboard_terminal_sections(
            [
                {"pr": 1, "state": "CLOSED", "closed_at": "2026-07-14T00:00:00Z"},
                {"pr": 2, "state": "CLOSED", "closed_at": "2026-07-10T00:00:00Z"},
                {"pr": 3, "state": "MERGED", "merged_at": "2026-07-01T00:00:00Z"},
            ],
            reference,
        )

        self.assertEqual([row["pr"] for row in sections["closed_recent"]], [1])
        self.assertEqual([row["pr"] for row in sections["closed_archive"]], [2])
        self.assertEqual([row["pr"] for row in sections["merged"]], [3])

    def test_dashboard_renderer_escapes_snapshot_content_and_auto_refreshes(self) -> None:
        rendered = pr_review_dashboard.render_dashboard(
            {
                "generated_at": "2026-07-15T00:00:00Z",
                "action_counts": {"review": 1},
                "candidates_by_action": {
                    "review": [
                        {
                            "pr": 44,
                            "title": "<script>alert(1)</script>",
                            "url": "https://example.test/pull/44",
                            "advisory_action": "review",
                            "reason": "fresh_head",
                            "ci": "green",
                            "local_history": {},
                        }
                    ]
                },
                "dashboard": {"closed_unmerged": {"recent": [], "archive": []}, "merged": []},
            }
        )

        self.assertIn('http-equiv="refresh" content="60"', rendered)
        self.assertIn("&lt;script&gt;alert(1)&lt;/script&gt;", rendered)
        self.assertNotIn("<script>alert(1)</script>", rendered)
        self.assertIn("<table>", rendered)
        self.assertIn('data-detail-target="open-details-44"', rendered)
        self.assertIn('id="pr-review-dashboard"', rendered)
        self.assertIn('href="https://example.test/pull/44"', rendered)
        self.assertIn('target="_blank"', rendered)
        self.assertIn('class="status-label ready"', rendered)
        self.assertNotIn('class="badge ready"', rendered)
        self.assertIn('aria-label="CI passing"', rendered)
        self.assertIn(">✓</span>", rendered)
        self.assertIn('id="pr-search"', rendered)
        self.assertIn('data-status-filter="review"', rendered)
        self.assertIn('id="ci-filter"', rendered)
        self.assertIn('const syncFilterUrl', rendered)

    def test_dashboard_data_updates_terminal_archive_and_removes_reopened_prs(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "review-dashboard.json"
            pr_review.write_json_atomically(
                output,
                {
                    "terminal_archive": [
                        {"pr": 7, "state": "CLOSED", "closed_at": "2026-07-01T00:00:00Z"}
                    ]
                },
            )
            args = type(
                "DashboardArgs",
                (),
                {
                    "output": output,
                    "html_output": None,
                    "state_dir": Path(temp),
                    "repo": "phase-rs/phase",
                    "terminal_limit": 200,
                    "limit": 100,
                },
            )()
            scan_output = {
                "generated_at": "2026-07-15T00:00:00Z",
                "candidates_by_action": {"review": [{"pr": 7}]},
            }
            terminal_row = {
                "pr": 8,
                "state": "CLOSED",
                "closed_at": pr_review.now_iso(),
            }
            context = type(
                "DashboardContext",
                (),
                {
                    "local_latest_events": {},
                    "local_latest_observations": {},
                    "local_latest_looks": {},
                    "local_latest_actions": {},
                },
            )()
            with (
                mock.patch.object(pr_review, "load_review_context", return_value=context),
                mock.patch.object(pr_review, "build_scan_output", return_value=scan_output),
                mock.patch.object(pr_review, "complete_open_nodes", return_value=[{"number": 7, "state": "OPEN", "headRefOid": "head"}]),
                mock.patch.object(pr_review, "fetch_terminal_prs", return_value=[{"number": 8}]),
                mock.patch.object(pr_review, "dashboard_terminal_row", return_value=terminal_row),
            ):
                self.assertEqual(pr_review.command_dashboard_data(args), 0)

            snapshot = json.loads(output.read_text())
            self.assertEqual([row["pr"] for row in snapshot["terminal_archive"]], [8])
            self.assertEqual([row["pr"] for row in snapshot["dashboard"]["closed_unmerged"]["recent"]], [8])
            self.assertTrue(output.with_suffix(".html").exists())

    def test_gh_user_uses_graphql_viewer_query(self) -> None:
        with mock.patch.object(
            pr_review,
            "run_json",
            return_value={"data": {"viewer": {"login": "maintainer"}}},
        ) as run_json:
            self.assertEqual(pr_review.gh_user(), "maintainer")

        self.assertEqual(
            run_json.call_args.args[0],
            [
                "gh",
                "api",
                "graphql",
                "-f",
                "query=query { viewer { login } }",
            ],
        )

    def test_required_status_checks_use_effective_graphql_branch_rule(self) -> None:
        with mock.patch.object(
            pr_review,
            "run_json",
            return_value={
                "data": {
                    "repository": {
                        "ref": {
                            "branchProtectionRule": {
                                "requiredStatusCheckContexts": ["Rust", "Frontend"]
                            }
                        }
                    }
                }
            },
        ) as run_json:
            self.assertEqual(
                pr_review.required_status_check_names("phase-rs/phase", "main"),
                {"Rust", "Frontend"},
            )

        command = run_json.call_args.args[0]
        self.assertIn("graphql", command)
        self.assertIn("qualifiedName=refs/heads/main", command)
        self.assertNotIn("protection/required_status_checks", command)

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

    def test_approved_for_review_label_forces_one_review_before_hard_stop(self) -> None:
        packet = {
            "pr": {
                "number": 7029,
                "state": "OPEN",
                "headRefOid": "head",
                "labels": ["pr:approved-for-review"],
                "self_authored": False,
            },
            "classification": {
                "hard_stop_paths": [".github/workflows/ci.yml"],
                "surface": "hard_stop",
            },
            "policy": {
                "labels": {"approved_for_review": "pr:approved-for-review"}
            },
            "ci": {"state": "green"},
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "approved_for_review_label")

        packet["local_current_event"] = {
            "event_type": "changes_requested",
            "review_routing_label": "pr:approved-for-review",
        }
        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "request_changes")
        self.assertEqual(recommendation["reason"], "hard_stop")

    def test_approved_for_review_label_yields_to_self_review_and_admission_gates(
        self,
    ) -> None:
        base = {
            "pr": {
                "number": 7030,
                "state": "OPEN",
                "headRefOid": "head",
                "labels": ["pr:approved-for-review"],
                "self_authored": False,
            },
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "policy": {"labels": {"approved_for_review": "pr:approved-for-review"}},
            "ci": {"state": "green"},
        }

        # Positive control: without a competing gate this packet does reach review,
        # so a non-review result below is the gate winning, not a malformed packet.
        self.assertEqual(
            pr_review.recommend_from_packet(base)["advisory_action"], "review"
        )

        recommendation = pr_review.recommend_from_packet(
            {**base, "pr": {**base["pr"], "self_authored": True}}
        )
        self.assertEqual(recommendation["advisory_action"], "skip")
        self.assertEqual(recommendation["reason"], "self_authored")

        for key, profile, action, reason in (
            ("artifacts", {"hold": True}, "hold", "insufficient_admission_data"),
            (
                "artifacts",
                {"decline": True},
                "decline",
                "required_artifacts_current_head",
            ),
            (
                "architecture_scope",
                {"decline": True},
                "decline",
                "architecture_scope_not_authorized",
            ),
        ):
            with self.subTest(gate=f"{key}:{sorted(profile)[0]}"):
                recommendation = pr_review.recommend_from_packet(
                    {**base, key: profile}
                )

                self.assertEqual(recommendation["advisory_action"], action)
                self.assertEqual(recommendation["reason"], reason)

    def test_packet_exposes_review_labels_from_policy(self) -> None:
        policy = pr_review.Policy(
            {
                "labels": {
                    "quality": "quality",
                    "approved_for_review": "pr:approved-for-review",
                }
            }
        )
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
        self.assertEqual(
            packet["policy"]["labels"]["approved_for_review"],
            "pr:approved-for-review",
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

        packet["freshness"] = pr_review.review_freshness(
            packet["pr"], packet["acting_login"], None, None
        )
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

        packet["freshness"] = pr_review.review_freshness(
            packet["pr"], packet["acting_login"], None, None
        )
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

    def test_current_head_bare_hold_is_honored(self) -> None:
        # A recorded hold on the current head with nothing new — green CI, no
        # author follow-up, no parse-diff — is honored rather than re-reviewed.
        # (Resurfacing on a real change is covered by the parse-diff and
        # head-change tests below.)
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

        self.assertEqual(recommendation["advisory_action"], "hold")
        self.assertEqual(recommendation["reason"], "local_hold_current_head")

    def test_ci_hold_rechecks_after_required_ci_settles(self) -> None:
        packet = {
            "pr": {
                "number": 4576,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
            },
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "held",
                "outcome": "hold_ci",
                "head_sha": "head",
            },
            "policy_trace": [],
        }

        for ci_state in ("green", "failed"):
            with self.subTest(ci_state=ci_state):
                recommendation = pr_review.recommend_from_packet(
                    {**packet, "ci": {"state": ci_state}}
                )

                self.assertEqual(
                    recommendation["advisory_action"], "recheck_ci_hold_for_handler"
                )
                self.assertEqual(recommendation["reason"], "ci_hold_settled")

    def test_ci_hold_remains_held_while_required_ci_is_pending(self) -> None:
        packet = {
            "pr": {
                "number": 4577,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
            },
            "ci": {"state": "pending"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "held",
                "outcome": "hold_ci",
                "head_sha": "head",
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "hold_ci")
        self.assertEqual(recommendation["reason"], "local_hold_current_head")

    def test_ci_hold_recheck_does_not_hide_a_current_head_conflict(self) -> None:
        packet = {
            "pr": {
                "number": 4578,
                "state": "OPEN",
                "headRefOid": "head",
                "reviewDecision": "",
                "isInMergeQueue": False,
                "mergeStateStatus": "DIRTY",
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": None,
            "local_current_event": {
                "event_type": "held",
                "outcome": "hold_ci",
                "head_sha": "head",
            },
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "blocked")
        self.assertEqual(recommendation["reason"], "local_hold_current_head")

    def test_bare_local_hold_resurfaces_on_parse_diff(self) -> None:
        # A parse-diff update landing after the hold re-surfaces the same head.
        packet = {
            "pr": {
                "number": 4575,
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
                "timestamp": self._minutes_ago(5),
            },
            "parse_diff": {"updated_at": self._minutes_ago(1)},
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "parse_diff_after_local_hold")

    def test_member_commented_last_helper(self) -> None:
        # A review outranks an older comment; a member review last -> True.
        self.assertTrue(
            pr_review.member_commented_last(
                {
                    "comments": [
                        {"authorAssociation": "CONTRIBUTOR", "createdAt": self._days_ago(1)},
                    ],
                    "reviews": [
                        {"authorAssociation": "MEMBER", "submittedAt": self._minutes_ago(1)},
                    ],
                }
            )
        )
        # Missing authorAssociation (e.g. a bot or cached payload) is not a
        # member, so the guard never suppresses on absent data.
        self.assertFalse(
            pr_review.member_commented_last(
                {
                    "comments": [
                        {"createdAt": self._minutes_ago(1)},
                    ],
                    "reviews": [],
                }
            )
        )
        # No dated activity at all -> False.
        self.assertFalse(pr_review.member_commented_last({"comments": [], "reviews": []}))

    def test_member_commented_last_holds_redundant_review(self) -> None:
        # No state trigger fires and a repo member spoke last -> hold, so the
        # sweep does not dispatch a redundant review of the same head.
        packet = {
            "pr": {
                "number": 6262,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "",
                "isInMergeQueue": False,
                "commentsComplete": True,
                "comments": [
                    {
                        "author": "contributor",
                        "authorAssociation": "CONTRIBUTOR",
                        "createdAt": self._days_ago(2),
                        "updatedAt": self._days_ago(2),
                    },
                    {
                        "author": "maintainer",
                        "authorAssociation": "MEMBER",
                        "createdAt": self._minutes_ago(5),
                        "updatedAt": self._minutes_ago(5),
                    },
                ],
                "reviews": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "hold")
        self.assertEqual(
            recommendation["reason"], "redundant_review_member_commented_last"
        )

    def test_contributor_commented_last_still_reviews(self) -> None:
        # The converse of the guard: the contributor spoke last, so there is an
        # unacknowledged follow-up and the PR must still surface for review.
        packet = {
            "pr": {
                "number": 6263,
                "state": "OPEN",
                "headRefOid": "head",
                "author_login": "contributor",
                "reviewDecision": "",
                "isInMergeQueue": False,
                "commentsComplete": True,
                "comments": [
                    {
                        "author": "maintainer",
                        "authorAssociation": "MEMBER",
                        "createdAt": self._days_ago(2),
                        "updatedAt": self._days_ago(2),
                    },
                    {
                        "author": "contributor",
                        "authorAssociation": "CONTRIBUTOR",
                        "createdAt": self._minutes_ago(5),
                        "updatedAt": self._minutes_ago(5),
                    },
                ],
                "reviews": [],
            },
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "head",
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "needs_review")

    def test_member_guard_never_overrides_head_change(self) -> None:
        # A member spoke last, but the head advanced since the recorded event: a
        # mandatory state trigger must win over the redundancy guard.
        previous_event = {
            "event_type": "reviewed",
            "outcome": "reviewed",
            "head_sha": "old-head",
            "timestamp": self._minutes_ago(10),
        }
        pr = {
            "number": 6264,
            "state": "OPEN",
            "headRefOid": "new-head",
            "author_login": "contributor",
            "reviewDecision": "",
            "isInMergeQueue": False,
            "commentsComplete": True,
            "comments": [
                {
                    "author": "maintainer",
                    "authorAssociation": "MEMBER",
                    "createdAt": self._minutes_ago(5),
                    "updatedAt": self._minutes_ago(5),
                },
            ],
            "reviews": [],
        }
        packet = {
            "pr": pr,
            "acting_login": "maintainer",
            "ci": {"state": "green"},
            "classification": {"hard_stop_paths": [], "surface": "backend"},
            "latest_maintainer_review_commit": "old-head",
            "local_current_event": None,
            "freshness": pr_review.review_freshness(pr, "maintainer", None, previous_event),
            "policy_trace": [],
        }

        recommendation = pr_review.recommend_from_packet(packet)

        self.assertEqual(recommendation["advisory_action"], "review")
        self.assertEqual(recommendation["reason"], "head_changed_since_local_event")

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
                "timestamp": self._days_ago(1),
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

    def test_repository_writer_bypasses_contributor_gates(self) -> None:
        enforced_admission = {"mode": "enforce", "enforced_after": "2026-07-10T00:00:00Z"}
        cases = {
            "hard_stop": (
                {"hard_stops": {"patterns": [".github/workflows/**"]}},
                ".github/workflows/ci.yml",
                ("request_changes", "hard_stop"),
            ),
            "architecture_scope": (
                {"architecture_scope": {"mode": "enforce", "patterns": ["crates/server-core/**"]}},
                "crates/server-core/src/lib.rs",
                ("decline", "architecture_scope_not_authorized"),
            ),
            "frontend_policy": (
                {"path_classes": {"frontend": {"patterns": ["client/**"]}}},
                "client/src/App.tsx",
                ("defer", "frontend_policy"),
            ),
            "artifacts": (
                {"admission": enforced_admission},
                "crates/engine/src/lib.rs",
                ("decline", "required_artifacts_current_head"),
            ),
        }
        for gate, (raw_policy, path, contributor_route) in cases.items():
            policy = pr_review.Policy(raw_policy)
            pr = {
                "number": 1,
                "state": "OPEN",
                "createdAt": "2026-08-01T00:00:00Z",
                "headRefOid": "a" * 40,
                "author": {"login": "writer"},
                "closingIssuesReferencesComplete": True,
                "files": [{"path": path}],
                "changedFiles": 1,
            }
            with self.subTest(gate=gate):
                contributor = pr_review.make_packet(pr, policy, "maintainer", "full", {})
                recommendation = contributor["recommendation"]
                self.assertEqual(
                    (recommendation["advisory_action"], recommendation["reason"]),
                    contributor_route,
                )
                writer = pr_review.make_packet(
                    pr, policy, "maintainer", "full", {}, author_is_repository_writer=True
                )
                self.assertTrue(writer["author_policy"]["repository_writer"])
                self.assertEqual(writer["recommendation"]["advisory_action"], "review")
                self.assertNotIn("audit_would_decline", writer["recommendation"])

    def test_repository_writer_logins_folds_pages_and_fails_closed(self) -> None:
        pages = [[{"login": "LGray"}], [{"login": "ntindle"}]]
        with mock.patch.object(pr_review, "run_json", return_value=pages) as run_json:
            self.assertEqual(
                pr_review.repository_writer_logins("phase-rs/phase"),
                frozenset({"lgray", "ntindle"}),
            )
        command = run_json.call_args.args[0]
        self.assertIn("repos/phase-rs/phase/collaborators", command)
        self.assertIn("permission=push", command)

        failure = subprocess.CalledProcessError(1, ["gh"])
        with (
            mock.patch.object(pr_review, "run_json", side_effect=failure),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(pr_review.repository_writer_logins("phase-rs/phase"), frozenset())

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
        self.assertEqual(policy.architecture_scope_mode, "review")
        self.assertEqual(policy.architecture_accepted_issue_label, "accepted")
        for pattern in expected:
            path = "crates/server-core/src/session.rs" if pattern.endswith("/**") else pattern
            with self.subTest(pattern=pattern):
                profile = pr_review.architecture_scope_profile(
                    {"author": {"login": "author"}}, [path], policy, {}
                )
                self.assertTrue(profile["triggered"])
                self.assertTrue(profile["requires_maintainer_review"])
                self.assertFalse(profile["decline"])
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

    def test_architecture_scope_authorizes_private_author_or_accepted_label(self) -> None:
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
        direct_pr = copy.deepcopy(base_pr)
        direct_pr["labels"] = [{"name": "accepted"}]
        direct = pr_review.architecture_scope_profile(direct_pr, ["central.rs"], policy, {})
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
        self.assertTrue(direct["authorized"])
        self.assertTrue(direct["evidence"]["accepted_pr_label"])
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

    def _rev_parse(self, rev: str) -> str:
        return subprocess.run(
            ["git", "rev-parse", rev],
            cwd=pr_review.REPO_ROOT,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()

    def _run_parser_gate(self, base: str) -> "subprocess.CompletedProcess[str]":
        return subprocess.run(
            [str(pr_review.REPO_ROOT / "scripts/check-parser-combinators.sh"), base],
            cwd=pr_review.REPO_ROOT,
            check=False,
            text=True,
            capture_output=True,
        )

    def test_gate_a_actual_success_output_is_sha_bound(self) -> None:
        """A real base..head window prints the SHA-bound PASS evidence line.

        The window is `HEAD~1..HEAD` — a genuine range — rather than `HEAD`
        against itself. `base == head` names an empty range, which the gate now
        refuses to certify (see the companion test below), so it is the wrong
        window to assert success on.

        The match is per-line (`re.MULTILINE`). The gate prints `Gate G` before
        `Gate A`, so a whole-string `^...$` match can never succeed no matter
        what the gate emits; the previous anchoring made this assertion
        unsatisfiable rather than strict.
        """
        head = self._rev_parse("HEAD")
        base = self._rev_parse("HEAD~1")
        result = self._run_parser_gate(base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(
            result.stdout,
            rf"(?m)^Gate A PASS head={re.escape(head)} base={re.escape(base)}$",
        )

    def test_gate_a_pass_line_is_the_line_pr_review_matches(self) -> None:
        """The emitted evidence line is the one the review tool parses.

        Two independent readers of one format drift silently. Asserting the
        real gate output against `pr_review`'s own pattern is what makes the
        format a contract rather than a coincidence.
        """
        head = self._rev_parse("HEAD")
        base = self._rev_parse("HEAD~1")
        result = self._run_parser_gate(base)
        match = re.search(
            r"(?m)^Gate A PASS head=([0-9a-f]{40}) base=([0-9a-f]{40})$",
            result.stdout,
        )
        self.assertIsNotNone(match, result.stdout)
        assert match is not None  # narrowing for type checkers
        self.assertEqual(match.group(1), head)
        self.assertEqual(match.group(2), base)

    def test_gate_a_refuses_to_certify_an_unknowable_window(self) -> None:
        """`base == head` with nothing staged scans zero lines, so it cannot PASS.

        This is the defect the gate change addresses: an empty range plus an
        empty index reads no input, and printing the same green as a real scan
        reports a verdict the run never earned.

        The exit-3 path is reachable only with `GIT_INDEX_FILE` unset — setting
        it selects the pre-commit branch — so this case necessarily reads the
        real index, which belongs to whoever runs the suite. The assertions are
        therefore split: the diagnostic contract is asserted on a clean index,
        and the invariant that holds in EVERY index state is asserted always.
        See the companion test for a deterministic staged scan.
        """
        head = self._rev_parse("HEAD")
        staged = subprocess.run(
            ["git", "diff", "--cached", "--name-only", "--", "crates/engine/src/parser"],
            cwd=pr_review.REPO_ROOT,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
        result = self._run_parser_gate(head)

        # Holds regardless of what the runner has staged: the evidence line is
        # never emitted except on a clean exit. A staged violation exits 1, so
        # requiring exit 0 here would report a false red about the runner's
        # index rather than about the gate.
        if re.search(r"(?m)^Gate A PASS head=", result.stdout):
            self.assertEqual(result.returncode, 0, result.stderr)

        if staged:
            return

        self.assertEqual(result.returncode, 3, result.stdout)
        self.assertNotRegex(result.stdout, r"(?m)^Gate A PASS head=")
        # The diagnostic is the deliverable of exit 3, not a courtesy: a bare
        # non-zero exit would leave the caller unable to act. Assert the parts
        # that make it actionable, on the stream the gate writes them to.
        self.assertIn("Gate A CANNOT ANSWER", result.stderr)
        self.assertIn(head, result.stderr)
        self.assertIn("cannot tell clean parser work from parser work it never saw", result.stderr)
        self.assertIn("scripts/check-parser-combinators.sh", result.stderr)

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


class IncrementalDashboardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.state_dir = Path(self.temp.name)
        self.args = pr_review.build_parser().parse_args([
            "dashboard-data", "--repo", "phase-rs/phase", "--acting-login", "maintainer",
            "--state-dir", str(self.state_dir), "--gittensor-api-url", "",
        ])
        self.nodes = {1: self.node(1)}
        self.issues = []
        self.comments = []
        self.calls = []
        self.clock = "2026-10-06T12:00:00Z"

        class FixedDateTime(datetime):
            @classmethod
            def now(cls, tz=None):
                return datetime.fromisoformat(self.clock).astimezone(tz)

        for patch in (
            mock.patch.object(pr_review, "run_json", side_effect=self.github),
            mock.patch.object(pr_review, "required_checks_by_base", return_value={"main": None}),
            mock.patch.object(pr_review, "now_iso", side_effect=lambda: self.clock),
            mock.patch.object(pr_review, "datetime", FixedDateTime),
        ):
            patch.start()
            self.addCleanup(patch.stop)

    @staticmethod
    def connection(nodes: list[dict], total: int | None = None) -> dict:
        return {"nodes": nodes, "totalCount": len(nodes) if total is None else total,
                "pageInfo": {"hasNextPage": False, "endCursor": None}}

    @classmethod
    def node(cls, number: int) -> dict:
        return {
            "number": number, "state": "OPEN", "title": f"Change {number}",
            "url": f"https://github.com/phase-rs/phase/pull/{number}",
            "author": {"login": "contributor"}, "body": "A focused tooling change.",
            "createdAt": "2026-07-01T00:00:00Z", "updatedAt": "2026-07-02T00:00:00Z",
            "headRefOid": f"head-{number}", "headRefName": "branch", "baseRefName": "main",
            "closedAt": None, "mergedAt": None, "mergeStateStatus": "CLEAN",
            "isDraft": False, "reviewDecision": "REVIEW_REQUIRED", "changedFiles": 1,
            "isInMergeQueue": False, "mergeQueueEntry": None, "autoMergeRequest": None,
            "labels": cls.connection([]), "assignees": cls.connection([]),
            "closingIssuesReferences": cls.connection([]),
            "files": cls.connection([{"path": "scripts/tool.py"}]),
            "comments": cls.connection([]), "reviews": cls.connection([]),
            "latestReviews": cls.connection([]),
            "commits": {"nodes": [{"commit": {
                "oid": f"head-{number}", "messageHeadline": "Focused change", "authors": cls.connection([]),
                "statusCheckRollup": {"state": "SUCCESS", "contexts": cls.connection([
                    {"__typename": "CheckRun", "name": "check", "status": "COMPLETED", "conclusion": "SUCCESS"}
                ])},
            }}]},
        }

    def github(self, command: list[str]) -> object:
        self.calls.append(command)
        endpoint = command[2]
        if endpoint.endswith("/collaborators"):
            return [[]]
        if endpoint != "graphql":
            rows = self.comments if endpoint.endswith("issues/comments") else self.issues
            page = int(next(item.split("=", 1)[1] for item in command if item.startswith("page=")))
            count = int(next(item.split("=", 1)[1] for item in command if item.startswith("per_page=")))
            return copy.deepcopy(rows[(page - 1) * count:page * count])
        query = next(item[6:] for item in command if item.startswith("query="))
        if "pullRequests(" in query:
            states = {"OPEN"} if "states:[OPEN]" in query else {"CLOSED", "MERGED"}
            nodes = sorted((node for node in self.nodes.values() if node["state"] in states),
                           key=lambda node: node["number"], reverse="direction:DESC" in query)
            first = int(next(item.split("=", 1)[1] for item in command if item.startswith("first=")))
            cursor = next((int(item.split("=", 1)[1]) for item in command if item.startswith("after=")), 0)
            chunk = nodes[cursor:cursor + first]
            return {"data": {"repository": {"pullRequests": {
                "nodes": copy.deepcopy(chunk), "pageInfo": {
                    "hasNextPage": cursor + first < len(nodes), "endCursor": str(cursor + first),
                },
            }}}}
        numbers = re.findall(r"q(\d+):pullRequest\(number:(\d+)\)", query)
        return {"data": {"repository": {
            f"q{alias}": copy.deepcopy(self.nodes.get(int(number))) for alias, number in numbers
        }}}

    def run_dashboard(self) -> dict:
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(pr_review.command_dashboard_data(self.args), 0)
        return json.loads((self.state_dir / "review-dashboard.json").read_text())

    def cache_bytes(self) -> bytes:
        return (self.state_dir / "github-discovery.json").read_bytes()

    def detail_calls(self) -> list:
        return [call for call in self.calls if any("q0:pullRequest(" in item for item in call)]

    @staticmethod
    def row(snapshot: dict, number: int = 1) -> dict:
        return next(row for rows in snapshot["candidates_by_action"].values() for row in rows if row["pr"] == number)

    def review(self, author: str, date: str, state: str = "COMMENTED") -> dict:
        return {"author": {"login": author}, "authorAssociation": "MEMBER" if author == "maintainer" else "CONTRIBUTOR",
                "state": state, "submittedAt": date, "updatedAt": date,
                "lastEditedAt": None, "commit": {"oid": self.nodes[1]["headRefOid"]}}

    def record(self, event_type: str, timestamp: str = "2026-10-06T10:00:00Z") -> None:
        pr_review.append_event(self.state_dir, {"event_type": event_type, "pr": 1,
            "head_sha": self.nodes[1]["headRefOid"], "author": "contributor", "timestamp": timestamp})

    def test_unchanged_sweep_retains_inventory_and_unfinished_review_without_full_fetch(self) -> None:
        first = self.run_dashboard()
        self.calls.clear()
        self.clock = "2026-10-06T12:02:00Z"
        second = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "review")
        self.assertEqual(self.row(second)["advisory_action"], "review")
        self.assertEqual(second["action_counts"], {"review": 1})
        self.assertEqual([row["pr"] for row in second["sweep_candidates_by_action"]["review"]], [1])
        self.assertEqual(self.detail_calls(), [])
        queries = [item for call in self.calls for item in call if item.startswith("query=")]
        self.assertEqual(queries, ["query=" + pr_review.CENSUS_PR_QUERY])
        self.assertNotIn("files(", queries[0])
        self.assertNotIn("body", queries[0])
        self.assertNotIn("commits(last:20)", queries[0])
        self.assertEqual(second["discovery"]["since"], "2026-10-06T11:59:00Z")
        self.assertEqual(json.loads(self.cache_bytes())["query_started_at"], self.clock)

    def test_rest_since_complete_pages_dedup_and_ordinary_issue_filter(self) -> None:
        self.run_dashboard()
        self.calls.clear()
        self.issues = [{"id": 9, "number": 1, "pull_request": {"url": "pull"}}] * 2
        self.issues += [{"id": number + 100, "number": number + 100} for number in range(4)]
        self.comments = [{"id": 99, "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/999"}]
        with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 2):
            result = self.run_dashboard()
        issue_calls = [call for call in self.calls if call[2].endswith("/issues")]
        self.assertEqual(len(issue_calls), 8)
        for call in issue_calls:
            self.assertIn("state=all", call)
            self.assertIn("sort=created", call)
            self.assertIn("direction=asc", call)
            self.assertIn("since=2026-10-06T11:59:00Z", call)
        self.assertEqual(result["discovery"]["refreshed_prs"], [1])
        self.assertEqual(len(self.detail_calls()), 1)
        self.assertNotIn("number:999", str(self.detail_calls()))

    def test_query_start_cursor_and_overlap_replays_equal_second_updates(self) -> None:
        self.run_dashboard()
        self.clock = "2026-10-06T12:02:00Z"
        original = self.github
        def advance_during_query(command):
            result = original(command)
            if command[2].endswith("/issues"):
                self.clock = "2026-10-06T12:03:00Z"
            return result
        with mock.patch.object(pr_review, "run_json", side_effect=advance_during_query):
            self.run_dashboard()
        self.assertEqual(json.loads(self.cache_bytes())["query_started_at"], "2026-10-06T12:02:00Z")
        self.issues = [{"id": 1, "number": 1, "pull_request": {}, "updated_at": "2026-10-06T12:02:00Z"}]
        # The marker's presence identifies PRs even if GitHub supplies an empty object.
        self.issues[0]["pull_request"] = {"url": "pull"}
        result = self.run_dashboard()
        self.assertEqual(result["discovery"]["since"], "2026-10-06T12:01:00Z")
        self.assertEqual(result["discovery"]["refreshed_prs"], [1])

    def test_old_comment_entering_before_read_page_is_recovered_during_long_sweep(self) -> None:
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-06T09:00:00Z")])
        self.record("held")
        self.run_dashboard()
        old_comment = {"id": 10, "node_id": "old-comment", "user": {"login": "contributor"},
            "author_association": "CONTRIBUTOR", "created_at": "2026-07-01T00:00:00Z",
            "updated_at": "2026-10-06T08:00:00Z", "body": "I addressed the feedback.",
            "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/1"}
        self.comments = [old_comment] + [
            {"id": number, "created_at": f"2026-08-{number:02d}T00:00:00Z",
             "updated_at": "2026-10-06T12:00:30Z",
             "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/999"}
            for number in (20, 30)
        ]
        self.clock = "2026-10-06T12:02:00Z"
        original = self.github
        edited = False

        def paginate_live_membership(command):
            nonlocal edited
            if not command[2].endswith("issues/comments"):
                return original(command)
            self.calls.append(command)
            since = next(item.split("=", 1)[1] for item in command if item.startswith("since="))
            page = int(next(item.split("=", 1)[1] for item in command if item.startswith("page=")))
            count = int(next(item.split("=", 1)[1] for item in command if item.startswith("per_page=")))
            matching = sorted((row for row in self.comments if row["updated_at"] > since),
                              key=lambda row: row["created_at"])
            result = copy.deepcopy(matching[(page - 1) * count:page * count])
            if not edited:
                # An old comment enters before page 1 AFTER page 1 was read.
                old_comment["updated_at"] = "2026-10-06T12:02:01Z"
                edited = True
                self.clock = "2026-10-06T12:10:00Z"
            return result

        with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 1), \
                mock.patch.object(pr_review, "run_json", side_effect=paginate_live_membership):
            first = self.run_dashboard()
            self.assertEqual(self.row(first)["advisory_action"], "review")
            self.assertEqual(first["discovery"]["refreshed_prs"], [1])
            self.assertEqual(json.loads(self.cache_bytes())["query_started_at"], "2026-10-06T12:02:00Z")
            second = self.run_dashboard()
        self.assertEqual(second["discovery"]["since"], "2026-10-06T12:01:00Z")
        self.assertEqual(second["discovery"]["refreshed_prs"], [])
        self.assertEqual(self.row(second)["advisory_action"], "review")
        self.assertEqual(self.row(second)["reason"], "author_followup_after_maintainer_activity")

    def test_deletion_before_page_offset_cannot_drop_unread_old_comment_edit(self) -> None:
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-06T09:00:00Z")])
        self.record("held")
        self.run_dashboard()
        self.clock = "2026-10-06T12:02:00Z"
        self.comments = [
            {"id": 10, "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/999"},
            {"id": 20, "node_id": "old-comment", "user": {"login": "contributor"},
             "author_association": "CONTRIBUTOR", "created_at": "2026-07-01T00:00:00Z",
             "updated_at": "2026-10-06T12:00:30Z", "body": "I addressed the feedback.",
             "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/1"},
            {"id": 30, "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/999"},
        ]
        original = self.github
        deleted = False

        def delete_read_comment(command):
            nonlocal deleted
            result = original(command)
            if command[2].endswith("issues/comments") and not deleted:
                # Removing page 1 shifts the unread edit behind page 2's offset.
                self.comments.pop(0)
                deleted = True
            return result

        with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 1), \
                mock.patch.object(pr_review, "run_json", side_effect=delete_read_comment):
            result = self.run_dashboard()
        self.assertEqual(result["discovery"]["refreshed_prs"], [1])
        self.assertEqual(self.row(result)["advisory_action"], "review")
        self.assertEqual(self.row(result)["reason"], "author_followup_after_maintainer_activity")
        self.assertEqual(json.loads(self.cache_bytes())["query_started_at"], "2026-10-06T12:02:00Z")

    def test_compact_watch_refreshes_independent_ci_queue_head_label_issue_and_review_edits(self) -> None:
        self.run_dashboard()
        mutations = [
            lambda node: node["commits"]["nodes"][-1]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0].update(conclusion="FAILURE"),
            lambda node: node.update(isInMergeQueue=True, mergeQueueEntry={"position": 1, "state": "QUEUED"}),
            lambda node: node.update(isInMergeQueue=False, mergeQueueEntry=None),
            lambda node: node.update(headRefOid="new-head"),
            lambda node: node.update(labels=self.connection([{"name": "approved-for-review"}])),
            lambda node: node.update(closingIssuesReferences=self.connection([
                {"number": 42, "state": "OPEN", "labels": self.connection([{"name": "accepted"}])}])),
            lambda node: node.update(reviews=self.connection([self.review("contributor", "2026-10-06T10:00:00Z")])),
            lambda node: node["reviews"]["nodes"][0].update(lastEditedAt="2026-10-06T11:30:00Z"),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                self.calls.clear()
                old_updated_at = self.nodes[1]["updatedAt"]
                mutate(self.nodes[1])
                result = self.run_dashboard()
                self.assertEqual(self.nodes[1]["updatedAt"], old_updated_at)
                self.assertEqual(result["discovery"]["refreshed_prs"], [1])
                self.assertEqual(len(self.detail_calls()), 1)
        self.assertEqual(self.row(result)["freshness"]["author_followup_after_maintainer_activity"], True)

    def test_edited_author_comment_beyond_recent_window_routes_production_dashboard(self) -> None:
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-06T09:00:00Z")])
        self.record("held")
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "hold")
        self.comments = [{"id": 55, "node_id": "old-comment", "user": {"login": "contributor"},
            "author_association": "CONTRIBUTOR", "created_at": "2026-07-01T00:00:00Z",
            "updated_at": "2026-10-06T11:00:00Z", "body": "I addressed the feedback.",
            "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/1"}]
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "review")
        self.assertEqual(self.row(result)["reason"], "author_followup_after_maintainer_activity")
        self.assertTrue(self.row(result)["freshness"]["author_followup_after_local_event"])
        self.assertEqual([row["pr"] for row in result["sweep_candidates_by_action"]["review"]], [1])

    def test_author_review_edit_routes_production_dashboard_and_member_comment_holds(self) -> None:
        author_review = self.review("contributor", "2026-10-06T08:00:00Z")
        self.nodes[1]["reviews"] = self.connection([author_review, self.review("maintainer", "2026-10-06T09:00:00Z")])
        self.record("held")
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "hold")
        author_review["lastEditedAt"] = "2026-10-06T11:00:00Z"
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "review")
        self.assertTrue(self.row(result)["freshness"]["author_followup_after_maintainer_activity"])
        self.assertFalse(self.row(result)["freshness"]["member_activity_is_latest"])
        self.nodes[1]["comments"] = self.connection([{"id": "member-comment", "author": {"login": "maintainer"},
            "authorAssociation": "MEMBER", "createdAt": "2026-10-06T11:30:00Z", "updatedAt": "2026-10-06T11:30:00Z", "body": "Held on `head-1`: waiting for author evidence. Next step: recheck after the author responds."}])
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "hold")
        self.assertTrue(self.row(result)["freshness"]["member_activity_is_latest"])
        self.calls.clear()
        result = self.run_dashboard()
        self.assertEqual(result["sweep_candidates_by_action"], {})
        self.assertEqual(result["action_counts"], {"hold": 1})
        self.assertEqual(self.detail_calls(), [])

    def test_lifecycle_new_closed_merged_reopened_without_terminal_history_rescan(self) -> None:
        self.nodes[9] = self.node(9)
        self.nodes[9].update(state="CLOSED", closedAt="2026-09-01T00:00:00Z")
        self.run_dashboard()
        self.nodes[2] = self.node(2)
        self.nodes[1].update(state="MERGED", mergedAt=self.clock, closedAt=self.clock)
        self.calls.clear()
        result = self.run_dashboard()
        self.assertEqual([row["pr"] for rows in result["candidates_by_action"].values() for row in rows], [2])
        self.assertEqual({row["pr"] for row in result["terminal_archive"]}, {1, 9})
        self.assertEqual(result["discovery"]["terminal_changed_prs"], [1])
        self.nodes[9].update(state="OPEN", closedAt=None)
        self.nodes[2].update(state="CLOSED", closedAt=self.clock)
        result = self.run_dashboard()
        self.assertEqual({row["pr"] for row in result["terminal_archive"]}, {1, 2})
        self.assertEqual(self.row(result, 9)["advisory_action"], "review")
        self.assertFalse(any("states:[CLOSED,MERGED]" in str(call) for call in self.calls))

    def test_new_terminal_delta_ordinary_issues_and_complete_census_cover_page_race(self) -> None:
        self.run_dashboard()
        self.nodes[8] = self.node(8)
        self.nodes[8].update(state="CLOSED", closedAt=self.clock)
        self.issues = [{"id": 8, "number": 8, "pull_request": {"url": "pull"}}, {"id": 99, "number": 99}]
        self.nodes[2] = self.node(2)  # Created between REST pages, absent from its result.
        with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 1), mock.patch.object(pr_review, "SCAN_PAGE_SIZE", 1):
            result = self.run_dashboard()
        self.assertEqual(result["discovery"]["refreshed_prs"], [2, 8])
        self.assertEqual(self.row(result, 2)["advisory_action"], "review")
        self.assertEqual({row["pr"] for row in result["terminal_archive"]}, {8})

    def test_local_history_and_policy_changes_recompute_without_detail_fetch(self) -> None:
        self.run_dashboard()
        self.calls.clear()
        self.record("held")
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "hold")
        self.assertEqual([row["pr"] for row in result["sweep_candidates_by_action"]["hold"]], [1])
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "hold")
        self.assertIn("hold", result["sweep_candidates_by_action"])
        private_path = self.state_dir / pr_review.PRIVATE_OVERRIDES
        private_path.write_text(json.dumps({"contributor_standing": {"contributor": {"standing": "skip"}}}) + "\n")
        result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "skip")
        self.assertEqual(self.detail_calls(), [])

    def test_elapsed_requested_changes_deadline_recomputes_without_detail_fetch(self) -> None:
        self.nodes[1]["reviewDecision"] = "CHANGES_REQUESTED"
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-05T09:00:00Z", "CHANGES_REQUESTED")])
        with mock.patch.object(pr_review, "age_in_days", return_value=1):
            first = self.run_dashboard()
        self.assertNotEqual(self.row(first)["advisory_action"], "warn_stale_changes_for_handler")
        self.calls.clear()
        with mock.patch.object(pr_review, "age_in_days", return_value=8):
            result = self.run_dashboard()
        self.assertEqual(self.row(result)["advisory_action"], "warn_stale_changes_for_handler")
        self.assertIn("warn_stale_changes_for_handler", result["sweep_candidates_by_action"])
        self.assertEqual(self.detail_calls(), [])

    def test_partial_graphql_and_page_failures_leave_cursor_and_outputs_unchanged(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        dashboard = (self.state_dir / "review-dashboard.json").read_bytes()
        original = self.github
        for failure in ("rest", "graphql"):
            with self.subTest(failure=failure):
                def fail(command):
                    if failure == "rest" and command[2].endswith("issues/comments"):
                        raise subprocess.CalledProcessError(1, command)
                    result = original(command)
                    if failure == "graphql" and command[2] == "graphql":
                        result["errors"] = [{"message": "Partial response"}]
                    return result
                with mock.patch.object(pr_review, "run_json", side_effect=fail):
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        self.run_dashboard()
                self.assertEqual(self.cache_bytes(), previous)
                self.assertEqual((self.state_dir / "review-dashboard.json").read_bytes(), dashboard)

    def test_gateway_timeouts_shrink_heavy_graphql_reads(self) -> None:
        self.nodes = {number: self.node(number) for number in (1, 2, 3)}
        original = self.github

        def timeout_unless_single(command):
            query = next((item for item in command if item.startswith("query=")), "")
            first = next((int(item.split("=", 1)[1]) for item in command if item.startswith("first=")), 1)
            if ("pullRequests(states:[OPEN]" in query and first > 1) or query.count(":pullRequest(") > 1:
                raise subprocess.CalledProcessError(1, command, stderr="gh: HTTP 504")
            return original(command)

        with mock.patch.object(pr_review, "run_json", side_effect=timeout_unless_single):
            self.assertEqual(self.run_dashboard()["discovery"]["refreshed_prs"], [1, 2, 3])
            self.issues = [{"id": number, "number": number, "pull_request": {"url": "pull"}} for number in (1, 2, 3)]
            self.assertEqual(self.run_dashboard()["discovery"]["refreshed_prs"], [1, 2, 3])

    def test_non_timeout_graphql_failure_is_not_retried_smaller(self) -> None:
        self.nodes = {number: self.node(number) for number in (1, 2)}
        self.run_dashboard()
        previous = self.cache_bytes()
        self.issues = [{"id": number, "number": number, "pull_request": {"url": "pull"}} for number in (1, 2)]
        original = self.github

        def unauthorized_detail(command):
            if any("q0:pullRequest(" in item for item in command):
                self.calls.append(command)
                raise subprocess.CalledProcessError(1, command, stderr="gh: HTTP 401")
            return original(command)

        self.calls = []
        with mock.patch.object(pr_review, "run_json", side_effect=unauthorized_detail):
            with self.assertRaises(subprocess.CalledProcessError):
                self.run_dashboard()
        self.assertEqual(len(self.detail_calls()), 1)
        self.assertEqual(self.cache_bytes(), previous)

    def test_second_rest_page_failure_replays_and_does_not_consume_changes(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        self.issues = [{"id": 1, "number": 1, "pull_request": {"url": "pull"}}]
        original = self.github
        def fail(command):
            if command[2].endswith("/issues") and "page=2" in command:
                raise ValueError("failed page")
            return original(command)
        with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 1), mock.patch.object(pr_review, "run_json", side_effect=fail):
            with self.assertRaises(ValueError):
                self.run_dashboard()
        self.assertEqual(self.cache_bytes(), previous)
        self.assertEqual(self.run_dashboard()["discovery"]["refreshed_prs"], [1])

    def test_delta_volume_and_membership_churn_exhaust_budget_without_publishing(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        dashboard_path = self.state_dir / "review-dashboard.json"
        html_path = self.state_dir / "review-dashboard.html"
        dashboard, html = dashboard_path.read_bytes(), html_path.read_bytes()
        original = self.github
        for endpoint in ("issues", "issues/comments"):
            for scenario in ("volume", "churn"):
                with self.subTest(endpoint=endpoint, scenario=scenario):
                    requests = 0

                    def exceed_budget(command):
                        nonlocal requests
                        if command[2] != f"repos/phase-rs/phase/{endpoint}":
                            return original(command)
                        requests += 1
                        self.assertLessEqual(requests, 3, "REST budget must also bound replay passes")
                        page = int(next(item.split("=", 1)[1] for item in command if item.startswith("page=")))
                        if scenario == "volume" or page == 1:
                            return [{"id": requests, "number": requests}]
                        return []

                    with mock.patch.object(pr_review, "DISCOVERY_REST_PAGE_SIZE", 1), \
                            mock.patch.object(pr_review, "DISCOVERY_REST_MAX_REQUESTS", 3), \
                            mock.patch.object(pr_review, "run_json", side_effect=exceed_budget):
                        with self.assertRaisesRegex(ValueError, "budget; discovery is incomplete"):
                            self.run_dashboard()
                    self.assertEqual(requests, 3)
                    self.assertEqual(self.cache_bytes(), previous)
                    self.assertEqual(dashboard_path.read_bytes(), dashboard)
                    self.assertEqual(html_path.read_bytes(), html)
        self.assertEqual(self.run_dashboard()["discovery"]["mode"], "incremental")

    def test_truncated_open_bootstrap_and_census_never_advance(self) -> None:
        self.args.limit = 1
        self.nodes[2] = self.node(2)
        with self.assertRaisesRegex(ValueError, "increase --limit"):
            self.run_dashboard()
        self.assertFalse((self.state_dir / "github-discovery.json").exists())
        self.nodes.pop(2)
        self.run_dashboard()  # Exactly at limit is complete, not truncated.
        previous = self.cache_bytes()
        self.nodes[2] = self.node(2)
        with self.assertRaisesRegex(ValueError, "increase --limit"):
            self.run_dashboard()
        self.assertEqual(self.cache_bytes(), previous)

    def test_json_html_and_packet_failures_never_advance_checkpoint(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        self.clock = "2026-10-06T12:02:00Z"
        for owner, name in ((pr_review, "write_json_atomically"),
                            (pr_review_dashboard, "write_text_atomically"),
                            (pr_review, "packet_for_pr")):
            with self.subTest(failure=name), mock.patch.object(owner, name, side_effect=ValueError("failed output")):
                with self.assertRaises(ValueError):
                    self.run_dashboard()
                self.assertEqual(self.cache_bytes(), previous)
        result = self.run_dashboard()
        self.assertEqual(result["discovery"]["since"], "2026-10-06T11:59:00Z")

    def test_invalid_repo_version_corrupt_cache_and_full_refresh_bootstrap(self) -> None:
        self.run_dashboard()
        cache_path = self.state_dir / "github-discovery.json"
        for field, value in (("repo", "other/repo"), ("version", -1), ("open_nodes", {})):
            cache = json.loads(cache_path.read_text())
            if field == "open_nodes":
                cache["query_started_at"] = "not-a-time"
            else:
                cache[field] = value
            cache_path.write_text(json.dumps(cache) + "\n")
            self.assertEqual(self.run_dashboard()["discovery"]["mode"], "bootstrap")
        cache_path.write_text("{broken\n")
        self.assertEqual(self.run_dashboard()["discovery"]["mode"], "bootstrap")
        self.args.full_refresh = True
        self.assertEqual(self.run_dashboard()["discovery"]["mode"], "bootstrap")

    def test_missing_or_invalid_detail_never_advances(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        self.nodes[1]["headRefOid"] = "changed"
        original = self.github
        for missing in (True, False):
            def invalid(command):
                result = original(command)
                if any("q0:pullRequest(" in item for item in command):
                    if missing:
                        result["data"]["repository"]["q0"] = None
                    else:
                        result["data"]["repository"]["q0"].pop("reviews")
                return result
            with mock.patch.object(pr_review, "run_json", side_effect=invalid):
                with self.assertRaises(ValueError):
                    self.run_dashboard()
            self.assertEqual(self.cache_bytes(), previous)

    def test_scan_is_full_diagnostic_and_does_not_advance_dashboard_cursor(self) -> None:
        self.run_dashboard()
        previous = self.cache_bytes()
        self.calls.clear()
        with contextlib.redirect_stdout(io.StringIO()):
            pr_review.command_scan(self.args)
        self.assertEqual(self.cache_bytes(), previous)
        self.assertTrue(any("query=" + pr_review.SCAN_PR_QUERY in call for call in self.calls))



    def test_identical_overlap_does_not_refetch_but_same_second_comment_edit_does(self) -> None:
        self.run_dashboard()
        self.issues = [{"id": 1, "number": 1, "pull_request": {"url": "pull"}, "updated_at": self.clock}]
        self.comments = [{"id": 51, "node_id": "comment", "user": {"login": "contributor"},
            "created_at": "2026-10-06T08:00:00Z", "updated_at": self.clock, "body": "First version",
            "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/1"}]
        self.assertEqual(self.run_dashboard()["discovery"]["refreshed_prs"], [1])
        self.calls.clear()
        result = self.run_dashboard()
        self.assertEqual(result["discovery"]["refreshed_prs"], [])
        self.assertEqual(self.detail_calls(), [])
        self.comments[0]["body"] = "Edited within the same second"
        self.assertEqual(self.run_dashboard()["discovery"]["refreshed_prs"], [1])

    def test_old_edited_comment_survives_later_ci_refresh_and_complete_history_proves_deletion(self) -> None:
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-06T09:00:00Z")])
        self.nodes[1]["comments"] = self.connection([], total=101)
        self.record("held")
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["reason"], "author_activity_history_incomplete")
        self.comments = [{"id": 55, "node_id": "old-comment", "user": {"login": "contributor"},
            "author_association": "CONTRIBUTOR", "created_at": "2026-07-01T00:00:00Z",
            "updated_at": "2026-10-06T11:00:00Z", "body": "Fixed it",
            "issue_url": "https://api.github.com/repos/phase-rs/phase/issues/1"}]
        edited = self.run_dashboard()
        self.assertEqual(self.row(edited)["reason"], "author_followup_after_maintainer_activity")
        self.comments = []
        self.nodes[1]["commits"]["nodes"][-1]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0]["conclusion"] = "FAILURE"
        refreshed = self.run_dashboard()
        self.assertEqual(refreshed["discovery"]["refreshed_prs"], [1])
        self.assertEqual(self.row(refreshed)["reason"], "author_followup_after_maintainer_activity")
        self.assertTrue(self.row(refreshed)["freshness"]["comment_history_incomplete"])
        self.calls.clear()
        unchanged = self.run_dashboard()
        self.assertEqual(unchanged["discovery"]["refreshed_prs"], [])
        self.assertTrue(self.row(unchanged)["freshness"]["author_followup_after_maintainer_activity"])
        # Once the complete authoritative history is available, the absent ID is
        # a proven deletion rather than an omission from the last-100 window.
        self.nodes[1]["comments"] = self.connection([])
        deleted = self.run_dashboard()
        self.assertFalse(self.row(deleted)["freshness"]["author_followup_after_maintainer_activity"])
        self.assertFalse(self.row(deleted)["freshness"]["comment_history_incomplete"])

    def test_ci_hold_completion_routes_handler_with_unchanged_parent_timestamp(self) -> None:
        check = self.nodes[1]["commits"]["nodes"][-1]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0]
        check.update(status="IN_PROGRESS", conclusion=None)
        self.record("hold_ci")
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "hold_ci")
        check.update(status="COMPLETED", conclusion="SUCCESS")
        second = self.run_dashboard()
        self.assertEqual(self.row(second)["advisory_action"], "recheck_ci_hold_for_handler")
        self.assertIn("recheck_ci_hold_for_handler", second["sweep_candidates_by_action"])
        self.assertEqual(second["discovery"]["refreshed_prs"], [1])

    def test_editing_old_review_preserves_newer_current_head_approval(self) -> None:
        older = self.review("maintainer", "2026-10-05T09:00:00Z")
        older["commit"] = {"oid": "old-head"}
        approval = self.review("maintainer", "2026-10-06T09:00:00Z", "APPROVED")
        self.nodes[1]["reviews"] = self.connection([older, approval])
        self.nodes[1].update(reviewDecision="APPROVED", isInMergeQueue=True,
                             mergeQueueEntry={"position": 1, "state": "QUEUED"})
        self.assertEqual(self.row(self.run_dashboard())["advisory_action"], "queued")
        older.update(updatedAt="2026-10-06T11:00:00Z", lastEditedAt="2026-10-06T11:00:00Z")
        edited = self.run_dashboard()
        self.assertEqual(edited["discovery"]["refreshed_prs"], [1])
        self.assertEqual(self.row(edited)["advisory_action"], "queued")
        self.assertEqual(self.row(edited)["freshness"]["latest_maintainer_activity_at"],
                         "2026-10-06T11:00:00Z")

    def test_nullable_author_survives_bootstrap_census_and_detail_refresh(self) -> None:
        self.nodes[1]["author"] = None
        self.assertEqual(self.row(self.run_dashboard())["pr"], 1)
        self.nodes[1]["title"] = "Updated title with a deleted author"
        refreshed = self.run_dashboard()
        self.assertEqual(refreshed["discovery"]["refreshed_prs"], [1])
        self.assertEqual(self.row(refreshed)["title"], self.nodes[1]["title"])

    def test_queue_drop_and_stale_approval_route_production_dashboard(self) -> None:
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", "2026-10-06T09:00:00Z", "APPROVED")])
        self.nodes[1].update(reviewDecision="APPROVED", isInMergeQueue=True,
                             mergeQueueEntry={"position": 1, "state": "QUEUED"})
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "queued")
        self.nodes[1].update(isInMergeQueue=False, mergeQueueEntry=None)
        dropped = self.run_dashboard()
        self.assertEqual(self.row(dropped)["advisory_action"], "approve_ready_for_handler")
        self.assertIn("approve_ready_for_handler", dropped["sweep_candidates_by_action"])
        self.nodes[1].update(headRefOid="unreviewed-head", autoMergeRequest={"enabledAt": self.clock})
        stale = self.run_dashboard()
        self.assertEqual(self.row(stale)["advisory_action"], "dequeue_stale_for_handler")
        self.assertEqual(self.row(stale)["reason"], "stale_approval")

    def test_live_routing_label_and_linked_issue_labels_reach_actual_packets(self) -> None:
        self.nodes[1]["files"] = self.connection([{"path": "client/src/Component.tsx"}])
        first = self.run_dashboard()
        self.assertEqual(self.row(first)["advisory_action"], "defer")
        label = pr_review.load_policy(self.args.config).approved_for_review_label
        self.nodes[1]["labels"] = self.connection([{"name": label}])
        labelled = self.run_dashboard()
        self.assertEqual(self.row(labelled)["reason"], "approved_for_review_label")
        self.assertEqual(self.row(labelled)["advisory_action"], "review")
        self.nodes[1]["closingIssuesReferences"] = self.connection([
            {"number": 77, "state": "OPEN", "labels": self.connection([{"name": "accepted"}])}])
        accepted = self.run_dashboard()
        records = self.row(accepted)["architecture_scope"]["evidence"]["closing_issue_records"]
        self.assertEqual(records[0]["number"], 77)
        self.assertEqual(records[0]["labels"], [{"name": "accepted"}])
        self.assertEqual(accepted["discovery"]["refreshed_prs"], [1])

    def test_concurrent_dashboard_processes_serialize_the_entire_publish(self) -> None:
        script = '''import argparse,json,sys,time
from pathlib import Path
import pr_review
state = Path(sys.argv[1])
def publish(args):
    path = state / "counter.json"
    before = json.loads(path.read_text()) if path.exists() else 0
    with (state / "order.log").open("a") as log:
        log.write("start\\n")
    time.sleep(0.15)
    path.write_text(json.dumps(before + 1))
    with (state / "order.log").open("a") as log:
        log.write("end\\n")
    return 0
pr_review.publish_dashboard_data = publish
pr_review.command_dashboard_data(argparse.Namespace(state_dir=state))
'''
        # Both enter the real dashboard command's flock before the replaced
        # publisher reads state. Without the lock both read zero and lose a run.
        commands = ["python3", "-c", script, str(self.state_dir)]
        env = {**os.environ, "PYTHONPATH": str(pr_review.REPO_ROOT / "scripts")}
        processes = [subprocess.Popen(commands, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(2)]
        for process in processes:
            _, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, stderr.decode())
        self.assertEqual(json.loads((self.state_dir / "counter.json").read_text()), 2)
        self.assertEqual((self.state_dir / "order.log").read_text().splitlines(), ["start", "end", "start", "end"])


    def test_unchanged_eventless_defer_stays_work_until_disposition_is_visible(self) -> None:
        self.nodes[1]["files"] = self.connection([{"path": "client/src/Component.tsx"}])
        self.assertEqual(self.row(self.run_dashboard())["advisory_action"], "defer")
        self.calls.clear()
        second = self.run_dashboard()
        self.assertEqual([row["pr"] for row in second["sweep_candidates_by_action"]["defer"]], [1])
        self.assertEqual(self.detail_calls(), [])
        self.record("deferred")
        self.run_dashboard()
        self.assertIn("defer", self.run_dashboard()["sweep_candidates_by_action"])
        self.nodes[1]["comments"] = self.connection([{
            "id": "defer-response", "author": {"login": "maintainer"}, "authorAssociation": "MEMBER",
            "createdAt": self.clock, "updatedAt": self.clock,
            "body": "<!-- pr-review-deferred -->\nDeferred current head `head-1` by frontend policy. A maintainer must take this review.",
        }])
        self.run_dashboard()
        self.assertEqual(self.run_dashboard()["sweep_candidates_by_action"], {})

    def test_unchanged_local_block_needs_formal_current_head_review_to_finish(self) -> None:
        self.record("blocked")
        self.run_dashboard()
        second = self.run_dashboard()
        self.assertEqual(self.row(second)["advisory_action"], "blocked")
        self.assertIn("blocked", second["sweep_candidates_by_action"])
        self.nodes[1]["reviews"] = self.connection([self.review("maintainer", self.clock, "CHANGES_REQUESTED")])
        self.nodes[1]["reviewDecision"] = "CHANGES_REQUESTED"
        self.run_dashboard()
        self.assertEqual(self.run_dashboard()["sweep_candidates_by_action"], {})
        # Formal visible disposition still suffices on a host with no local log.
        (self.state_dir / "review-events.jsonl").unlink()
        self.run_dashboard()
        stable = self.run_dashboard()
        self.assertEqual(self.row(stable)["advisory_action"], "blocked")
        self.assertEqual(stable["sweep_candidates_by_action"], {})


    def test_local_only_waiting_events_and_ambiguous_member_comments_remain_actionable(self) -> None:
        for event_type, action in (("held", "hold"), ("hold_ci", "hold_ci"), ("deferred", "defer")):
            with self.subTest(event=event_type):
                (self.state_dir / "github-discovery.json").unlink(missing_ok=True)
                (self.state_dir / "review-events.jsonl").unlink(missing_ok=True)
                self.nodes[1] = self.node(1)
                if action == "hold_ci":
                    self.nodes[1]["commits"]["nodes"][-1]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0].update(status="IN_PROGRESS", conclusion=None)
                elif action == "defer":
                    self.nodes[1]["files"] = self.connection([{"path": "client/src/Component.tsx"}])
                self.record(event_type)
                responses = [
                    None,
                    "Thanks for contributing to `head-1`.",
                    "Held on `head-1-old`: pending artifact publication. Next step: recheck after publication.",
                    "Held on `head-1`: pending artifact publication.",
                    "Held on `head-1`. Next step: recheck after publication.",
                    "<!-- pr-review-deferred -->\nDeferred head `head-1-old` by frontend policy.",
                ]
                for index, body in enumerate(responses):
                    with self.subTest(event=event_type, body=body):
                        self.nodes[1]["comments"] = self.connection([] if body is None else [{
                            "id": f"response-{index}", "author": {"login": "maintainer"}, "authorAssociation": "MEMBER",
                            "createdAt": self.clock, "updatedAt": self.clock, "body": body,
                        }])
                        self.run_dashboard()
                        self.calls.clear()
                        unchanged = self.run_dashboard()
                        self.assertEqual(self.row(unchanged)["advisory_action"], action)
                        self.assertEqual([row["pr"] for row in unchanged["sweep_candidates_by_action"][action]], [1])
                        self.assertEqual(self.detail_calls(), [])

    def test_matching_visible_hold_and_ci_hold_finish_unchanged_waiting_routes(self) -> None:
        for event_type, action in (("held", "hold"), ("hold_ci", "hold_ci")):
            with self.subTest(event=event_type):
                (self.state_dir / "github-discovery.json").unlink(missing_ok=True)
                (self.state_dir / "review-events.jsonl").unlink(missing_ok=True)
                self.nodes[1] = self.node(1)
                if action == "hold_ci":
                    self.nodes[1]["commits"]["nodes"][-1]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0].update(status="IN_PROGRESS", conclusion=None)
                self.record(event_type)
                self.nodes[1]["comments"] = self.connection([{
                    "id": "visible-hold", "author": {"login": "maintainer"}, "authorAssociation": "MEMBER",
                    "createdAt": self.clock, "updatedAt": self.clock,
                    "body": "**Held** on `head-1`: pending artifact publication. Next step: recheck after the artifacts publish.",
                }])
                self.run_dashboard()
                self.calls.clear()
                unchanged = self.run_dashboard()
                self.assertEqual(self.row(unchanged)["advisory_action"], action)
                self.assertEqual(unchanged["sweep_candidates_by_action"], {})
                self.assertEqual(unchanged["action_counts"], {action: 1})
                self.assertEqual(self.detail_calls(), [])


if __name__ == "__main__":
    unittest.main()
