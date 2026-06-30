#!/usr/bin/env python3
"""Portable PR review intelligence helper.

This tool keeps durable review memory as an append-only JSONL event log and
maintains a derived SQLite index for cheap queries. It is advisory: GitHub
mutations stay in the maintainer handling skills.
"""
from __future__ import annotations

import argparse
import csv
import fnmatch
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from statistics import median
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_POLICY = REPO_ROOT / ".agents/pr-review-policy.toml"
PRIVATE_OVERRIDES = "private-overrides.json"
SUCCESS_STATES = {"accepted", "merged"}
BLOCK_STATES = {"blocked", "changes_requested"}
HOLD_STATES = {"held", "held_ci"}
TERMINAL_STATES = SUCCESS_STATES | BLOCK_STATES | {"closed"}
PR_ATTRIBUTED_EVENTS = {
    "approval_enqueue",
    "approved_enqueued",
    "blocked",
    "changes_requested",
    "defer",
    "deferred",
    "fixup_push",
    "freshness_check",
    "hard_stop",
    "held",
    "held_current_changes_requested",
    "held_mixed_fe",
    "hold",
    "hold_ci",
    "hold_review",
    "pruned",
    "pruned_merged",
    "prune_merged",
    "request_changes",
    "review",
    "review_blocked",
    "review_correction",
    "review_reopened",
    "tracker_row",
    "update_branch",
}
QUALITY_SIGNAL_WEIGHTS = {
    "wrong-seam": 14,
    "false-green": 12,
    "runtime-test-gap": 10,
    "scope-contamination": 10,
    "rebase-not-fix": 8,
    "build-for-card": 8,
    "fmt/clippy-slip": 5,
    "stale-approval": 4,
    "low-effort-risk": 8,
    "author-created-issue-high-bar": 6,
    "value-bar": 6,
    "careful-watch": 4,
}


@dataclass(frozen=True)
class CanonicalOutcome:
    state: str
    source: str
    confidence: str
    reason: str


@dataclass
class PrAccumulator:
    pr: int
    contributor_login: str
    events: list[dict[str, Any]]
    head_events: dict[str, list[dict[str, Any]]]
    quality_signals: dict[str, int]


@dataclass(frozen=True)
class Policy:
    raw: dict[str, Any]

    @property
    def hard_stop_patterns(self) -> list[str]:
        return list(self.raw.get("hard_stops", {}).get("patterns", []))

    @property
    def generated_patterns(self) -> list[str]:
        return list(self.raw.get("generated", {}).get("patterns", []))

    @property
    def path_classes(self) -> dict[str, list[str]]:
        classes = self.raw.get("path_classes", {})
        return {name: list(value.get("patterns", [])) for name, value in classes.items()}

    @property
    def rules_domain(self) -> str | None:
        value = self.raw.get("domain", {}).get("rules_domain")
        return str(value) if value else None

    @property
    def default_tier(self) -> str:
        return str(self.raw.get("defaults", {}).get("tier", "T2"))


def now_iso() -> str:
    return datetime.now(UTC).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def repo_slug(repo: str | None) -> str:
    return (repo or "default").replace("/", "__")


def default_state_dir(repo: str | None) -> Path:
    if os.environ.get("PR_REVIEW_STATE_DIR"):
        return Path(os.environ["PR_REVIEW_STATE_DIR"]).expanduser()
    return Path.home() / ".local/state/pr-review" / repo_slug(repo)


def load_policy(path: Path) -> Policy:
    if not path.exists():
        return Policy({})
    with path.open("rb") as file:
        return Policy(tomllib.load(file))


def load_private_overrides(state_dir: Path) -> dict[str, Any]:
    path = state_dir / PRIVATE_OVERRIDES
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def frontend_review_allowed(author_login: str | None, overrides: dict[str, Any]) -> bool:
    if not author_login:
        return False
    authors = overrides.get("frontend_review_authors", [])
    normalized = {str(author).lower() for author in authors}
    return author_login.lower() in normalized


def json_dumps(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def text_hash(value: str | None) -> str | None:
    if value is None:
        return None
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:16]


def excerpt(value: str | None, limit: int = 500) -> str:
    if not value:
        return ""
    normalized = " ".join(value.split())
    if len(normalized) <= limit:
        return normalized
    return normalized[: limit - 1] + "…"


def event_id(event: dict[str, Any]) -> str:
    clean = {key: value for key, value in event.items() if key != "event_id"}
    return hashlib.sha256(json_dumps(clean).encode("utf-8")).hexdigest()


def normalize_event(event: dict[str, Any]) -> dict[str, Any]:
    normalized = dict(event)
    if normalized.get("head_sha") is None and normalized.get("head") is not None:
        normalized["head_sha"] = normalized["head"]
    action = normalized.get("action")
    if normalized.get("event_type") is None and action is not None:
        normalized["event_type"] = action
    summary = str(normalized.get("summary") or normalized.get("note") or "")
    if normalized.get("event_type") in {None, "observation"} and (
        summary.startswith("CHANGES_REQUESTED:")
        or summary.startswith("Requested changes:")
    ):
        normalized["event_type"] = "changes_requested"
    if normalized.get("outcome") is None and action in {
        "changes_requested",
        "blocked",
        "approved_enqueued",
        "deferred",
        "held",
    }:
        normalized["outcome"] = action
    normalized.setdefault("timestamp", now_iso())
    normalized.setdefault("event_type", "observation")
    normalized.setdefault("schema_version", 1)
    normalized["event_id"] = normalized.get("event_id") or event_id(normalized)
    return normalized


def run_json(command: list[str]) -> Any:
    result = subprocess.run(
        command,
        cwd=REPO_ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return json.loads(result.stdout or "null")


def run_text(command: list[str]) -> str:
    result = subprocess.run(
        command,
        cwd=REPO_ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout


def gh_user() -> str:
    return str(run_json(["gh", "api", "user"])["login"])


def ensure_state(state_dir: Path) -> sqlite3.Connection:
    state_dir.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(state_dir / "review-state.sqlite")
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS events (
            event_id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            pr INTEGER,
            head_sha TEXT,
            author TEXT,
            timestamp TEXT NOT NULL,
            payload_json TEXT NOT NULL
        )
        """
    )
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS leases (
            pr INTEGER NOT NULL,
            head_sha TEXT NOT NULL,
            acting_login TEXT NOT NULL,
            run_id TEXT NOT NULL,
            acquired_at TEXT NOT NULL,
            PRIMARY KEY (pr, head_sha, acting_login)
        )
        """
    )
    return conn


def append_event(state_dir: Path, event: dict[str, Any]) -> bool:
    normalized = normalize_event(event)
    conn = ensure_state(state_dir)
    inserted = False
    with conn:
        cursor = conn.execute(
            """
            INSERT OR IGNORE INTO events
              (event_id, event_type, pr, head_sha, author, timestamp, payload_json)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                normalized["event_id"],
                normalized["event_type"],
                normalized.get("pr"),
                normalized.get("head_sha"),
                normalized.get("author"),
                normalized["timestamp"],
                json_dumps(normalized),
            ),
        )
        inserted = cursor.rowcount == 1
    if inserted:
        with (state_dir / "review-events.jsonl").open("a", encoding="utf-8") as file:
            file.write(json_dumps(normalized) + "\n")
    conn.close()
    return inserted


def rebuild_index(state_dir: Path) -> None:
    conn = ensure_state(state_dir)
    with conn:
        conn.execute("DELETE FROM events")
        event_log = state_dir / "review-events.jsonl"
        if event_log.exists():
            for line in event_log.read_text(encoding="utf-8").splitlines():
                if not line.strip():
                    continue
                event = normalize_event(json.loads(line))
                conn.execute(
                    """
                    INSERT OR IGNORE INTO events
                      (event_id, event_type, pr, head_sha, author, timestamp, payload_json)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event["event_id"],
                        event["event_type"],
                        event.get("pr"),
                        event.get("head_sha"),
                        event.get("author"),
                        event["timestamp"],
                        json_dumps(event),
                    ),
                )
    conn.close()


def all_events(state_dir: Path) -> list[dict[str, Any]]:
    conn = ensure_state(state_dir)
    rows = conn.execute(
        "SELECT payload_json FROM events ORDER BY timestamp, event_id"
    ).fetchall()
    conn.close()
    return [json.loads(row[0]) for row in rows]


def latest_events_by_pr_head(state_dir: Path) -> dict[tuple[int, str], dict[str, Any]]:
    latest: dict[tuple[int, str], dict[str, Any]] = {}
    for event in all_events(state_dir):
        pr = event.get("pr")
        head_sha = event.get("head_sha")
        if pr is None or not head_sha:
            continue
        latest[(int(pr), str(head_sha))] = event
    return latest


def event_sort_key(event: dict[str, Any]) -> tuple[str, str]:
    return (str(event.get("timestamp") or ""), str(event.get("event_id") or ""))


def parse_event_datetime(value: str | None) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return parsed.replace(tzinfo=UTC)
    return parsed


def filtered_events_by_days(events: list[dict[str, Any]], days: int | None) -> list[dict[str, Any]]:
    if days is None:
        return events
    cutoff = datetime.now(UTC).replace(microsecond=0).timestamp() - (days * 24 * 60 * 60)
    filtered = []
    for event in events:
        timestamp = parse_event_datetime(event.get("timestamp"))
        if timestamp is not None and timestamp.timestamp() >= cutoff:
            filtered.append(event)
    return filtered


def canonical_from_text(value: str | None) -> tuple[str, str] | None:
    if not value:
        return None
    text = value.lower().replace("_", "-")
    if "changes-requested" in text or "request-changes" in text or "reviewed-request-changes" in text:
        return ("changes_requested", "negative_review")
    if "still-blocked" in text or text == "blocked" or text.startswith("blocked-"):
        return ("blocked", "blocked")
    if "hard-stop" in text:
        return ("blocked", "hard_stop")
    if "merged" in text or "pruned-as-merged" in text or text == "pruned-merged":
        return ("merged", "merged")
    if "defer-fe" in text or text == "defer" or text == "deferred":
        return ("deferred", "deferred")
    if "ci-failed" in text:
        return ("changes_requested", "ci_failed")
    if "pending-ci" in text or "hold-ci" in text or text == "hold-ci":
        return ("held_ci", "ci_pending")
    if text.startswith("hold") or text == "held" or text.startswith("held-"):
        return ("held", "held")
    if "approved-enqueued" in text or "approved-labeled-enqueued" in text:
        return ("accepted", "approved_enqueued")
    if text in {"enqueued", "enqueue", "approve-enqueue", "approval-enqueue", "handler-enqueue"}:
        return ("accepted", "enqueued")
    if text == "approved" or text == "approve":
        return ("accepted", "approved")
    if text.startswith("approve-pending") or text.startswith("content-clean-pending"):
        return ("held_ci", "approval_pending_ci")
    if text == "review" or text.startswith("review-"):
        return ("review", "review")
    if text == "pending" or text.startswith("pending-"):
        return ("pending", "pending")
    if text == "closed" or text.startswith("supersede") or text.startswith("superseded"):
        return ("closed", "closed")
    if text in {"queued", "pruned"}:
        return ("accepted", text)
    return None


def canonical_outcome(event: dict[str, Any]) -> CanonicalOutcome:
    tracker = event.get("tracker") or {}
    sources = [
        ("outcome", event.get("outcome")),
        ("action", event.get("action")),
        ("event_type", event.get("event_type")),
        ("tracker.verdict", tracker.get("verdict")),
    ]
    for source, value in sources:
        mapped = canonical_from_text(str(value) if value is not None else None)
        if mapped is not None:
            state, reason = mapped
            return CanonicalOutcome(state, source, "high", reason)
    enqueued = str(tracker.get("enqueued") or "").lower()
    if enqueued in {"yes", "true"}:
        return CanonicalOutcome("accepted", "tracker.enqueued", "medium", "legacy_enqueued")
    return CanonicalOutcome("unknown", "none", "low", "unclassified")


def contributor_login_for_event(event: dict[str, Any]) -> str | None:
    event_type = event.get("event_type")
    tracker = event.get("tracker") or {}
    quality = event.get("quality") or {}
    if event_type == "tracker_row":
        return tracker.get("author") or event.get("author")
    if event_type == "quality_entry":
        return quality.get("login") or event.get("author")
    if event_type in PR_ATTRIBUTED_EVENTS:
        return event.get("author")
    return None


def audit_event_values(events: list[dict[str, Any]]) -> dict[str, dict[str, int]]:
    counters: dict[str, dict[str, int]] = {
        "event_type": {},
        "action": {},
        "outcome": {},
        "tracker_verdict": {},
        "tracker_enqueued": {},
    }
    for event in events:
        tracker = event.get("tracker") or {}
        for name, value in [
            ("event_type", event.get("event_type")),
            ("action", event.get("action")),
            ("outcome", event.get("outcome")),
            ("tracker_verdict", tracker.get("verdict")),
            ("tracker_enqueued", tracker.get("enqueued")),
        ]:
            if value:
                text = str(value)
                counters[name][text] = counters[name].get(text, 0) + 1
    return counters


def unknown_event_values(events: list[dict[str, Any]]) -> dict[str, dict[str, int]]:
    unknowns: dict[str, dict[str, int]] = {
        "event_type": {},
        "action": {},
        "outcome": {},
        "tracker_verdict": {},
    }
    for event in events:
        if canonical_outcome(event).state != "unknown":
            continue
        tracker = event.get("tracker") or {}
        for name, value in [
            ("event_type", event.get("event_type")),
            ("action", event.get("action")),
            ("outcome", event.get("outcome")),
            ("tracker_verdict", tracker.get("verdict")),
        ]:
            if value:
                text = str(value)
                unknowns[name][text] = unknowns[name].get(text, 0) + 1
    return {name: values for name, values in unknowns.items() if values}


def head_analytics(pr: int, head_sha: str, events: list[dict[str, Any]]) -> dict[str, Any]:
    sorted_events = sorted(events, key=event_sort_key)
    ever_states: dict[str, int] = {}
    terminal = CanonicalOutcome("pending", "default", "low", "no_terminal_event")
    for event in sorted_events:
        outcome = canonical_outcome(event)
        ever_states[outcome.state] = ever_states.get(outcome.state, 0) + 1
        if outcome.state in TERMINAL_STATES:
            terminal = outcome
        elif terminal.state not in TERMINAL_STATES and outcome.state in {
            "held",
            "held_ci",
            "deferred",
            "review",
            "pending",
        }:
            terminal = outcome
    return {
        "pr": pr,
        "head_sha": head_sha,
        "events": len(sorted_events),
        "canonical_state": terminal.state,
        "terminal_state": terminal.state,
        "terminal_state_source": terminal.source,
        "terminal_state_reason": terminal.reason,
        "ever_states": ever_states,
        "first_seen": sorted_events[0].get("timestamp") if sorted_events else None,
        "last_seen": sorted_events[-1].get("timestamp") if sorted_events else None,
    }


def no_head_analytics(pr: int, events: list[dict[str, Any]]) -> dict[str, Any]:
    return head_analytics(pr, "", events)


def pr_analytics(accumulator: PrAccumulator) -> dict[str, Any]:
    head_rows = [
        head_analytics(accumulator.pr, head_sha, events)
        for head_sha, events in accumulator.head_events.items()
    ]
    no_head_events = [event for event in accumulator.events if not event.get("head_sha")]
    if not head_rows and no_head_events:
        head_rows.append(no_head_analytics(accumulator.pr, no_head_events))
    head_rows.sort(key=lambda item: (item.get("last_seen") or "", item.get("head_sha") or ""))
    latest = head_rows[-1] if head_rows else {
        "terminal_state": "unknown",
        "terminal_state_source": "none",
        "terminal_state_reason": "no_events",
        "ever_states": {},
    }
    all_events_for_pr = sorted(accumulator.events, key=event_sort_key)
    ever_states: dict[str, int] = {}
    for row in head_rows:
        for state, count in row["ever_states"].items():
            ever_states[state] = ever_states.get(state, 0) + count
    observed_heads = len([head_sha for head_sha in accumulator.head_events if head_sha])
    return {
        "pr": accumulator.pr,
        "contributor_login": accumulator.contributor_login,
        "observed_heads": observed_heads,
        "latest_head_sha": latest.get("head_sha") or None,
        "head_states": head_rows,
        "terminal_state": latest["terminal_state"],
        "terminal_state_source": latest["terminal_state_source"],
        "terminal_state_reason": latest["terminal_state_reason"],
        "ever_states": ever_states,
        "quality_signals": accumulator.quality_signals,
        "first_seen": all_events_for_pr[0].get("timestamp") if all_events_for_pr else None,
        "last_seen": all_events_for_pr[-1].get("timestamp") if all_events_for_pr else None,
        "is_open_or_pending": latest["terminal_state"] not in TERMINAL_STATES,
        "event_count": len(all_events_for_pr),
    }


def rate(numerator: int, denominator: int) -> float | None:
    if denominator == 0:
        return None
    return numerator / denominator


def percentile(value: float | None) -> str:
    if value is None:
        return "-"
    return f"{round(value * 100):d}%"


def average(values: list[int]) -> float:
    if not values:
        return 0.0
    return sum(values) / len(values)


def confidence_for(total_prs: int, terminal_prs: int, unclassified_ratio: float, refreshed: bool) -> str:
    if total_prs == 0 or terminal_prs < 2 or unclassified_ratio > 0.35:
        return "low"
    if refreshed and terminal_prs >= 8 and unclassified_ratio <= 0.10:
        return "high"
    if terminal_prs >= 5 and unclassified_ratio <= 0.20:
        return "medium"
    return "low"


def score_label(score: int, confidence: str) -> str:
    if confidence == "low":
        return "Insufficient Data"
    if score >= 90:
        return "Excellent Signal"
    if score >= 75:
        return "Strong Signal"
    if score >= 55:
        return "Watch"
    return "Elevated Scrutiny"


def contributor_score(
    success_rate: float | None,
    block_rate: float | None,
    avg_observed_heads: float,
    repo_median_heads: float,
    quality_signals: dict[str, int],
) -> dict[str, Any]:
    success_component = 0 if success_rate is None else round((success_rate - 0.5) * 30)
    block_penalty = 0 if block_rate is None else round(block_rate * 30)
    observed_head_penalty = max(0, round((avg_observed_heads - repo_median_heads) * 6))
    signal_penalty = sum(
        QUALITY_SIGNAL_WEIGHTS.get(signal, 5) * count
        for signal, count in quality_signals.items()
    )
    clean_bonus = 5 if success_rate is not None and success_rate >= 0.85 and signal_penalty == 0 else 0
    score = 65 + success_component - block_penalty - observed_head_penalty - signal_penalty + clean_bonus
    score = min(100, max(0, score))
    return {
        "score": score,
        "components": {
            "baseline": 65,
            "success_component": success_component,
            "block_penalty": block_penalty,
            "observed_head_penalty": observed_head_penalty,
            "quality_signal_penalty": signal_penalty,
            "clean_bonus": clean_bonus,
        },
    }


def contributor_analytics(
    login: str,
    prs: list[dict[str, Any]],
    quality_signals: dict[str, int],
    repo_median_heads: float,
    refreshed: bool,
    min_prs: int,
) -> dict[str, Any]:
    terminal = [pr for pr in prs if pr["terminal_state"] in TERMINAL_STATES]
    successes = [pr for pr in terminal if pr["terminal_state"] in SUCCESS_STATES]
    blocks = [
        pr for pr in prs
        if any(state in pr["ever_states"] for state in BLOCK_STATES)
    ]
    holds = [
        pr for pr in prs
        if any(state in pr["ever_states"] for state in HOLD_STATES)
    ]
    deferred = [pr for pr in prs if pr["terminal_state"] == "deferred"]
    observed_heads = [pr["observed_heads"] for pr in prs]
    unknown_events = sum(pr["ever_states"].get("unknown", 0) for pr in prs)
    total_events = sum(sum(pr["ever_states"].values()) for pr in prs)
    unclassified_ratio = (unknown_events / total_events) if total_events else 0.0
    success = rate(len(successes), len(terminal))
    block = rate(len(blocks), len(prs))
    score_data = contributor_score(
        success,
        block,
        average(observed_heads),
        repo_median_heads,
        quality_signals,
    )
    confidence = confidence_for(len(prs), len(terminal), unclassified_ratio, refreshed)
    if len(prs) < min_prs:
        confidence = "low"
    top_signals = sorted(
        quality_signals.items(),
        key=lambda item: (-item[1], item[0]),
    )[:5]
    return {
        "login": login,
        "prs": len(prs),
        "terminal_prs": len(terminal),
        "accepted_or_enqueued": len(successes),
        "observed_success_rate": success,
        "observed_heads_avg": round(average(observed_heads), 2),
        "observed_heads_median": median(observed_heads) if observed_heads else 0,
        "blocks": len(blocks),
        "holds": len(holds),
        "deferred": len(deferred),
        "quality_signals": quality_signals,
        "top_signals": [{"signal": signal, "count": count} for signal, count in top_signals],
        "local_signal_score": score_data["score"],
        "score_components": score_data["components"],
        "confidence": confidence,
        "score_label": score_label(score_data["score"], confidence),
        "unclassified_ratio": round(unclassified_ratio, 3),
        "first_seen": min((pr["first_seen"] for pr in prs if pr.get("first_seen")), default=None),
        "last_seen": max((pr["last_seen"] for pr in prs if pr.get("last_seen")), default=None),
        "recent_prs": sorted(prs, key=lambda pr: (pr.get("last_seen") or "", pr["pr"]))[-5:],
    }


def contributor_rows_from_prs(
    prs: list[dict[str, Any]],
    contributor_quality: dict[str, dict[str, int]],
    repo_median_heads: float,
    refreshed: bool,
    min_prs: int,
    author: str | None,
) -> list[dict[str, Any]]:
    contributor_prs: dict[str, list[dict[str, Any]]] = {}
    for pr in prs:
        contributor_prs.setdefault(pr["contributor_login"], []).append(pr)
    contributors = []
    for login, login_prs in contributor_prs.items():
        contributors.append(
            contributor_analytics(
                login,
                sorted(login_prs, key=lambda item: item["pr"]),
                contributor_quality.get(login, {}),
                repo_median_heads,
                refreshed,
                min_prs,
            )
        )
    for login, signals in contributor_quality.items():
        if author and login.lower() != author.lower():
            continue
        if login not in contributor_prs:
            contributors.append(
                contributor_analytics(
                    login,
                    [],
                    signals,
                    repo_median_heads,
                    refreshed,
                    min_prs,
                )
            )
    return contributors


def build_pr_contributor_map(events: list[dict[str, Any]]) -> dict[int, str]:
    contributors: dict[int, str] = {}
    for event in sorted(events, key=event_sort_key):
        pr = event.get("pr")
        if pr is None:
            continue
        login = contributor_login_for_event(event)
        if login:
            contributors.setdefault(int(pr), str(login))
    return contributors


def add_counter(target: dict[str, int], key: str, count: int = 1) -> None:
    target[key] = target.get(key, 0) + count


def build_analytics_model(
    events: list[dict[str, Any]],
    *,
    days: int | None,
    author: str | None,
    min_prs: int,
    include_open: bool,
    refreshed: bool = False,
) -> dict[str, Any]:
    all_sorted_events = sorted(events, key=event_sort_key)
    pr_contributors = build_pr_contributor_map(all_sorted_events)
    filtered_events = filtered_events_by_days(all_sorted_events, days)
    pr_accumulators: dict[int, PrAccumulator] = {}
    contributor_quality: dict[str, dict[str, int]] = {}
    for event in filtered_events:
        event_type = event.get("event_type")
        login = contributor_login_for_event(event)
        if event_type == "quality_entry":
            if login:
                signals = event.get("quality", {}).get("signals", [])
                quality = contributor_quality.setdefault(str(login), {})
                for signal in signals:
                    add_counter(quality, str(signal))
            continue
        pr = event.get("pr")
        if pr is None:
            continue
        pr_number = int(pr)
        contributor = login or pr_contributors.get(pr_number)
        if contributor is None:
            continue
        if author and contributor.lower() != author.lower():
            continue
        accumulator = pr_accumulators.setdefault(
            pr_number,
            PrAccumulator(pr_number, contributor, [], {}, {}),
        )
        accumulator.events.append(event)
        head_sha = event.get("head_sha")
        if head_sha:
            accumulator.head_events.setdefault(str(head_sha), []).append(event)
    for pr_number, accumulator in pr_accumulators.items():
        quality = contributor_quality.get(accumulator.contributor_login, {})
        accumulator.quality_signals.update(quality)
    prs = [pr_analytics(accumulator) for accumulator in pr_accumulators.values()]
    if not include_open:
        prs = [pr for pr in prs if not pr["is_open_or_pending"]]
    observed_head_values = [pr["observed_heads"] for pr in prs if pr["observed_heads"] > 0]
    repo_median_heads = median(observed_head_values) if observed_head_values else 0
    contributor_signals = {
        login: dict(signals) for login, signals in contributor_quality.items()
        if author is None or login.lower() == author.lower()
    }
    contributors = contributor_rows_from_prs(
        prs,
        contributor_signals,
        repo_median_heads,
        refreshed,
        min_prs,
        author,
    )
    return {
        "generated_at": now_iso(),
        "mode": "github_refreshed" if refreshed else "local_observed",
        "title": "Local Observed Review Analytics",
        "filters": {
            "author": author,
            "days": days,
            "min_prs": min_prs,
            "include_open": include_open,
        },
        "repo_medians": {"observed_heads": repo_median_heads},
        "contributors": contributors,
        "prs": prs,
        "quality_by_contributor": contributor_signals,
        "unclassified_counts": unknown_event_values(filtered_events),
        "audit_counts": audit_event_values(filtered_events),
        "warnings": [],
    }


def score_bar(score: int, width: int = 12) -> str:
    filled = round((score / 100) * width)
    return "#" * filled + "." * (width - filled)


def sorted_contributors(
    contributors: list[dict[str, Any]],
    sort_key: str,
    limit: int | None,
) -> list[dict[str, Any]]:
    def confidence_rank(item: dict[str, Any]) -> int:
        return {"high": 0, "medium": 1, "low": 2}.get(item["confidence"], 3)

    if sort_key == "activity":
        key = lambda item: (confidence_rank(item), -item["prs"], item["login"].lower())
    elif sort_key == "acceptance":
        key = lambda item: (
            confidence_rank(item),
            -(item["observed_success_rate"] or 0),
            item["login"].lower(),
        )
    elif sort_key == "observed-heads":
        key = lambda item: (
            confidence_rank(item),
            -item["observed_heads_avg"],
            item["login"].lower(),
        )
    else:
        key = lambda item: (
            confidence_rank(item),
            -item["local_signal_score"],
            item["login"].lower(),
        )
    rows = sorted(contributors, key=key)
    return rows[:limit] if limit is not None else rows


def render_top_signals(contributor: dict[str, Any]) -> str:
    signals = contributor.get("top_signals", [])
    if not signals:
        return "-"
    return ",".join(f"{item['signal']}:{item['count']}" for item in signals[:3])


def confidence_display(value: str) -> str:
    return {"high": "high", "medium": "med", "low": "low"}.get(value, value[:5])


def render_analytics_table(model: dict[str, Any], *, sort_key: str, limit: int | None) -> str:
    rows = sorted_contributors(model["contributors"], sort_key, limit)
    output = [
        model["title"],
        "Note: local observed data; use --refresh-github for authoritative merge/close state.",
        "",
        "Contributor           PRs Term Succ% Heads Blocks Holds Score Conf  Signal        TopSignals",
        "-------------------- ---- ---- ----- ----- ------ ----- ----- ----- ------------- ----------------",
    ]
    for row in rows:
        output.append(
            f"{row['login'][:20]:20} "
            f"{row['prs']:4d} "
            f"{row['terminal_prs']:4d} "
            f"{percentile(row['observed_success_rate']):>5} "
            f"{row['observed_heads_avg']:5.1f} "
            f"{row['blocks']:6d} "
            f"{row['holds']:5d} "
            f"{row['local_signal_score']:5d} "
            f"{confidence_display(row['confidence']):5} "
            f"{score_bar(row['local_signal_score']):13} "
            f"{render_top_signals(row)}"
        )
    if not rows:
        output.append("(no contributors matched)")
    return "\n".join(output)


def render_count_bar(label: str, value: int, max_value: int) -> str:
    width = 24
    filled = 0 if max_value == 0 else round((value / max_value) * width)
    return f"{label:18} {value:4d} {'#' * filled}{'.' * (width - filled)}"


def render_contributor_detail(model: dict[str, Any], login: str) -> str:
    matches = [
        contributor for contributor in model["contributors"]
        if contributor["login"].lower() == login.lower()
    ]
    if not matches:
        return f"No analytics found for {login}."
    row = matches[0]
    components = row["score_components"]
    max_count = max(row["accepted_or_enqueued"], row["blocks"], row["holds"], row["deferred"], 1)
    lines = [
        f"{row['login']} - {model['title']}",
        "Note: local observed data; use --refresh-github for authoritative merge/close state.",
        "",
        f"Local Signal Score: {row['local_signal_score']} / 100 ({row['score_label']}, confidence: {row['confidence']})",
        f"PRs: {row['prs']}  Terminal: {row['terminal_prs']}  Observed success: {percentile(row['observed_success_rate'])}",
        f"Observed heads avg: {row['observed_heads_avg']}  median: {row['observed_heads_median']}",
        "",
        "Score Components",
    ]
    for name, value in components.items():
        lines.append(f"  {name:24} {value}")
    lines.extend(
        [
            "",
            "Outcomes",
            render_count_bar("accepted/enqueued", row["accepted_or_enqueued"], max_count),
            render_count_bar("blocks", row["blocks"], max_count),
            render_count_bar("holds", row["holds"], max_count),
            render_count_bar("deferred", row["deferred"], max_count),
            "",
            "Top Signals",
        ]
    )
    if row["top_signals"]:
        for signal in row["top_signals"]:
            lines.append(f"  {signal['signal']}: {signal['count']}")
    else:
        lines.append("  -")
    lines.append("")
    lines.append("Recent PRs")
    for pr in row["recent_prs"]:
        lines.append(
            f"  #{pr['pr']} state={pr['terminal_state']} "
            f"heads={pr['observed_heads']} last={pr.get('last_seen') or '-'}"
        )
    if not row["recent_prs"]:
        lines.append("  -")
    return "\n".join(lines)


def render_analytics_ascii(model: dict[str, Any], args: argparse.Namespace) -> str:
    if args.author:
        return render_contributor_detail(model, args.author)
    return render_analytics_table(model, sort_key=args.sort, limit=args.limit)


def gh_pr_analytics_state(repo: str, pr_number: int) -> dict[str, Any]:
    fields = "number,state,author,headRefOid,reviewDecision,mergedAt,closedAt"
    return run_json(["gh", "pr", "view", str(pr_number), "--repo", repo, "--json", fields])


def apply_github_refresh(model: dict[str, Any], repo: str, min_prs: int, author: str | None) -> None:
    warnings = model.setdefault("warnings", [])
    refreshed = 0
    for pr in model["prs"]:
        try:
            live = gh_pr_analytics_state(repo, int(pr["pr"]))
        except subprocess.CalledProcessError as exc:
            warnings.append(f"failed to refresh PR {pr['pr']}: {exc}")
            continue
        if not isinstance(live, dict):
            warnings.append(f"failed to refresh PR {pr['pr']}: empty or invalid response")
            continue
        refreshed += 1
        state = str(live.get("state") or "").upper()
        pr["github"] = {
            "state": state,
            "author_login": (live.get("author") or {}).get("login"),
            "headRefOid": live.get("headRefOid"),
            "reviewDecision": live.get("reviewDecision"),
            "mergedAt": live.get("mergedAt"),
            "closedAt": live.get("closedAt"),
        }
        if state == "MERGED":
            pr["terminal_state"] = "merged"
            pr["terminal_state_source"] = "github.state"
            pr["terminal_state_reason"] = "merged"
            pr["is_open_or_pending"] = False
        elif state == "CLOSED":
            pr["terminal_state"] = "closed"
            pr["terminal_state_source"] = "github.state"
            pr["terminal_state_reason"] = "closed"
            pr["is_open_or_pending"] = False
    model["mode"] = "github_refreshed"
    model["title"] = "GitHub Refreshed Review Analytics"
    model["github_refreshed_prs"] = refreshed
    model["contributors"] = contributor_rows_from_prs(
        model["prs"],
        model.get("quality_by_contributor", {}),
        float(model["repo_medians"]["observed_heads"]),
        True,
        min_prs,
        author,
    )


def filter_open_prs(model: dict[str, Any], min_prs: int, author: str | None, refreshed: bool) -> None:
    model["prs"] = [pr for pr in model["prs"] if not pr["is_open_or_pending"]]
    observed_head_values = [pr["observed_heads"] for pr in model["prs"] if pr["observed_heads"] > 0]
    model["repo_medians"]["observed_heads"] = median(observed_head_values) if observed_head_values else 0
    model["contributors"] = contributor_rows_from_prs(
        model["prs"],
        model.get("quality_by_contributor", {}),
        float(model["repo_medians"]["observed_heads"]),
        refreshed,
        min_prs,
        author,
    )


def matches_any(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatch(path, pattern) for pattern in patterns)


def classify_files(files: list[str], policy: Policy) -> dict[str, Any]:
    hard_stops = [path for path in files if matches_any(path, policy.hard_stop_patterns)]
    generated = [path for path in files if matches_any(path, policy.generated_patterns)]
    classes: dict[str, list[str]] = {}
    for name, patterns in policy.path_classes.items():
        matched = [path for path in files if matches_any(path, patterns)]
        if matched:
            classes[name] = matched

    if hard_stops:
        surface = "hard_stop"
        gate = "hard_stop"
    elif classes and set(classes) == {"frontend"}:
        surface = "frontend"
        gate = "policy"
    elif "frontend" in classes and len(classes) > 1:
        surface = "mixed"
        gate = "policy"
    elif "engine" in classes:
        surface = "backend"
        gate = "review"
    else:
        surface = "unknown"
        gate = "review"

    return {
        "surface": surface,
        "gate": gate,
        "hard_stop_paths": hard_stops,
        "generated_paths": generated,
        "path_classes": classes,
    }


def status_summary(checks: list[dict[str, Any]]) -> dict[str, Any]:
    pending = []
    failures = []
    successes = []
    for check in checks:
        name = check.get("name", "<unknown>")
        status = check.get("status")
        conclusion = (check.get("conclusion") or "").upper()
        if status != "COMPLETED":
            pending.append(name)
        elif conclusion not in {"SUCCESS", "SKIPPED", "NEUTRAL"}:
            failures.append(name)
        else:
            successes.append(name)
    if failures:
        state = "failed"
    elif pending:
        state = "pending"
    elif successes:
        state = "green"
    else:
        state = "unknown"
    return {"state": state, "pending": pending, "failures": failures, "successes": successes}


def pr_files_from_view(pr: dict[str, Any]) -> list[str]:
    return [item["path"] for item in pr.get("files", []) if item.get("path")]


def latest_review_commit(pr: dict[str, Any], acting_login: str) -> str | None:
    reviews = [
        review
        for review in (pr.get("reviews") or pr.get("latestReviews") or [])
        if review.get("author", {}).get("login") == acting_login
    ]
    if not reviews:
        return None
    reviews.sort(key=lambda review: review.get("submittedAt") or "")
    commit = reviews[-1].get("commit") or {}
    return commit.get("oid") or None


def compact_pr_view(pr: dict[str, Any], acting_login: str) -> dict[str, Any]:
    author_login = pr.get("author", {}).get("login")
    return {
        "number": pr.get("number"),
        "title": pr.get("title"),
        "state": pr.get("state"),
        "isDraft": pr.get("isDraft"),
        "url": pr.get("url"),
        "author_login": author_login,
        "self_authored": author_login == acting_login,
        "headRefName": pr.get("headRefName"),
        "headRefOid": pr.get("headRefOid"),
        "baseRefName": pr.get("baseRefName"),
        "mergeStateStatus": pr.get("mergeStateStatus"),
        "reviewDecision": pr.get("reviewDecision"),
        "isInMergeQueue": pr.get("isInMergeQueue"),
        "mergeQueueEntry": pr.get("mergeQueueEntry"),
        "autoMergeRequest": pr.get("autoMergeRequest"),
        "labels": [label.get("name") for label in pr.get("labels", [])],
        "assignees": [assignee.get("login") for assignee in pr.get("assignees", [])],
        "body_hash": text_hash(pr.get("body")),
        "body_excerpt": excerpt(pr.get("body"), 800),
        "comments": [
            {
                "author": comment.get("author", {}).get("login"),
                "createdAt": comment.get("createdAt"),
                "body_hash": text_hash(comment.get("body")),
                "body_excerpt": excerpt(comment.get("body"), 300),
            }
            for comment in pr.get("comments", [])
        ],
        "reviews": [
            {
                "author": review.get("author", {}).get("login"),
                "state": review.get("state"),
                "submittedAt": review.get("submittedAt"),
                "commit": (review.get("commit") or {}).get("oid"),
                "body_hash": text_hash(review.get("body")),
                "body_excerpt": excerpt(review.get("body"), 300),
            }
            for review in pr.get("reviews", [])
        ],
    }


def recommend_from_packet(packet: dict[str, Any]) -> dict[str, Any]:
    pr = packet["pr"]
    head = pr.get("headRefOid")
    classification = packet.get("classification", {})
    latest_commit = packet.get("latest_maintainer_review_commit")
    review_decision = pr.get("reviewDecision")
    queue = bool(
        pr.get("isInMergeQueue") or pr.get("mergeQueueEntry") or pr.get("autoMergeRequest")
    )
    local_event = packet.get("local_current_event") or {}
    local_event_type = local_event.get("event_type")
    local_outcome = local_event.get("outcome")
    author_policy = packet.get("author_policy", {})
    local_block_event = local_outcome != "ci_failed" and local_event_type in {
        "review_blocked",
        "changes_requested",
        "blocked",
    }
    local_block_outcome = local_outcome in {
        "changes_requested",
        "reviewed_request_changes",
        "blocked",
    }

    if pr.get("state") == "MERGED":
        action = "merged_prune"
        reason = "merged"
    elif pr.get("state") == "CLOSED":
        action = "skip"
        reason = "closed"
    elif pr.get("self_authored"):
        action = "skip"
        reason = "self_authored"
    elif classification.get("hard_stop_paths"):
        action = "request_changes"
        reason = "hard_stop"
    elif local_outcome == "DEFER-FE":
        action = "defer"
        reason = "local_defer_fe_current_head"
    elif local_block_event or local_block_outcome:
        action = "blocked"
        reason = "local_block_current_head"
    elif latest_commit and latest_commit != head and review_decision == "APPROVED":
        action = "dequeue_stale_for_handler" if queue else "review"
        reason = "stale_approval"
    elif queue and review_decision == "APPROVED":
        action = "queued"
        reason = (
            "already_in_merge_queue"
            if (pr.get("isInMergeQueue") or pr.get("mergeQueueEntry"))
            else "auto_merge_enabled"
        )
    elif classification.get("surface") == "frontend" and not author_policy.get(
        "frontend_review_allowed"
    ):
        action = "defer"
        reason = "frontend_policy"
    elif local_event_type == "approved_enqueued":
        action = "approve_ready_for_handler"
        reason = "local_approved_enqueued_live_check"
    elif review_decision == "CHANGES_REQUESTED" and latest_commit == head:
        action = "blocked"
        reason = "changes_requested_current_head"
    elif review_decision == "CHANGES_REQUESTED":
        action = "review"
        reason = "stale_changes_requested"
    elif review_decision == "APPROVED" and pr.get("mergeStateStatus") == "BEHIND":
        action = "update_branch_for_handler"
        reason = "approved_behind"
    elif review_decision == "APPROVED":
        action = "approve_ready_for_handler"
        reason = "approved_needs_live_queue_check"
    else:
        action = "review"
        reason = "needs_review"

    return {
        "pr": pr.get("number"),
        "head_sha": head,
        "advisory_action": action,
        "reason": reason,
        "requires_live_verification": action.endswith("_for_handler"),
        "policy_trace": packet.get("policy_trace", []),
    }


def make_packet(
    pr: dict[str, Any],
    policy: Policy,
    acting_login: str,
    mode: str,
    private_overrides: dict[str, Any],
) -> dict[str, Any]:
    files = pr_files_from_view(pr)
    classification = classify_files(files, policy)
    checks = status_summary(pr.get("statusCheckRollup", []))
    compact_pr = compact_pr_view(pr, acting_login)
    author_policy = {
        "frontend_review_allowed": frontend_review_allowed(
            compact_pr.get("author_login"), private_overrides
        )
    }
    packet = {
        "schema_version": 1,
        "completeness": "complete" if mode == "full" else "triage",
        "acting_login": acting_login,
        "pr": compact_pr,
        "files": files,
        "classification": classification,
        "ci": checks,
        "latest_maintainer_review_commit": latest_review_commit(pr, acting_login),
        "domain": {"rules_domain": policy.rules_domain},
        "author_policy": author_policy,
        "policy_trace": policy_trace(classification),
    }
    packet["recommendation"] = recommend_from_packet(packet)
    return packet


def policy_trace(classification: dict[str, Any]) -> list[str]:
    trace = ["hard_stop", "safety_queue_freshness", "private_override", "standing", "path_policy", "default"]
    if classification.get("hard_stop_paths"):
        trace.append("matched:hard_stop")
    if classification.get("surface") == "frontend":
        trace.append("matched:frontend")
    if classification.get("surface") == "mixed":
        trace.append("matched:mixed")
    return trace


def gh_pr_view(repo: str, pr_number: int) -> dict[str, Any]:
    fields = (
        "number,title,body,state,isDraft,url,author,createdAt,updatedAt,headRefName,headRefOid,"
        "baseRefName,mergeStateStatus,reviewDecision,labels,assignees,"
        "statusCheckRollup,latestReviews,reviews,comments,files"
    )
    pr = run_json(["gh", "pr", "view", str(pr_number), "--repo", repo, "--json", fields])
    pr.update(gh_queue_state(repo, pr_number))
    return pr


def gh_queue_state(repo: str, pr_number: int) -> dict[str, Any]:
    owner, name = repo.split("/", 1)
    query = (
        "query($owner:String!,$repo:String!,$number:Int!){"
        "repository(owner:$owner,name:$repo){"
        "pullRequest(number:$number){"
        "isInMergeQueue mergeQueueEntry{position state} autoMergeRequest{enabledAt}"
        "}}}"
    )
    try:
        result = run_json(
            [
                "gh",
                "api",
                "graphql",
                "-f",
                f"owner={owner}",
                "-f",
                f"repo={name}",
                "-F",
                f"number={pr_number}",
                "-f",
                f"query={query}",
            ]
        )
    except subprocess.CalledProcessError:
        return {"isInMergeQueue": None, "mergeQueueEntry": None, "autoMergeRequest": None}
    pull = result.get("data", {}).get("repository", {}).get("pullRequest", {})
    return {
        "isInMergeQueue": pull.get("isInMergeQueue"),
        "mergeQueueEntry": pull.get("mergeQueueEntry"),
        "autoMergeRequest": pull.get("autoMergeRequest"),
    }


def command_scan(args: argparse.Namespace) -> int:
    policy = load_policy(args.config)
    private_overrides = load_private_overrides(args.state_dir)
    acting_login = args.acting_login or gh_user()
    local_events = latest_events_by_pr_head(args.state_dir)
    prs = run_json(
        [
            "gh",
            "pr",
            "list",
            "--repo",
            args.repo,
            "--state",
            "open",
            "--limit",
            str(args.limit),
            "--json",
            "number,title,author,createdAt,updatedAt,headRefOid,isDraft,mergeStateStatus,reviewDecision,latestReviews,labels,statusCheckRollup,files",
        ]
    )
    candidates = []
    for pr in prs:
        pr_number = int(pr["number"])
        packet = make_packet(pr, policy, acting_login, "light", private_overrides)
        packet["local_current_event"] = local_events.get((pr_number, pr.get("headRefOid") or ""))
        packet["recommendation"] = recommend_from_packet(packet)
        if packet["recommendation"]["reason"] in {
            "stale_changes_requested",
            "stale_approval",
        } or packet["recommendation"]["advisory_action"] in {
            "approve_ready_for_handler",
            "update_branch_for_handler",
            "dequeue_stale_for_handler",
        }:
            pr = gh_pr_view(args.repo, pr_number)
            packet = make_packet(pr, policy, acting_login, "full", private_overrides)
            packet["local_current_event"] = local_events.get(
                (pr_number, pr.get("headRefOid") or "")
            )
            packet["recommendation"] = recommend_from_packet(packet)
        candidates.append(
            {
                "pr": pr.get("number"),
                "title": pr.get("title"),
                "created_at": pr.get("createdAt"),
                "updated_at": pr.get("updatedAt"),
                "head_sha": pr.get("headRefOid"),
                "author_login": packet["pr"].get("author_login"),
                "self_authored": packet["pr"].get("self_authored"),
                "surface": packet["classification"]["surface"],
                "gate": packet["classification"]["gate"],
                "hard_stop_paths": packet["classification"]["hard_stop_paths"],
                "ci": packet["ci"]["state"],
                "review_decision": pr.get("reviewDecision"),
                "is_in_merge_queue": packet["pr"].get("isInMergeQueue"),
                "merge_queue_entry": packet["pr"].get("mergeQueueEntry"),
                "auto_merge_request": packet["pr"].get("autoMergeRequest"),
                "advisory_action": packet["recommendation"]["advisory_action"],
                "reason": packet["recommendation"]["reason"],
                "policy_trace": packet["policy_trace"],
            }
        )

    action_order = {
        "dequeue_stale_for_handler": 0,
        "update_branch_for_handler": 1,
        "approve_ready_for_handler": 2,
        "review": 3,
        "hold_ci": 4,
        "request_changes": 5,
        "blocked": 6,
        "defer": 7,
        "queued": 8,
        "merged_prune": 9,
        "skip": 10,
    }

    def candidate_sort_key(candidate: dict[str, Any]) -> tuple[Any, ...]:
        action = candidate.get("advisory_action") or ""
        created = candidate.get("created_at") or ""
        updated = candidate.get("updated_at") or ""
        pr_number = candidate.get("pr") or 0
        if action == "review":
            return (action_order.get(action, 99), created, pr_number)
        if action in {"dequeue_stale_for_handler", "update_branch_for_handler", "approve_ready_for_handler"}:
            return (action_order.get(action, 99), updated, created, pr_number)
        return (action_order.get(action, 99), pr_number)

    candidates.sort(key=candidate_sort_key)
    candidates_by_action: dict[str, list[dict[str, Any]]] = {}
    for candidate in candidates:
        candidates_by_action.setdefault(candidate["advisory_action"], []).append(candidate)
    action_counts = {action: len(items) for action, items in candidates_by_action.items()}
    print(
        json_dumps(
            {
                "acting_login": acting_login,
                "completeness": "triage",
                "action_counts": action_counts,
                "candidates_by_action": candidates_by_action,
                "candidates": candidates,
            }
        )
    )
    return 0


def command_inspect(args: argparse.Namespace) -> int:
    policy = load_policy(args.config)
    private_overrides = load_private_overrides(args.state_dir)
    acting_login = args.acting_login or gh_user()
    pr = gh_pr_view(args.repo, args.pr)
    packet = make_packet(pr, policy, acting_login, args.mode, private_overrides)
    packet["local_current_event"] = latest_events_by_pr_head(args.state_dir).get(
        (args.pr, pr.get("headRefOid") or "")
    )
    packet["recommendation"] = recommend_from_packet(packet)
    print(json_dumps(packet))
    return 0


def command_recommend(args: argparse.Namespace) -> int:
    policy = load_policy(args.config)
    private_overrides = load_private_overrides(args.state_dir)
    acting_login = args.acting_login or gh_user()
    pr = gh_pr_view(args.repo, args.pr)
    packet = make_packet(pr, policy, acting_login, "full", private_overrides)
    packet["local_current_event"] = latest_events_by_pr_head(args.state_dir).get(
        (args.pr, pr.get("headRefOid") or "")
    )
    packet["recommendation"] = recommend_from_packet(packet)
    recommendation = packet["recommendation"]
    if packet["completeness"] != "complete" and recommendation["advisory_action"].endswith("_for_handler"):
        recommendation = {
            "pr": args.pr,
            "head_sha": pr.get("headRefOid"),
            "advisory_action": "hold_ci",
            "reason": "insufficient_data",
            "requires_live_verification": False,
            "policy_trace": packet.get("policy_trace", []),
        }
    print(json_dumps(recommendation))
    return 0


def read_event_arg(value: str) -> dict[str, Any]:
    if value == "-":
        return json.loads(sys.stdin.read())
    return json.loads(Path(value).read_text(encoding="utf-8"))


def command_record(args: argparse.Namespace) -> int:
    event = read_event_arg(args.event_json)
    state_dir = args.state_dir
    inserted = append_event(state_dir, event)
    print(json_dumps({"inserted": inserted, "event_id": normalize_event(event)["event_id"]}))
    return 0


def tsv_import_events(path: Path) -> list[dict[str, Any]]:
    events = []
    with path.open("r", encoding="utf-8", newline="") as file:
        reader = csv.DictReader(file, delimiter="\t")
        for line_number, row in enumerate(reader, start=2):
            pr_raw = row.get("pr") or ""
            if not pr_raw.isdigit():
                continue
            events.append(
                {
                    "event_type": "tracker_row",
                    "timestamp": row.get("timestamp") or now_iso(),
                    "pr": int(pr_raw),
                    "author": row.get("author") or None,
                    "head_sha": row.get("head_sha") or None,
                    "source": {"file": str(path), "line": line_number},
                    "tracker": row,
                }
            )
    return events


def quality_import_events(path: Path) -> list[dict[str, Any]]:
    events = []
    current_login: str | None = None
    current_lines: list[str] = []
    start_line = 0
    lines = path.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines, start=1):
        if line.startswith("### "):
            if current_login:
                events.append(quality_entry(path, start_line, current_login, current_lines))
            heading = line[4:].strip()
            current_login = heading.split("—", 1)[0].strip().split()[0]
            current_lines = [line]
            start_line = index
        elif current_login:
            current_lines.append(line)
    if current_login:
        events.append(quality_entry(path, start_line, current_login, current_lines))
    return events


def quality_entry(path: Path, line_number: int, login: str, lines: list[str]) -> dict[str, Any]:
    body = "\n".join(lines).strip()
    signals = []
    for token in [
        "runtime-test-gap",
        "false-green",
        "fmt/clippy-slip",
        "wrong-seam",
        "rebase-not-fix",
        "scope-contamination",
        "build-for-card",
        "stale-approval",
    ]:
        if token in body:
            signals.append(token)
    return {
        "event_type": "quality_entry",
        "timestamp": now_iso(),
        "author": login,
        "source": {"file": str(path), "line": line_number},
        "confidence": "low",
        "quality": {
            "login": login,
            "signals": signals,
            "summary": body[:1200],
        },
    }


def command_import(args: argparse.Namespace) -> int:
    count = 0
    if args.tracker:
        for event in tsv_import_events(args.tracker):
            count += 1 if append_event(args.state_dir, event) else 0
    if args.quality:
        for event in quality_import_events(args.quality):
            count += 1 if append_event(args.state_dir, event) else 0
    print(json_dumps({"inserted": count, "state_dir": str(args.state_dir)}))
    return 0


def command_rebuild_index(args: argparse.Namespace) -> int:
    rebuild_index(args.state_dir)
    print(json_dumps({"rebuilt": True, "state_dir": str(args.state_dir)}))
    return 0


def command_check_skill_sync(args: argparse.Namespace) -> int:
    canonical = args.canonical
    mirror = args.mirror
    canonical_bytes = canonical.read_bytes()
    mirror_bytes = mirror.read_bytes()
    synced = canonical_bytes == mirror_bytes
    print(json_dumps({"synced": synced, "canonical": str(canonical), "mirror": str(mirror)}))
    return 0 if synced else 1


def command_compact(args: argparse.Namespace) -> int:
    rebuild_index(args.state_dir)
    events = all_events(args.state_dir)
    prs: dict[str, dict[str, Any]] = {}
    contributors: dict[str, dict[str, Any]] = {}
    for event in events:
        pr = event.get("pr")
        author = event.get("author")
        if pr is not None:
            key = str(pr)
            prs[key] = {
                "pr": pr,
                "head_sha": event.get("head_sha") or prs.get(key, {}).get("head_sha"),
                "latest_event": event.get("event_type"),
                "latest_timestamp": event.get("timestamp"),
                "verdict": event.get("tracker", {}).get("verdict") or prs.get(key, {}).get("verdict"),
            }
        if author:
            entry = contributors.setdefault(
                author,
                {"login": author, "events": 0, "signals": {}, "latest_timestamp": None},
            )
            entry["events"] += 1
            entry["latest_timestamp"] = event.get("timestamp")
            for signal in event.get("quality", {}).get("signals", []):
                entry["signals"][signal] = entry["signals"].get(signal, 0) + 1
    summary = {
        "generated_at": now_iso(),
        "prs": sorted(prs.values(), key=lambda item: item["pr"]),
        "contributors": sorted(contributors.values(), key=lambda item: item["login"].lower()),
    }
    output = args.state_dir / "review-summary.json"
    output.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json_dumps({"summary": str(output), "prs": len(prs), "contributors": len(contributors)}))
    return 0


def command_analytics(args: argparse.Namespace) -> int:
    rebuild_index(args.state_dir)
    events = all_events(args.state_dir)
    model = build_analytics_model(
        events,
        days=args.days,
        author=args.author,
        min_prs=args.min_prs,
        include_open=args.include_open or args.refresh_github,
    )
    if args.refresh_github:
        apply_github_refresh(model, args.repo, args.min_prs, args.author)
        if not args.include_open:
            filter_open_prs(model, args.min_prs, args.author, True)
        model["filters"]["include_open"] = args.include_open
    model["contributors"] = sorted_contributors(model["contributors"], args.sort, args.limit)
    if args.format == "json":
        print(json.dumps(model, indent=2, sort_keys=True))
    else:
        print(render_analytics_ascii(model, args))
    return 0


def existing_path(value: str) -> Path:
    path = Path(value).expanduser()
    if not path.exists():
        raise argparse.ArgumentTypeError(f"{path} does not exist")
    return path


def add_common(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo", default="phase-rs/phase")
    parser.add_argument("--config", type=Path, default=DEFAULT_POLICY)
    parser.add_argument("--state-dir", type=Path, default=None)
    parser.add_argument("--acting-login", default=None)


def add_state(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo", default="phase-rs/phase")
    parser.add_argument("--state-dir", type=Path, default=None)


def finalize_state_dir(args: argparse.Namespace) -> None:
    if getattr(args, "state_dir", None) is None:
        args.state_dir = default_state_dir(getattr(args, "repo", None))
    args.state_dir = args.state_dir.expanduser()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    scan = sub.add_parser("scan")
    add_common(scan)
    scan.add_argument("--limit", type=int, default=100)
    scan.set_defaults(func=command_scan)

    inspect = sub.add_parser("inspect")
    add_common(inspect)
    inspect.add_argument("pr", type=int)
    inspect.add_argument("--mode", choices=["light", "full"], default="light")
    inspect.set_defaults(func=command_inspect)

    recommend = sub.add_parser("recommend")
    add_common(recommend)
    recommend.add_argument("pr", type=int)
    recommend.set_defaults(func=command_recommend)

    record = sub.add_parser("record")
    add_state(record)
    record.add_argument("--event-json", required=True)
    record.set_defaults(func=command_record)

    import_cmd = sub.add_parser("import")
    add_state(import_cmd)
    import_cmd.add_argument("--tracker", type=existing_path)
    import_cmd.add_argument("--quality", type=existing_path)
    import_cmd.set_defaults(func=command_import)

    compact = sub.add_parser("compact")
    add_state(compact)
    compact.set_defaults(func=command_compact)

    analytics = sub.add_parser("analytics")
    add_state(analytics)
    analytics.add_argument("--author")
    analytics.add_argument("--days", type=int, default=None)
    analytics.add_argument("--min-prs", type=int, default=3)
    analytics.add_argument("--format", choices=["ascii", "json"], default="ascii")
    analytics.add_argument(
        "--sort",
        choices=["score", "activity", "acceptance", "observed-heads"],
        default="score",
    )
    analytics.add_argument("--limit", type=int, default=None)
    analytics.add_argument("--include-open", action="store_true")
    analytics.add_argument("--refresh-github", action="store_true")
    analytics.set_defaults(func=command_analytics)

    rebuild = sub.add_parser("rebuild-index")
    add_state(rebuild)
    rebuild.set_defaults(func=command_rebuild_index)

    skill_sync = sub.add_parser("check-skill-sync")
    skill_sync.add_argument(
        "--canonical",
        type=Path,
        default=REPO_ROOT / ".agents/skills/pr-review-loop/SKILL.md",
    )
    skill_sync.add_argument(
        "--mirror",
        type=Path,
        default=REPO_ROOT / ".claude/skills/pr-review-loop/SKILL.md",
    )
    skill_sync.set_defaults(func=command_check_skill_sync)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    finalize_state_dir(args)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
