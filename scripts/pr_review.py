#!/usr/bin/env python3
"""Portable PR review intelligence helper.

This tool keeps durable review memory as an append-only JSONL event log
(`review-events.jsonl`), the sole canonical store; `review-summary.json` is a
derived artifact. It is advisory: GitHub mutations stay in the maintainer
handling skills.

Architecture (one-way data flow, top to bottom):

    GitHub GraphQL (read-only, via `gh`)      Event log (JSONL, append-only)
        fetch_open_prs / gh_pr_view               all_events -> normalize_event
        normalize_graphql_pr                      |         |
              |                                   |    build_analytics_model
              |                                   |    collect_signal_occurrences
              |                              latest_events_by_pr_head
              v                                   v
        make_packet  <---  ReviewContext (policy + overrides + local history)
              |             build_contributor_summary (standing/scrutiny)
              v
        recommend_from_packet (ordered advisory-action ladder)

Commands: `scan` (triage every open PR), `inspect`/`recommend` (one PR),
`record` (validated event append), `import` (legacy TSV/markdown), `compact`
(summary artifact), `analytics` (contributor tables), `check-skill-sync`.

Invariants:
- All GitHub access is read-only and goes through run_json (retried).
- The event log is the only mutable store; append_event is its only writer.
- Every tunable threshold lives in the constants block below, not inline.
- Recommendations are advisory; precedence is the elif ladder in
  recommend_from_packet, ordered so safety (hard_stop) always wins.
"""
from __future__ import annotations

import argparse
import csv
import fcntl
import fnmatch
import hashlib
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from statistics import median
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_POLICY = REPO_ROOT / ".agents/pr-review-policy.toml"
PRIVATE_OVERRIDES = "private-overrides.json"
SUCCESS_STATES = {"accepted", "merged"}
BLOCK_STATES = {"blocked", "changes_requested"}
HOLD_STATES = {"held", "held_ci"}
# Local events that mean "this head is blocked". Single authority for both the
# routing decision and the requested-changes expiry anchor — two copies would drift.
LOCAL_BLOCK_EVENT_TYPES = {"review_blocked", "changes_requested", "blocked"}
LOCAL_BLOCK_OUTCOMES = {"changes_requested", "reviewed_request_changes", "blocked"}
TERMINAL_STATES = SUCCESS_STATES | BLOCK_STATES | {"closed"}
PR_ATTRIBUTED_EVENTS = {
    "approval_enqueue",
    "approved_enqueued",
    "blocked",
    "changes_requested",
    "defer",
    "deferred",
    "decline",
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
    "requested_changes_warning",
    "review",
    "review_blocked",
    "review_correction",
    "review_reopened",
    "stale_changes_closed",
    "tracker_row",
    "update_branch",
}
# Closed vocabulary enforced at record time (see command_record). New events must
# use one of these event types; legacy events already in the log are read via
# canonical_from_text without validation.
ALLOWED_EVENT_TYPES = PR_ATTRIBUTED_EVENTS | {"observation", "quality_entry", "tracker_row"}
# Closed vocabulary for the optional `outcome` field, derived from the high-confidence
# states canonical_from_text recognizes. Enforced (lowercased) at record time.
ALLOWED_OUTCOMES = {
    "changes_requested",
    "blocked",
    "hard_stop",
    "merged",
    "closed",
    "deferred",
    "decline",
    "declined",
    "defer-fe",
    "ci_failed",
    "hold_ci",
    "held",
    "held_ci",
    "approved",
    "approved_enqueued",
    "enqueued",
    "review",
    "pending",
    "accepted",
    "queued",
    "pruned",
    "requested_changes_warning",
    "stale_changes_closed",
}
# Defect signals subtract from the contributor score and are the ONLY signals
# that feed windowed recurrence / scrutiny elevation.
DEFECT_SIGNAL_WEIGHTS = {
    "wrong-seam": 14,
    "false-green": 12,
    "runtime-test-gap": 10,
    "scope-contamination": 10,
    "rebase-not-fix": 8,
    "build-for-card": 8,
    "inert-fix": 8,
    "fmt/clippy-slip": 5,
    "stale-approval": 4,
    "low-effort-risk": 8,
    "author-created-issue-high-bar": 6,
    "no-repro": 6,
    "value-bar": 6,
    "careful-watch": 4,
    "ai-template-gap": 4,
    "unchecked-engine-implementer": 4,
    "wrong-or-stale-cr-annotation": 8,
    "duplicated-domain-vocabulary": 8,
}
# Praise signals add to the contributor score (credit, capped below). They never
# affect recurrence, scrutiny, or the derived-trusted gate — praise softens the
# risk gauge, it does not launder defects.
PRAISE_SIGNAL_WEIGHTS = {
    "right-seam": 6,
    "scope-discipline": 5,
    "discriminating-runtime-test": 5,
    "parameterized-not-proliferated": 6,
    "evidence-backed-pushback": 4,
}
# Total praise credit a contributor's score can earn; keeps volume of praise
# from masking real defect penalties.
PRAISE_CREDIT_CAP = 15
# Write-time vocabulary: `record` accepts exactly these signal tokens.
QUALITY_SIGNAL_VOCAB = frozenset(DEFECT_SIGNAL_WEIGHTS) | frozenset(PRAISE_SIGNAL_WEIGHTS)
# Read-side normalization for tokens already in the append-only log from before
# write-time validation existed. Aliases map to canonical vocabulary; anything
# still non-canonical after aliasing is dropped from derived metrics (and
# surfaced in the analytics model's unknown_signals audit field).
SIGNAL_ALIASES = {
    "runtime-test-present": "discriminating-runtime-test",
    "discriminating-parser-test": "discriminating-runtime-test",
    "gemini-case-finding-refuted": "evidence-backed-pushback",
}
# Recurrence ("same defect, multiple PRs after feedback") and the derived-trusted
# gate both read signal occurrences within this window, so an improving contributor
# ages out of elevation instead of carrying lifetime signals forever.
RECURRENCE_WINDOW_DAYS = 60
# Shared by the analytics CLI default and the packet-path contributor summary so a
# contributor's score/confidence (and thus scrutiny) never diverges between the two.
ANALYTICS_DEFAULT_MIN_PRS = 3
# Closed vocabulary for private-overrides.json contributor_standing entries. Only
# "skip" changes the advisory action; watch/probation force elevated scrutiny;
# trusted marks light-touch eligibility. Anything else in the file is ignored.
ALLOWED_STANDINGS = {"skip", "probation", "watch", "trusted"}
# Score bands. SCORE_WATCH_FLOOR is shared by the score_label display bands and
# the scrutiny ladder (score below it at medium/high confidence elevates).
SCORE_EXCELLENT = 90
SCORE_STRONG = 75
SCORE_WATCH_FLOOR = 55
# Derived-trusted gate: enough terminal history, high success, and a clean
# recurrence window (see build_contributor_summary).
TRUSTED_MIN_TERMINAL_PRS = 5
TRUSTED_MIN_SUCCESS_RATE = 0.85
# Same signal on this many distinct PRs inside RECURRENCE_WINDOW_DAYS.
RECURRENCE_ELEVATED_PRS = 2
RECURRENCE_ATTENTION_PRS = 3
# Non-terminal states that still advance a head's "latest known posture".
PROGRESS_STATES = {"held", "held_ci", "deferred", "review", "pending"}
# GitHub read retry policy (see run_json).
RUN_JSON_ATTEMPTS = 3
RUN_JSON_BACKOFF_SECONDS = (2, 5)
# Keep the repo-wide sweep query comfortably below GitHub's per-request execution
# timeout. Single-PR inspect/recommend still fetch full detail when needed.
SCAN_PAGE_SIZE = 25
# Sticky-comment marker posted by .github/workflows/coverage-parse-diff-comment.yml
# as the first (HTML-comment) line of the parse-detail diff body.
PARSE_DIFF_MARKER = "<!-- coverage-parse-diff -->"
DEFAULT_GITTENSOR_API_URL = "https://api.gittensor.io/prs"
GITTENSOR_CLOSED_ATTENTION_MIN = 20
GITTENSOR_CLOSED_ATTENTION_RATIO = 0.6
AI_CONTRIBUTOR_TEMPLATE_HEADINGS = ("summary", "files changed", "track", "llm", "verification")
REQUIRED_ARTIFACT_H2_HEADINGS = (
    "implementation method (required)",
    "verification",
    "gate a",
    "anchored on",
    "final review-impl",
)
REQUIRED_VERIFICATION_CHECKBOX_LABELS = (
    "Required checks ran clean, or the exact CI-owned alternative is stated below.",
    "Gate A output below is for the current committed head.",
    "Final review-impl below is clean for the current committed head.",
    "Both anchors cite existing analogous code at the same seam.",
)
PROOF_REQUIRED_RISK_FLAGS = {
    "verification-skipped-or-delegated",
    "agent-coauthored-all-commits",
    "gittensor-closed-heavy",
}
PROOF_SKIP_PHRASES = (
    "local verification skipped",
    "no rust toolchain",
    "no local toolchain",
    "see ci status checks",
)
AI_AGENT_COAUTHOR_LOGINS = {"cursoragent"}
REQUESTED_CHANGES_EXPIRY_MARKER = "<!-- pr-review-requested-changes-expiry -->"
DEFAULT_REQUESTED_CHANGES_WARNING_AFTER_DAYS = 7
DEFAULT_REQUESTED_CHANGES_CLOSE_AFTER_WARNING_DAYS = 7
# Sweep-priority order for scan output buckets (lower sorts first).
CANDIDATE_ACTION_ORDER = {
    "close_stale_changes_for_handler": 0,
    "dequeue_stale_for_handler": 0,
    "update_branch_for_handler": 1,
    "approve_ready_for_handler": 2,
    "warn_stale_changes_for_handler": 3,
    "review": 3,
    "hold": 4,
    "hold_ci": 4,
    "request_changes": 5,
    "decline": 5,
    "blocked": 6,
    "defer": 7,
    "queued": 8,
    "merged_prune": 9,
    "skip": 10,
}


# ─── Config, overrides, and small helpers ────────────────────────────────────


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

    @property
    def frontend_deferred_label(self) -> str | None:
        value = self.raw.get("labels", {}).get("frontend_deferred")
        return str(value) if value else None

    @property
    def quality_label(self) -> str | None:
        value = self.raw.get("labels", {}).get("quality")
        return str(value) if value else None

    @property
    def admission_mode(self) -> str:
        return str(self.raw.get("admission", {}).get("mode", "audit"))

    @property
    def admission_enforced_after(self) -> str | None:
        value = self.raw.get("admission", {}).get("enforced_after")
        return str(value) if value else None

    @property
    def architecture_scope_patterns(self) -> list[str]:
        return [
            str(pattern)
            for pattern in self.raw.get("architecture_scope", {}).get("patterns", [])
        ]

    @property
    def architecture_scope_mode(self) -> str:
        return str(self.raw.get("architecture_scope", {}).get("mode", "audit"))

    @property
    def architecture_scope_spans(self) -> dict[str, list[str]]:
        spans = self.raw.get("architecture_scope", {}).get("spans", {})
        if spans:
            return {
                str(name): [str(pattern) for pattern in patterns]
                for name, patterns in spans.items()
            }
        return self.path_classes

    @property
    def architecture_scope_span_threshold(self) -> int:
        return self._positive_int(
            self.raw.get("architecture_scope", {}).get("span_threshold"), 3
        )

    @property
    def architecture_accepted_issue_label(self) -> str | None:
        value = self.raw.get("admission", {}).get("accepted_issue_label")
        return str(value) if value else None

    @property
    def requested_changes_warning_after_days(self) -> int:
        return self._positive_int(
            self.raw.get("requested_changes", {}).get("warning_after_days"),
            DEFAULT_REQUESTED_CHANGES_WARNING_AFTER_DAYS,
        )

    @property
    def requested_changes_close_after_warning_days(self) -> int:
        return self._positive_int(
            self.raw.get("requested_changes", {}).get("close_after_warning_days"),
            DEFAULT_REQUESTED_CHANGES_CLOSE_AFTER_WARNING_DAYS,
        )

    @staticmethod
    def _positive_int(value: Any, default: int) -> int:
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed > 0 else default


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
        policy = Policy(tomllib.load(file))
    validate_policy(policy)
    return policy


def validate_policy(policy: Policy) -> None:
    if policy.admission_mode not in {"audit", "enforce"}:
        raise ValueError(f"invalid admission.mode: {policy.admission_mode!r}")
    if policy.admission_mode == "enforce":
        cutoff = policy.admission_enforced_after
        if (
            cutoff is None
            or not cutoff.endswith("Z")
            or parse_event_datetime(cutoff) is None
        ):
            raise ValueError(
                "admission.mode='enforce' requires a valid UTC admission.enforced_after timestamp ending in Z"
            )
    if policy.architecture_scope_mode not in {"audit", "enforce"}:
        raise ValueError(
            f"invalid architecture_scope.mode: {policy.architecture_scope_mode!r}"
        )


def load_private_overrides(state_dir: Path) -> dict[str, Any]:
    path = state_dir / PRIVATE_OVERRIDES
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def fold_login(login: str) -> str:
    """Case-fold a GitHub login for grouping/lookup; GitHub logins are case-insensitive."""
    return login.lower()


def canonical_signal(token: str) -> str | None:
    """Normalize a logged signal token to canonical vocabulary.

    Single read-side authority: applies legacy aliases, then returns the token
    only if it is canonical (defect or praise). Returns None for anything else —
    the log is append-only, so pre-validation strays are neutralized here rather
    than rewritten.
    """
    resolved = SIGNAL_ALIASES.get(token, token)
    return resolved if resolved in QUALITY_SIGNAL_VOCAB else None


def frontend_review_allowed(author_login: str | None, overrides: dict[str, Any]) -> bool:
    if not author_login:
        return False
    authors = overrides.get("frontend_review_authors", [])
    normalized = {fold_login(str(author)) for author in authors}
    return fold_login(author_login) in normalized


def contributor_standing_override(
    author_login: str, overrides: dict[str, Any]
) -> dict[str, Any] | None:
    standings = overrides.get("contributor_standing") or {}
    folded = fold_login(author_login)
    for login, entry in standings.items():
        if fold_login(str(login)) == folded and isinstance(entry, dict):
            if entry.get("standing") in ALLOWED_STANDINGS:
                return entry
    return None


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


# ─── GitHub subprocess helpers (read-only) ───────────────────────────────────


def run_json(command: list[str]) -> Any:
    """Run a read-only gh query, retrying transient failures with backoff.

    Every caller is a GitHub read (scan pagination, PR view, refresh chunk,
    identity), so retries are idempotent. GitHub can still return transient
    "Something went wrong" / HTTP 5xx errors for GraphQL reads, which `gh`
    reports as a non-zero exit — one such blip must not kill a whole sweep.
    """
    last_error: subprocess.CalledProcessError | None = None
    for attempt in range(RUN_JSON_ATTEMPTS):
        try:
            result = subprocess.run(
                command,
                cwd=REPO_ROOT,
                check=True,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            return json.loads(result.stdout or "null")
        except subprocess.CalledProcessError as exc:
            last_error = exc
            if attempt < RUN_JSON_ATTEMPTS - 1:
                delay = RUN_JSON_BACKOFF_SECONDS[min(attempt, len(RUN_JSON_BACKOFF_SECONDS) - 1)]
                print(
                    f"gh query failed (attempt {attempt + 1}/{RUN_JSON_ATTEMPTS}), "
                    f"retrying in {delay}s: {(exc.stderr or '').strip()[:300]}",
                    file=sys.stderr,
                )
                time.sleep(delay)
    # Surface gh's captured stderr before re-raising — CalledProcessError's own
    # message shows only the command and exit code, which is undiagnosable.
    assert last_error is not None
    print((last_error.stderr or "").strip(), file=sys.stderr)
    raise last_error


def gh_user() -> str:
    return str(run_json(["gh", "api", "user"])["login"])


# ─── Event log (the sole mutable store) ──────────────────────────────────────


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


def append_event(state_dir: Path, event: dict[str, Any]) -> bool:
    normalized = normalize_event(event)
    state_dir.mkdir(parents=True, exist_ok=True)
    log_path = state_dir / "review-events.jsonl"
    # An exclusive flock makes the read-existing-ids-then-append sequence atomic
    # across concurrent agent processes, so simultaneous records can't both write
    # the same event or interleave a partial line into the canonical log.
    with log_path.open("a+", encoding="utf-8") as file:
        fcntl.flock(file, fcntl.LOCK_EX)
        try:
            file.seek(0)
            for line in file:
                if not line.strip():
                    continue
                if json.loads(line).get("event_id") == normalized["event_id"]:
                    return False
            file.write(json_dumps(normalized) + "\n")
            file.flush()
            os.fsync(file.fileno())
            return True
        finally:
            fcntl.flock(file, fcntl.LOCK_UN)


def all_events(state_dir: Path) -> list[dict[str, Any]]:
    log_path = state_dir / "review-events.jsonl"
    if not log_path.exists():
        return []
    events = []
    with log_path.open("r", encoding="utf-8") as file:
        # Shared lock pairs with append_event's exclusive lock: a reader can't
        # observe a partially flushed line from a concurrent agent's append.
        fcntl.flock(file, fcntl.LOCK_SH)
        try:
            for line in file:
                if not line.strip():
                    continue
                events.append(normalize_event(json.loads(line)))
        finally:
            fcntl.flock(file, fcntl.LOCK_UN)
    # Preserve the previous SELECT ... ORDER BY timestamp, event_id semantics that
    # downstream aggregation relies on.
    return sorted(events, key=event_sort_key)


def effective_signals_by_event(events: list[dict[str, Any]]) -> dict[str, list[str]]:
    """Return signals after append-only review-correction tombstones."""
    by_id = {
        str(event.get("event_id")): event
        for event in events
        if event.get("event_id")
    }
    retracted: dict[str, set[str]] = {}
    for correction in sorted(events, key=event_sort_key):
        if correction.get("event_type") != "review_correction":
            continue
        target_id = str(correction.get("corrects_event_id") or "")
        target = by_id.get(target_id)
        if target is None or target.get("event_type") == "review_correction":
            continue
        target_signals = {
            canonical
            for signal in target.get("signals") or []
            if (canonical := canonical_signal(str(signal))) is not None
        }
        corrected = {
            canonical
            for signal in correction.get("signals") or []
            if (canonical := canonical_signal(str(signal))) in target_signals
        }
        retracted.setdefault(target_id, set()).update(corrected)
    effective: dict[str, list[str]] = {}
    for event_id, event in by_id.items():
        if event.get("event_type") == "review_correction":
            effective[event_id] = []
            continue
        removed = retracted.get(event_id, set())
        effective[event_id] = [
            str(signal)
            for signal in event.get("signals") or []
            if (canonical := canonical_signal(str(signal))) is None
            or canonical not in removed
        ]
    return effective


def latest_events_by_pr_head(events: list[dict[str, Any]]) -> dict[tuple[int, str], dict[str, Any]]:
    latest: dict[tuple[int, str], dict[str, Any]] = {}
    for event in events:
        if event.get("event_type") == "review_correction":
            continue
        pr = event.get("pr")
        head_sha = event.get("head_sha")
        if pr is None or not head_sha:
            continue
        latest[(int(pr), str(head_sha))] = event
    return latest


def latest_events_by_pr(events: list[dict[str, Any]]) -> dict[int, dict[str, Any]]:
    """Return the newest recorded event for each PR, regardless of its head.

    The current-head index above decides whether a prior outcome still applies.
    This companion index makes a head transition visible to the recommendation
    ladder instead of silently treating an old hold as the PR's current state.
    """
    latest: dict[int, dict[str, Any]] = {}
    for event in events:
        if event.get("event_type") == "review_correction":
            continue
        pr = event.get("pr")
        if pr is None:
            continue
        latest[int(pr)] = event
    return latest


def is_block_event(event: dict[str, Any] | None) -> bool:
    """True when a local event means the current head is blocked."""
    event = event or {}
    outcome = event.get("outcome")
    if outcome == "ci_failed":
        return False
    return event.get("event_type") in LOCAL_BLOCK_EVENT_TYPES or outcome in LOCAL_BLOCK_OUTCOMES


def first_block_events_by_pr_head(
    events: list[dict[str, Any]],
) -> dict[tuple[int, str], dict[str, Any]]:
    """Earliest blocking event per (pr, head) — the requested-changes expiry anchor.

    Deliberately the earliest, never the latest: every sweep re-records a `blocked`
    event for a PR that is still blocked, and `event_id` hashes the timestamp, so
    each pass appends a distinct row. Anchoring the expiry clock on the newest event
    would let the loop reset its own timer, and `warning_due` could never fire.
    """
    first: dict[tuple[int, str], dict[str, Any]] = {}
    for event in events:  # append-only log; file order is chronological
        pr = event.get("pr")
        head_sha = event.get("head_sha")
        if pr is None or not head_sha or not is_block_event(event):
            continue
        first.setdefault((int(pr), str(head_sha)), event)
    return first


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


def timestamp_after(candidate: str | None, baseline: str | None) -> bool:
    candidate_dt = parse_event_datetime(candidate)
    baseline_dt = parse_event_datetime(baseline)
    return candidate_dt is not None and baseline_dt is not None and candidate_dt > baseline_dt


def age_in_days(value: str | None) -> float | None:
    timestamp = parse_event_datetime(value)
    if timestamp is None:
        return None
    return (datetime.now(UTC) - timestamp).total_seconds() / (24 * 60 * 60)


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


# ─── Canonical outcome mapping (read-side, legacy-tolerant) ──────────────────


def canonical_from_text(value: str | None) -> tuple[str, str] | None:
    # Read-side legacy mapper: collapses the free-form strings already present in
    # historical events (and in the `import` path) into canonical states. New
    # events are validated against ALLOWED_EVENT_TYPES/ALLOWED_OUTCOMES at write
    # time (see command_record), so this is not a write-side authority.
    if not value:
        return None
    text = value.lower().replace("_", "-")
    if "changes-requested" in text or "request-changes" in text or "reviewed-request-changes" in text:
        return ("changes_requested", "negative_review")
    if "still-blocked" in text or text == "blocked" or text.startswith("blocked-"):
        return ("blocked", "blocked")
    if "hard-stop" in text:
        return ("blocked", "hard_stop")
    if text == "decline" or text == "declined":
        return ("blocked", "declined")
    if "merged" in text or "pruned-as-merged" in text or text == "pruned-merged":
        return ("merged", "merged")
    if "defer-fe" in text or text == "defer" or text == "deferred":
        return ("deferred", "deferred")
    if text == "requested-changes-warning":
        return ("held", "requested_changes_warning")
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
    if (
        text == "closed"
        or text == "stale-changes-closed"
        or text.startswith("supersede")
        or text.startswith("superseded")
    ):
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


# ─── Analytics aggregation (events → PR rows → contributor rows) ─────────────


def head_analytics(pr: int, head_sha: str, events: list[dict[str, Any]]) -> dict[str, Any]:
    sorted_events = sorted(events, key=event_sort_key)
    ever_states: dict[str, int] = {}
    terminal = CanonicalOutcome("pending", "default", "low", "no_terminal_event")
    for event in sorted_events:
        outcome = canonical_outcome(event)
        ever_states[outcome.state] = ever_states.get(outcome.state, 0) + 1
        if outcome.state in TERMINAL_STATES:
            terminal = outcome
        elif terminal.state not in TERMINAL_STATES and outcome.state in PROGRESS_STATES:
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


def pr_analytics(accumulator: PrAccumulator) -> dict[str, Any]:
    head_rows = [
        head_analytics(accumulator.pr, head_sha, events)
        for head_sha, events in accumulator.head_events.items()
    ]
    no_head_events = [event for event in accumulator.events if not event.get("head_sha")]
    if not head_rows and no_head_events:
        head_rows.append(head_analytics(accumulator.pr, "", no_head_events))
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
        "first_seen": all_events_for_pr[0].get("timestamp") if all_events_for_pr else None,
        "last_seen": all_events_for_pr[-1].get("timestamp") if all_events_for_pr else None,
        "is_open_or_pending": latest["terminal_state"] not in TERMINAL_STATES,
        "event_count": len(all_events_for_pr),
    }


def rate(numerator: int, denominator: int) -> float | None:
    if denominator == 0:
        return None
    return numerator / denominator


def format_percent(value: float | None) -> str:
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
    if score >= SCORE_EXCELLENT:
        return "Excellent Signal"
    if score >= SCORE_STRONG:
        return "Strong Signal"
    if score >= SCORE_WATCH_FLOOR:
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
    # quality_signals arrives canonical (see canonical_signal), so direct
    # per-vocabulary lookups are safe; defects penalize, praise credits (capped).
    signal_penalty = sum(
        DEFECT_SIGNAL_WEIGHTS[signal] * count
        for signal, count in quality_signals.items()
        if signal in DEFECT_SIGNAL_WEIGHTS
    )
    praise_credit = min(
        PRAISE_CREDIT_CAP,
        sum(
            PRAISE_SIGNAL_WEIGHTS[signal] * count
            for signal, count in quality_signals.items()
            if signal in PRAISE_SIGNAL_WEIGHTS
        ),
    )
    clean_bonus = 5 if success_rate is not None and success_rate >= 0.85 and signal_penalty == 0 else 0
    score = (
        65
        + success_component
        - block_penalty
        - observed_head_penalty
        - signal_penalty
        + praise_credit
        + clean_bonus
    )
    score = min(100, max(0, score))
    return {
        "score": score,
        "components": {
            "baseline": 65,
            "success_component": success_component,
            "block_penalty": block_penalty,
            "observed_head_penalty": observed_head_penalty,
            "quality_signal_penalty": signal_penalty,
            "praise_credit": praise_credit,
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
    # top_signals is the "what to dig into" list for scrutiny consumers, so it
    # carries defects only; praise is reported separately.
    top_signals = sorted(
        (
            (signal, count)
            for signal, count in quality_signals.items()
            if signal in DEFECT_SIGNAL_WEIGHTS
        ),
        key=lambda item: (-item[1], item[0]),
    )[:5]
    praise_signals = {
        signal: count
        for signal, count in sorted(quality_signals.items())
        if signal in PRAISE_SIGNAL_WEIGHTS
    }
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
        "praise_signals": praise_signals,
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
        if author and fold_login(login) != fold_login(author):
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
        if event.get("event_type") == "review_correction":
            continue
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
    effective_signals = effective_signals_by_event(all_sorted_events)
    pr_contributors = build_pr_contributor_map(all_sorted_events)
    filtered_events = filtered_events_by_days(all_sorted_events, days)
    pr_accumulators: dict[int, PrAccumulator] = {}
    contributor_quality: dict[str, dict[str, int]] = {}
    unknown_signals: dict[str, int] = {}
    # GitHub logins are case-insensitive, so all grouping keys are folded; the
    # first-seen original casing is kept for display and restored on the final rows.
    display_names: dict[str, str] = {}

    def add_signals(folded_login: str, raw_signals: list[Any]) -> None:
        # Canonicalize every logged token; non-canonical strays are excluded from
        # derived metrics but counted in unknown_signals so they stay auditable.
        for raw in raw_signals:
            canonical = canonical_signal(str(raw))
            if canonical is None:
                add_counter(unknown_signals, str(raw))
            else:
                add_counter(contributor_quality.setdefault(folded_login, {}), canonical)

    for event in filtered_events:
        event_type = event.get("event_type")
        if event_type == "review_correction":
            continue
        login = contributor_login_for_event(event)
        if event_type == "quality_entry":
            if login:
                folded = fold_login(str(login))
                display_names.setdefault(folded, str(login))
                add_signals(folded, (event.get("quality") or {}).get("signals") or [])
            continue
        pr = event.get("pr")
        if pr is None:
            continue
        pr_number = int(pr)
        raw_contributor = login or pr_contributors.get(pr_number)
        if raw_contributor is None:
            continue
        contributor = fold_login(str(raw_contributor))
        display_names.setdefault(contributor, str(raw_contributor))
        if author and contributor != fold_login(author):
            continue
        # Signals recorded on PR-attributed outcome events join the same lifetime
        # aggregate the legacy quality_entry import feeds (per-occurrence recurrence
        # is collected separately by collect_signal_occurrences).
        add_signals(
            contributor,
            effective_signals.get(str(event.get("event_id") or ""), []),
        )
        accumulator = pr_accumulators.setdefault(
            pr_number,
            PrAccumulator(pr_number, contributor, [], {}),
        )
        accumulator.events.append(event)
        head_sha = event.get("head_sha")
        if head_sha:
            accumulator.head_events.setdefault(str(head_sha), []).append(event)
    prs = [pr_analytics(accumulator) for accumulator in pr_accumulators.values()]
    if not include_open:
        prs = [pr for pr in prs if not pr["is_open_or_pending"]]
    contributor_signals = {
        login: dict(signals) for login, signals in contributor_quality.items()
        if author is None or login == fold_login(author)
    }
    model = {
        "generated_at": now_iso(),
        "mode": "github_refreshed" if refreshed else "local_observed",
        "title": "Local Observed Review Analytics",
        "filters": {
            "author": author,
            "days": days,
            "min_prs": min_prs,
            "include_open": include_open,
        },
        "repo_medians": {"observed_heads": 0},
        "contributors": [],
        "display_names": display_names,
        "prs": prs,
        "quality_by_contributor": contributor_signals,
        "unknown_signals": unknown_signals,
        "unclassified_counts": unknown_event_values(filtered_events),
        "audit_counts": audit_event_values(filtered_events),
        "warnings": [],
    }
    finalize_contributor_model(model, min_prs=min_prs, author=author, refreshed=refreshed)
    return model


def finalize_contributor_model(
    model: dict[str, Any], *, min_prs: int, author: str | None, refreshed: bool
) -> None:
    """Recompute repo medians and contributor rows from the current PR rows.

    Called once after all PR-row mutations (github refresh, open-PR filter) so the
    expensive contributor aggregation happens a single time rather than per stage.
    """
    observed_head_values = [pr["observed_heads"] for pr in model["prs"] if pr["observed_heads"] > 0]
    repo_median_heads = median(observed_head_values) if observed_head_values else 0
    model["repo_medians"]["observed_heads"] = repo_median_heads
    model["contributors"] = contributor_rows_from_prs(
        model["prs"],
        model.get("quality_by_contributor", {}),
        float(repo_median_heads),
        refreshed,
        min_prs,
        author,
    )
    # Rows are grouped by folded login; restore first-seen casing for display.
    display_names = model.get("display_names", {})
    for row in model["contributors"]:
        row["login"] = display_names.get(row["login"], row["login"])


# ─── Contributor intelligence (recurrence, standing, scrutiny) ───────────────


def collect_signal_occurrences(
    events: list[dict[str, Any]],
) -> dict[str, list[dict[str, Any]]]:
    """Collect dated, PR-attributed DEFECT-signal occurrences per folded login.

    Only top-level `signals` on PR-attributed events qualify: legacy quality_entry
    imports carry neither a PR nor a real observation date (their timestamp is the
    import time), so they can never feed windowed recurrence. Praise signals are
    excluded by design — recurrence exists to elevate scrutiny and gate derived
    trust, and repeated praise must do neither.
    """
    occurrences: dict[str, list[dict[str, Any]]] = {}
    effective_signals = effective_signals_by_event(events)
    for event in events:
        if (
            event.get("event_type") not in PR_ATTRIBUTED_EVENTS
            or event.get("event_type") == "review_correction"
        ):
            continue
        signals = effective_signals.get(str(event.get("event_id") or ""), [])
        pr = event.get("pr")
        login = contributor_login_for_event(event)
        if not signals or pr is None or not login:
            continue
        entries = occurrences.setdefault(fold_login(str(login)), [])
        for signal in signals:
            canonical = canonical_signal(str(signal))
            if canonical not in DEFECT_SIGNAL_WEIGHTS:
                continue
            entries.append(
                {"signal": canonical, "pr": int(pr), "timestamp": event.get("timestamp")}
            )
    return occurrences


def windowed_recurrence(entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Reduce signal occurrences to per-signal distinct-PR counts within the window."""
    cutoff = datetime.now(UTC) - timedelta(days=RECURRENCE_WINDOW_DAYS)
    per_signal: dict[str, dict[str, Any]] = {}
    for entry in entries:
        timestamp = parse_event_datetime(entry.get("timestamp"))
        if timestamp is None or timestamp < cutoff:
            continue
        info = per_signal.setdefault(entry["signal"], {"prs": set(), "last_seen": ""})
        info["prs"].add(entry["pr"])
        info["last_seen"] = max(info["last_seen"], str(entry.get("timestamp") or ""))
    return [
        {
            "signal": signal,
            "distinct_prs_window": len(info["prs"]),
            "last_seen": info["last_seen"] or None,
        }
        for signal, info in sorted(per_signal.items())
    ]


def build_contributor_summary(
    author_login: str | None,
    current_pr: int | None,
    model: dict[str, Any],
    occurrences: dict[str, list[dict[str, Any]]],
    private_overrides: dict[str, Any],
) -> dict[str, Any] | None:
    """Build the packet's advisory `contributor` block from local observed history.

    Single authority for standing/scrutiny: make_packet and recommend_from_packet
    both read this block rather than re-deriving from overrides. Only an override
    standing of "skip" ever changes the advisory action; everything else informs
    review posture (scrutiny wins over light-touch when they disagree).
    """
    if not author_login:
        return None
    folded = fold_login(author_login)
    row = next(
        (r for r in model["contributors"] if fold_login(r["login"]) == folded), None
    )
    prior_prs = {
        pr["pr"]
        for pr in model["prs"]
        if pr["contributor_login"] == folded and pr["pr"] != current_pr
    }
    recurrence = windowed_recurrence(occurrences.get(folded, []))
    override = contributor_standing_override(author_login, private_overrides)
    derived_trusted = bool(
        row
        and row["terminal_prs"] >= TRUSTED_MIN_TERMINAL_PRS
        and (row["observed_success_rate"] or 0) >= TRUSTED_MIN_SUCCESS_RATE
        and not recurrence
    )
    if override:
        standing, standing_source = str(override["standing"]), "override"
    elif derived_trusted:
        standing, standing_source = "trusted", "derived"
    else:
        standing, standing_source = "unknown", "derived"
    score = row["local_signal_score"] if row else None
    confidence = row["confidence"] if row else None
    scrutiny_reasons = []
    if score is not None and score < SCORE_WATCH_FLOOR and confidence in {"medium", "high"}:
        scrutiny_reasons.append(f"low_score_{score}_{confidence}_confidence")
    for entry in recurrence:
        if entry["distinct_prs_window"] >= RECURRENCE_ELEVATED_PRS:
            scrutiny_reasons.append(
                f"recurrence_{entry['signal']}_{entry['distinct_prs_window']}_prs_in_window"
            )
    if standing in {"watch", "probation"}:
        scrutiny_reasons.append(f"standing_{standing}")
    if any(entry["distinct_prs_window"] >= RECURRENCE_ATTENTION_PRS for entry in recurrence):
        scrutiny = "maintainer_attention"
    elif scrutiny_reasons:
        scrutiny = "elevated"
    else:
        scrutiny = "normal"
    return {
        "login": author_login,
        "first_contribution": not prior_prs,
        "prior_prs": len(prior_prs),
        "score": score,
        "confidence": confidence,
        "top_signals": row["top_signals"] if row else [],
        "praise_signals": row["praise_signals"] if row else {},
        "recurrence": recurrence,
        "standing": standing,
        "standing_source": standing_source,
        "standing_note": override.get("note") if override else None,
        "scrutiny": scrutiny,
        "scrutiny_reasons": scrutiny_reasons,
        # Scrutiny wins: trusted standing never grants light touch while any
        # elevation reason is live.
        "light_touch_eligible": standing == "trusted" and scrutiny == "normal",
    }


# ─── ASCII rendering for analytics ───────────────────────────────────────────


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

    # Sort metric per --sort choice; every ordering tiebreaks on confidence first
    # and folded login last so rows are stable across runs.
    metrics = {
        "activity": lambda item: -item["prs"],
        "acceptance": lambda item: -(item["observed_success_rate"] or 0),
        "observed-heads": lambda item: -item["observed_heads_avg"],
        "score": lambda item: -item["local_signal_score"],
    }
    metric = metrics.get(sort_key, metrics["score"])
    rows = sorted(
        contributors,
        key=lambda item: (confidence_rank(item), metric(item), fold_login(item["login"])),
    )
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
            f"{format_percent(row['observed_success_rate']):>5} "
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
        if fold_login(contributor["login"]) == fold_login(login)
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
        f"PRs: {row['prs']}  Terminal: {row['terminal_prs']}  Observed success: {format_percent(row['observed_success_rate'])}",
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
    lines.append("Praise")
    if row["praise_signals"]:
        for signal, count in row["praise_signals"].items():
            lines.append(f"  {signal}: {count}")
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


# ─── GitHub live refresh (analytics --refresh-github) ────────────────────────


def gh_pr_refresh_chunk(repo: str, numbers: list[int]) -> dict[str, dict[str, Any] | None]:
    """Fetch live terminal state for up to 50 PRs in one aliased GraphQL query.

    Alias names (q0, q1, ...) are generated and the PR numbers are cast with int()
    before formatting, so they cannot carry injection; owner/name stay as variables.
    """
    owner, name = repo.split("/", 1)
    aliases = " ".join(
        f"q{index}: pullRequest(number: {int(number)}){{"
        "number state author{login} headRefOid reviewDecision mergedAt closedAt}"
        for index, number in enumerate(numbers)
    )
    query = f"query($owner:String!,$name:String!){{repository(owner:$owner,name:$name){{{aliases}}}}}"
    result = run_json(
        ["gh", "api", "graphql", "-f", f"owner={owner}", "-f", f"name={name}", "-f", f"query={query}"]
    )
    # GraphQL can answer HTTP 200 with `"data": null` plus an errors array, so
    # every level of the response is guarded with `or {}`, not a .get default.
    repository = (result.get("data") or {}).get("repository") or {}
    return {str(number): repository.get(f"q{index}") for index, number in enumerate(numbers)}


def apply_github_refresh(model: dict[str, Any], repo: str) -> None:
    warnings = model.setdefault("warnings", [])
    refreshed = 0
    prs_by_number = {str(pr["pr"]): pr for pr in model["prs"]}
    numbers = [int(pr["pr"]) for pr in model["prs"]]
    for start in range(0, len(numbers), 50):
        chunk = numbers[start : start + 50]
        try:
            live_by_number = gh_pr_refresh_chunk(repo, chunk)
        except subprocess.CalledProcessError as exc:
            warnings.append(f"failed to refresh PRs {chunk[0]}-{chunk[-1]}: {exc}")
            continue
        for number in chunk:
            pr = prs_by_number[str(number)]
            live = live_by_number.get(str(number))
            if not isinstance(live, dict):
                warnings.append(f"failed to refresh PR {number}: empty or invalid response")
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


# ─── Path classification, CI summary, and packet assembly ────────────────────


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
    elif set(classes) & {"engine", "server", "draft"}:
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


def status_summary(
    checks: list[dict[str, Any]], required_check_names: set[str] | None = None
) -> dict[str, Any]:
    """Summarize merge-gating checks, retaining non-required checks as advisory.

    GitHub's status rollup contains every integration that reports on a commit,
    including third-party trust and informational services. Only the checks
    configured on the PR base branch can block the merge queue. When that
    configuration is unavailable, retain the conservative legacy behavior rather
    than claiming a PR is ready without evidence.
    """
    advisory = []
    if required_check_names is None:
        required_checks = checks
        required_names = None
    else:
        required_names = set(required_check_names)
        required_checks = []
        seen_names = set()
        for check in checks:
            name = check.get("name", "<unknown>")
            if name in required_names:
                required_checks.append(check)
                seen_names.add(name)
            else:
                advisory.append(
                    {
                        "name": name,
                        "status": check.get("status"),
                        "conclusion": check.get("conclusion"),
                    }
                )

    pending = []
    failures = []
    successes = []
    for check in required_checks:
        name = check.get("name", "<unknown>")
        status = check.get("status")
        conclusion = (check.get("conclusion") or "").upper()
        if status != "COMPLETED":
            pending.append(name)
        elif conclusion not in {"SUCCESS", "SKIPPED", "NEUTRAL"}:
            failures.append(name)
        else:
            successes.append(name)
    if required_names is not None:
        pending.extend(sorted(required_names - seen_names, key=str.casefold))
    if failures:
        state = "failed"
    elif pending:
        state = "pending"
    elif successes or required_names == set():
        state = "green"
    else:
        state = "unknown"
    return {
        "state": state,
        "pending": pending,
        "failures": failures,
        "successes": successes,
        "required_checks": (
            sorted(required_names, key=str.casefold) if required_names is not None else None
        ),
        "advisory": advisory,
    }


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
        "createdAt": pr.get("createdAt"),
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
        "commit_author_logins": commit_author_logins(pr.get("commits") or []),
        "maintainer_merge_commit_count": maintainer_merge_commit_count(
            pr.get("commits") or [], acting_login
        ),
        "comments": [
            {
                "author": comment.get("author", {}).get("login"),
                "createdAt": comment.get("createdAt"),
                "body_hash": text_hash(comment.get("body")),
                "body_excerpt": excerpt(comment.get("body"), 300),
                "requested_changes_expiry_marker": REQUESTED_CHANGES_EXPIRY_MARKER
                in (comment.get("body") or ""),
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


def markdown_headings(body: str | None) -> set[str]:
    headings = set()
    for line in (body or "").splitlines():
        stripped = line.strip()
        if not stripped.startswith("#"):
            continue
        heading = stripped.lstrip("#").strip().lower()
        if heading:
            headings.add(heading)
    return headings


def markdown_sections(body: str | None) -> dict[str, list[str]]:
    """Split a PR body at exact H2 headings without interpreting other heading levels."""
    sections: dict[str, list[str]] = {}
    current: str | None = None
    for line in (body or "").splitlines():
        stripped = line.strip()
        match = re.match(r"^##\s+(.+?)\s*$", stripped)
        if match:
            current = match.group(1).casefold()
            sections.setdefault(current, [])
        elif current is not None:
            sections[current].append(line)
    return sections


def relevant_h2_heading_counts(body: str | None) -> dict[str, int]:
    counts = {heading: 0 for heading in REQUIRED_ARTIFACT_H2_HEADINGS}
    for line in (body or "").splitlines():
        match = re.match(r"^##\s+(.+?)\s*$", line.strip())
        if match:
            heading = match.group(1).casefold()
            if heading in counts:
                counts[heading] += 1
    return counts


def section_text(sections: dict[str, list[str]], heading: str) -> str:
    return "\n".join(sections.get(heading, [])).strip()


def admission_time_profile(pr: dict[str, Any], policy: Policy) -> dict[str, Any]:
    """Classify creation time for artifact admission enforcement."""
    cutoff = parse_event_datetime(policy.admission_enforced_after)
    created_raw = pr.get("createdAt")
    created = parse_event_datetime(created_raw)
    insufficient = created is None
    post_cutoff = cutoff is not None and created is not None and created >= cutoff
    if insufficient:
        status = "unknown"
    elif cutoff is None:
        status = "audit_unconfigured"
    elif post_cutoff:
        status = "post_cutoff"
    else:
        status = "proven_legacy"
    return {
        "created_at": created_raw,
        "created_at_status": status,
        "insufficient_admission_data": insufficient,
        "post_cutoff": post_cutoff,
        "enforced": policy.admission_mode == "enforce" and post_cutoff,
        "hold": policy.admission_mode == "enforce" and insufficient,
    }


def artifact_profile(
    pr: dict[str, Any],
    policy: Policy,
    files: list[str] | None = None,
    classification: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Evaluate immutable, current-head contributor artifacts separately from proof."""
    timing = admission_time_profile(pr, policy)
    body = str(pr.get("body") or "")
    head = str(pr.get("headRefOid") or "").lower()
    sections = markdown_sections(body)
    heading_counts = relevant_h2_heading_counts(body)
    missing_headings = sorted(
        heading for heading, count in heading_counts.items() if count == 0
    )
    duplicate_headings = sorted(
        heading for heading, count in heading_counts.items() if count > 1
    )
    gate_matches = re.findall(
        r"(?m)^Gate A PASS head=([0-9a-f]{40}) base=([0-9a-f]{40})$",
        section_text(sections, "gate a"),
    )
    review_matches = re.findall(
        r"(?m)^Final review-impl PASS head=([0-9a-f]{40})$",
        section_text(sections, "final review-impl"),
    )
    anchor_pattern = re.compile(r"^-\s+([^\s:][^:]*):([1-9][0-9]*)\s+(?:—|-)\s+.+$")
    anchors = []
    for line in sections.get("anchored on", []):
        match = anchor_pattern.match(line.strip())
        if match:
            anchors.append({"path": match.group(1), "line": int(match.group(2))})
    distinct_anchors = {
        (anchor["path"], anchor["line"]) for anchor in anchors
    }
    verification_lines = sections.get("verification", [])
    verification_text = "\n".join(verification_lines).strip()
    checked_required: dict[str, int] = {}
    unchecked_required: list[str] = []
    for label in REQUIRED_VERIFICATION_CHECKBOX_LABELS:
        checked_line = f"- [x] {label}"
        unchecked_line = f"- [ ] {label}"
        checked_required[label] = sum(
            line.strip() == checked_line for line in verification_lines
        )
        if any(line.strip() == unchecked_line for line in verification_lines):
            unchecked_required.append(unchecked_line)
    required_verification_valid = all(
        count == 1 for count in checked_required.values()
    ) and not unchecked_required
    method = section_text(sections, "implementation method (required)")
    method_records = re.findall(r"(?m)^Method:\s*.+$", method)
    backend_surface = bool(
        set((classification or {}).get("path_classes") or {})
        & {"engine", "server", "draft"}
    )
    method_valid = False
    if len(method_records) == 1:
        method_record = method_records[0]
        engine_method = method_record == "Method: /engine-implementer"
        not_applicable_method = bool(
            re.fullmatch(r"Method: not-applicable\s+[—-]\s+\S.+", method_record)
        )
        method_valid = engine_method or (not_applicable_method and not backend_surface)
    claimed_text = section_text(sections, "claimed parse impact")
    claimed_cards = []
    if claimed_text.casefold().rstrip(".") != "none":
        claimed_cards = sorted(
            {
                line.strip()[2:].strip()
                for line in sections.get("claimed parse impact", [])
                if line.strip().startswith("- ") and line.strip()[2:].strip()
            },
            key=str.casefold,
        )
    failures = []
    if missing_headings:
        failures.append("missing_required_h2_headings")
    if duplicate_headings:
        failures.append("duplicate_required_h2_headings")
    if not gate_matches:
        failures.append("missing_gate_a_pass")
    elif len(gate_matches) > 1:
        failures.append("duplicate_gate_a_pass")
    elif gate_matches[0][0] != head:
        failures.append("stale_gate_a_head")
    if not review_matches:
        failures.append("missing_final_review_impl_pass")
    elif len(review_matches) > 1:
        failures.append("duplicate_final_review_impl_pass")
    elif review_matches[0] != head:
        failures.append("stale_final_review_impl_head")
    if len(distinct_anchors) < 2:
        failures.append("fewer_than_two_anchors")
    if unchecked_required:
        failures.append("unchecked_required_verification")
    if not verification_text:
        failures.append("missing_or_empty_verification")
    elif not required_verification_valid:
        failures.append("invalid_required_verification_checkboxes")
    if not method_valid:
        failures.append("invalid_implementation_method")

    return {
        "mode": policy.admission_mode,
        "enforcement_cutoff": policy.admission_enforced_after,
        **timing,
        "passes": not failures,
        "would_decline": bool(failures),
        "decline": timing["enforced"] and bool(failures),
        "failures": failures,
        "gate_a_head": gate_matches[0][0] if len(gate_matches) == 1 else None,
        "final_review_head": review_matches[0] if len(review_matches) == 1 else None,
        "anchors": anchors,
        "unchecked_required": unchecked_required,
        "required_verification_counts": checked_required,
        "missing_relevant_h2_headings": missing_headings,
        "duplicate_relevant_h2_headings": duplicate_headings,
        "method_valid": method_valid,
        "method_records": method_records,
        "claimed_cards": claimed_cards,
    }


def accepted_closing_issues(pr: dict[str, Any], accepted_label: str | None) -> list[int]:
    if not accepted_label:
        return []
    accepted = accepted_label.casefold()
    return sorted(
        int(issue["number"])
        for issue in pr.get("closingIssuesReferences", [])
        if issue.get("number") is not None
        and accepted
        in {
            str(label.get("name") or "").casefold()
            for label in issue.get("labels", [])
        }
    )


def architecture_scope_profile(
    pr: dict[str, Any], files: list[str], policy: Policy, private_overrides: dict[str, Any]
) -> dict[str, Any]:
    matched_paths = sorted(
        path for path in files if matches_any(path, policy.architecture_scope_patterns)
    )
    span_names: set[str] = set()
    ambiguous_span_files: dict[str, list[str]] = {}
    for path in files:
        path_spans = sorted(
            name
            for name, patterns in policy.architecture_scope_spans.items()
            if matches_any(path, patterns)
        )
        if path_spans:
            # A file is one architectural unit. Overlapping policy patterns must
            # never manufacture three spans from a single changed path.
            span_names.add(path_spans[0])
        if len(path_spans) > 1:
            ambiguous_span_files[path] = path_spans
    spans = sorted(span_names)
    span_triggered = len(spans) >= policy.architecture_scope_span_threshold
    triggered = bool(matched_paths) or span_triggered
    author = (pr.get("author") or {}).get("login")
    private_authors = {
        fold_login(str(login))
        for login in private_overrides.get("architecture_scope_authors", [])
    }
    author_authorized = bool(author) and fold_login(str(author)) in private_authors
    issue_evidence_complete = bool(pr.get("closingIssuesReferencesComplete"))
    issues = (
        accepted_closing_issues(pr, policy.architecture_accepted_issue_label)
        if issue_evidence_complete
        else []
    )
    issue_authorized = bool(issues)
    authorized = author_authorized or issue_authorized
    mode = policy.architecture_scope_mode
    incomplete_issue_evidence = (
        triggered and not author_authorized and not issue_evidence_complete
    )
    evidence = {
        "matched_paths": matched_paths,
        "spans": spans,
        "span_threshold": policy.architecture_scope_span_threshold,
        "ambiguous_span_files": ambiguous_span_files,
        "accepted_closing_issues": issues,
        "closing_issue_records": pr.get("closingIssuesReferences", []),
        "closing_issue_count": pr.get("closingIssuesReferencesCount"),
        "author_private_override": author_authorized,
        "closing_issue_evidence_complete": issue_evidence_complete,
        "files_complete": bool(pr.get("filesComplete")),
    }
    path_text = ", ".join(matched_paths) if matched_paths else "none"
    span_text = ", ".join(spans) if spans else "none"
    decline_comment = (
        "**Closed without implementation-diff review.** This PR enters protected "
        "architecture scope because it touches "
        f"`{path_text}` and/or spans `{span_text}`. AI-contributor PRs may enter "
        "this scope only after an explicit prior maintainer appointment or when "
        "the PR closes an issue labeled `accepted`. Tier, contributor standing, "
        "the `quality` label, prior praise, and frontend permission do not waive "
        "this gate. Open a fresh PR from current `main` only after one of those "
        "authorizations exists, and rerun `/engine-implementer`, the final "
        "`review-impl`, and Gate A against its committed head."
    )
    return {
        "mode": mode,
        "enforcement_cutoff": None,
        "post_cutoff": None,
        "enforced": mode == "enforce",
        "hold": mode == "enforce" and incomplete_issue_evidence,
        "triggered": triggered,
        "authorized": authorized,
        "would_decline": triggered and not authorized,
        "decline": (
            mode == "enforce"
            and triggered
            and not authorized
            and not incomplete_issue_evidence
        ),
        "evidence": evidence,
        "decline_comment": decline_comment,
    }


def unchecked_markdown_items(body: str | None) -> list[str]:
    return [
        line.strip()
        for line in (body or "").splitlines()
        if line.lstrip().startswith("- [ ]")
    ]


def checked_markdown_items(body: str | None) -> list[str]:
    return [
        line.strip()
        for line in (body or "").splitlines()
        if line.lstrip().lower().startswith("- [x]")
    ]


def checked_test_evidence(body: str | None) -> list[str]:
    evidence_terms = ("test", "cargo ", "pnpm ", "./scripts/", "tilt", "ci ")
    return [
        item
        for item in checked_markdown_items(body)
        if item.startswith("- [x] `") or any(term in item.lower() for term in evidence_terms)
    ]


def commit_author_logins(commits: list[dict[str, Any]]) -> list[str]:
    logins = {
        str(author.get("login"))
        for commit in commits
        for author in commit.get("authors", [])
        if author.get("login")
    }
    return sorted(logins, key=str.casefold)


def maintainer_merge_commit_count(commits: list[dict[str, Any]], acting_login: str) -> int:
    acting = fold_login(acting_login)
    count = 0
    for commit in commits:
        headline = str(commit.get("messageHeadline") or "")
        if not headline.startswith("Merge "):
            continue
        author_logins = {
            fold_login(str(author.get("login")))
            for author in commit.get("authors", [])
            if author.get("login")
        }
        if acting in author_logins:
            count += 1
    return count


def every_commit_has_agent_coauthor(commits: list[dict[str, Any]]) -> bool:
    if not commits:
        return False
    for commit in commits:
        logins = {
            fold_login(str(author.get("login")))
            for author in commit.get("authors", [])
            if author.get("login")
        }
        if not logins & AI_AGENT_COAUTHOR_LOGINS:
            return False
    return True


def proof_profile(
    pr: dict[str, Any],
    contributor: dict[str, Any] | None,
    gittensor: dict[str, Any] | None = None,
) -> dict[str, Any]:
    body = pr.get("body") or ""
    headings = markdown_headings(body)
    missing_template_sections = [
        heading for heading in AI_CONTRIBUTOR_TEMPLATE_HEADINGS if heading not in headings
    ]
    unchecked_items = unchecked_markdown_items(body)
    checked_evidence = checked_test_evidence(body)
    lower_body = body.lower()
    skipped_phrases = [phrase for phrase in PROOF_SKIP_PHRASES if phrase in lower_body]
    agent_coauthored = every_commit_has_agent_coauthor(pr.get("commits") or [])
    scrutiny = (contributor or {}).get("scrutiny")

    risk_flags = []
    tracking_signals = []
    if missing_template_sections:
        risk_flags.append("missing-ai-contributor-template")
        tracking_signals.append("ai-template-gap")
    if unchecked_items:
        risk_flags.append("unchecked-verification-items")
        if any("engine-implementer" in item for item in unchecked_items):
            tracking_signals.append("unchecked-engine-implementer")
    if skipped_phrases:
        risk_flags.append("verification-skipped-or-delegated")
    if agent_coauthored:
        risk_flags.append("agent-coauthored-all-commits")
    if scrutiny in {"elevated", "maintainer_attention"}:
        risk_flags.append(f"contributor-scrutiny-{scrutiny}")
    if (gittensor or {}).get("risk_flag"):
        risk_flags.append(str((gittensor or {})["risk_flag"]))

    # Missing template sections, unchecked checklist items, and elevated
    # contributor scrutiny are tracking signals for repeat patterns. They should
    # not by themselves block an otherwise passing review; only hard proof risks
    # require concrete verification before queue handoff.
    proof_required = any(flag in PROOF_REQUIRED_RISK_FLAGS for flag in risk_flags)
    template_verification_complete = bool(body.strip()) and not (
        missing_template_sections or skipped_phrases
    )
    proof_satisfied = template_verification_complete or bool(checked_evidence)
    return {
        "proof_required": proof_required,
        "proof_satisfied": proof_satisfied,
        "proof_gap": proof_required and not proof_satisfied,
        "risk_flags": risk_flags,
        "missing_template_sections": missing_template_sections,
        "unchecked_items": unchecked_items[:5],
        "checked_test_evidence": checked_evidence[:5],
        "skipped_phrases": skipped_phrases,
        "agent_coauthored_all_commits": agent_coauthored,
        "tracking_signals": tracking_signals,
    }


def proof_tracking_signals(packet: dict[str, Any]) -> list[str]:
    proof = packet.get("proof") or {}
    return list(proof.get("tracking_signals") or [])


def is_parser_refactor_file(path: str) -> bool:
    return path.startswith("crates/engine/src/parser/")


def is_declared_refactor(pr: dict[str, Any]) -> bool:
    labels = {str(label).casefold() for label in pr.get("labels", []) if label}
    title = str(pr.get("title") or "").casefold()
    return "refactor" in labels or title.startswith("refactor")


def low_risk_refactor_proof_override(
    *,
    pr: dict[str, Any],
    files: list[str],
    classification: dict[str, Any],
    checks: dict[str, Any],
    parse_diff: dict[str, Any],
    proof: dict[str, Any],
) -> bool:
    """Allow green parser-only refactors to clear body-proof-only gaps.

    Agent coauthorship still raises scrutiny, but for a refactor with no card parse
    changes the concrete evidence is the diff shape, parse-diff, and green checks.
    Hard proof risks like skipped verification or Gittensor closed-heavy still block.
    """
    if not proof.get("proof_gap"):
        return False
    risk_flags = set(proof.get("risk_flags") or [])
    if risk_flags & {"verification-skipped-or-delegated", "gittensor-closed-heavy"}:
        return False
    return (
        bool(files)
        and classification.get("surface") == "backend"
        and is_declared_refactor(pr)
        and all(is_parser_refactor_file(path) for path in files)
        and checks.get("state") == "green"
        and parse_diff.get("present")
        and parse_diff.get("state") == "no_changes"
    )


def fetch_gittensor_records(api_url: str | None) -> tuple[list[dict[str, Any]], str | None]:
    if not api_url:
        return [], None
    try:
        request = urllib.request.Request(
            api_url,
            headers={"User-Agent": "phase-rs-pr-review/1.0"},
        )
        with urllib.request.urlopen(request, timeout=20) as response:
            records = json.load(response)
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as exc:
        return [], f"Gittensor API unavailable ({exc})"
    if not isinstance(records, list):
        return [], "Gittensor API returned a non-list payload"
    return [record for record in records if isinstance(record, dict)], None


def build_gittensor_index(records: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    index: dict[str, dict[str, Any]] = {}
    for record in records:
        author = record.get("author")
        if not author:
            continue
        login = fold_login(str(author))
        row = index.setdefault(
            login,
            {
                "login": str(author),
                "total_prs": 0,
                "states": {},
                "repositories": {},
                "hotkeys": set(),
            },
        )
        state = str(record.get("prState") or "").upper()
        if not state:
            state = "MERGED" if record.get("mergedAt") else "UNKNOWN"
        repo = str(record.get("repository") or "unknown").lower()
        row["total_prs"] += 1
        row["states"][state] = row["states"].get(state, 0) + 1
        row["repositories"][repo] = row["repositories"].get(repo, 0) + 1
        if record.get("hotkey"):
            row["hotkeys"].add(str(record["hotkey"]))

    for row in index.values():
        total = row["total_prs"]
        closed = row["states"].get("CLOSED", 0)
        row["closed_ratio"] = closed / total if total else 0.0
        row["repository_count"] = len(row["repositories"])
        row["hotkey_count"] = len(row["hotkeys"])
        row["hotkeys"] = sorted(row["hotkeys"])
        row["top_repositories"] = [
            {"repository": repo, "prs": count}
            for repo, count in sorted(
                row["repositories"].items(), key=lambda item: (-item[1], item[0])
            )[:5]
        ]
        row.pop("repositories", None)
        if (
            closed >= GITTENSOR_CLOSED_ATTENTION_MIN
            and row["closed_ratio"] >= GITTENSOR_CLOSED_ATTENTION_RATIO
        ):
            row["risk_flag"] = "gittensor-closed-heavy"
        else:
            row["risk_flag"] = None
    return index


def gittensor_summary(
    author_login: str | None, index: dict[str, dict[str, Any]], warning: str | None
) -> dict[str, Any] | None:
    if not author_login:
        return None
    row = index.get(fold_login(author_login))
    if row is None:
        if warning:
            return {"present": False, "warning": warning}
        return {"present": False}
    summary = dict(row)
    summary["present"] = True
    if warning:
        summary["warning"] = warning
    return summary


# ─── Advisory recommendation (ordered precedence ladder) ─────────────────────


def requested_changes_policy(packet: dict[str, Any]) -> dict[str, Any]:
    configured = (packet.get("policy") or {}).get("requested_changes") or {}
    return {
        "warning_after_days": configured.get(
            "warning_after_days", DEFAULT_REQUESTED_CHANGES_WARNING_AFTER_DAYS
        ),
        "close_after_warning_days": configured.get(
            "close_after_warning_days", DEFAULT_REQUESTED_CHANGES_CLOSE_AFTER_WARNING_DAYS
        ),
        "warning_marker": configured.get(
            "warning_marker", REQUESTED_CHANGES_EXPIRY_MARKER
        ),
    }


def comment_login(comment: dict[str, Any]) -> str | None:
    author = comment.get("author")
    if isinstance(author, dict):
        return author.get("login")
    return author


def comment_text(comment: dict[str, Any]) -> str:
    return str(comment.get("body") or comment.get("body_excerpt") or "")


def activity_timestamp(activity: dict[str, Any], *fields: str) -> str | None:
    """Return the latest parseable timestamp from one comment or review."""
    timestamps = [
        value
        for field in fields
        if (value := activity.get(field)) and parse_event_datetime(value) is not None
    ]
    return max(timestamps, key=lambda value: parse_event_datetime(value) or datetime.min.replace(tzinfo=UTC), default=None)


def author_activity_after(pr: dict[str, Any], timestamp: str | None) -> bool:
    author_login = pr.get("author_login")
    return any(
        comment_login(comment) == author_login
        and timestamp_after(activity_timestamp(comment, "createdAt", "updatedAt"), timestamp)
        for comment in pr.get("comments", [])
    ) or any(
        comment_login(review) == author_login
        and timestamp_after(activity_timestamp(review, "submittedAt"), timestamp)
        for review in pr.get("reviews", [])
    )


def has_author_activity(pr: dict[str, Any]) -> bool:
    """Whether the contributor has left a comment or review on this PR."""
    author_login = pr.get("author_login")
    return any(
        comment_login(comment) == author_login for comment in pr.get("comments", [])
    ) or any(
        comment_login(review) == author_login for review in pr.get("reviews", [])
    )


def latest_maintainer_activity_timestamp(pr: dict[str, Any], acting_login: str | None) -> str | None:
    """Find the latest GitHub-visible response from the acting maintainer.

    Local event-log rows record what the loop observed or decided; they are not a
    response to a contributor. Follow-up freshness must therefore anchor only on
    a real GitHub review/comment from the maintainer.
    """
    if not acting_login:
        return None
    timestamps = [
        activity_timestamp(comment, "createdAt", "updatedAt")
        for comment in pr.get("comments", [])
        if comment_login(comment) == acting_login
    ] + [
        activity_timestamp(review, "submittedAt")
        for review in pr.get("reviews", [])
        if comment_login(review) == acting_login
    ]
    timestamps = [timestamp for timestamp in timestamps if timestamp]
    return max(
        timestamps,
        key=lambda timestamp: parse_event_datetime(timestamp) or datetime.min.replace(tzinfo=UTC),
        default=None,
    )


def review_freshness(
    pr: dict[str, Any],
    acting_login: str | None,
    local_current_event: dict[str, Any] | None,
    local_latest_event: dict[str, Any] | None,
) -> dict[str, Any]:
    """Describe GitHub activity that invalidates a cached local posture."""
    current_head = pr.get("headRefOid")
    previous_head = (local_latest_event or {}).get("head_sha")
    maintainer_activity = latest_maintainer_activity_timestamp(pr, acting_login)
    local_timestamp = (local_current_event or {}).get("timestamp")
    author_followup_after_maintainer_activity = (
        author_activity_after(pr, maintainer_activity)
        if maintainer_activity
        else has_author_activity(pr)
    )
    return {
        "head_changed_since_local_event": bool(
            current_head and previous_head and previous_head != current_head
        ),
        "previous_head_sha": previous_head,
        "latest_maintainer_activity_at": maintainer_activity,
        "author_followup_after_maintainer_activity": author_followup_after_maintainer_activity,
        "author_followup_after_local_event": author_activity_after(pr, local_timestamp),
        "comment_history_incomplete": pr.get("commentsComplete") is False,
    }


def latest_requested_changes_review_timestamp(packet: dict[str, Any]) -> str | None:
    head = (packet.get("pr") or {}).get("headRefOid")
    reviews = [
        review
        for review in (packet.get("pr") or {}).get("reviews", [])
        if review.get("state") == "CHANGES_REQUESTED"
        and (not review.get("commit") or not head or review.get("commit") == head)
        and review.get("submittedAt")
    ]
    if not reviews:
        return None
    reviews.sort(key=lambda review: review.get("submittedAt") or "")
    return reviews[-1].get("submittedAt")


def latest_requested_changes_warning(packet: dict[str, Any]) -> dict[str, Any] | None:
    local_event = packet.get("local_current_event") or {}
    candidates = []
    if local_event.get("event_type") == "requested_changes_warning" or local_event.get(
        "outcome"
    ) == "requested_changes_warning":
        candidates.append(
            {
                "source": "event_log",
                "timestamp": local_event.get("timestamp"),
            }
        )
    marker = requested_changes_policy(packet)["warning_marker"]
    acting_login = packet.get("acting_login")
    for comment in (packet.get("pr") or {}).get("comments", []):
        marker_present = comment.get("requested_changes_expiry_marker") or (
            marker in comment_text(comment)
        )
        if not marker_present:
            continue
        author_login = comment_login(comment)
        if acting_login and author_login and author_login != acting_login:
            continue
        candidates.append(
            {
                "source": "github_comment",
                "timestamp": comment.get("createdAt"),
            }
        )
    candidates = [candidate for candidate in candidates if candidate.get("timestamp")]
    if not candidates:
        return None
    candidates.sort(key=lambda candidate: candidate["timestamp"])
    return candidates[-1]


def requested_changes_expiry_state(
    packet: dict[str, Any], local_block: bool, author_followup_after_local_event: bool
) -> dict[str, Any]:
    pr = packet.get("pr") or {}
    policy = requested_changes_policy(packet)
    head = pr.get("headRefOid")
    review_decision = pr.get("reviewDecision")
    latest_commit = packet.get("latest_maintainer_review_commit")
    current_head_changes_requested = review_decision == "CHANGES_REQUESTED" and (
        latest_commit is None or latest_commit == head
    )
    # "Has this head ever been blocked?" is a property of the log's history, not of its
    # last row. `local_block` only inspects the newest event, so once we post the stale
    # warning that warning becomes the newest event and erases the block — `active` would
    # drop to False and the warning could never mature into a close. Consult the first
    # block recorded for this head instead.
    first_block_event = packet.get("local_first_block_event")
    head_ever_blocked = local_block or bool(first_block_event)
    active = head_ever_blocked or current_head_changes_requested
    warning = latest_requested_changes_warning(packet)
    warning_timestamp = (warning or {}).get("timestamp")
    author_followup_after_warning = author_activity_after(pr, warning_timestamp)
    # Anchor the expiry clock on the blocker itself, never on our observation of it.
    # GitHub is authoritative, so a formal CHANGES_REQUESTED on the current head wins
    # (a newer one correctly restarts the window). Only when no formal review exists
    # do we fall back to the local log — and then to the FIRST block recorded for this
    # head, because the sweep re-records `blocked` every pass and the newest event
    # would pin blocker_age at ~0 forever, silently disabling warn/close entirely.
    blocker_timestamp = None
    if current_head_changes_requested:
        blocker_timestamp = latest_requested_changes_review_timestamp(packet)
    if blocker_timestamp is None and head_ever_blocked:
        anchor = first_block_event or (packet.get("local_current_event") if local_block else None)
        blocker_timestamp = (anchor or {}).get("timestamp")
    blocker_age = age_in_days(blocker_timestamp)
    warning_age = age_in_days(warning_timestamp)
    warning_due = (
        active
        and warning is None
        and blocker_age is not None
        and blocker_age >= policy["warning_after_days"]
    )
    close_due = (
        active
        and warning is not None
        and not author_followup_after_warning
        and not author_followup_after_local_event
        and warning_age is not None
        and warning_age >= policy["close_after_warning_days"]
    )
    return {
        "active": active,
        "blocker_timestamp": blocker_timestamp,
        "warning": warning,
        "warning_due": warning_due,
        "close_due": close_due,
        "author_followup_after_warning": author_followup_after_warning,
        "warning_after_days": policy["warning_after_days"],
        "close_after_warning_days": policy["close_after_warning_days"],
        "warning_marker": policy["warning_marker"],
    }


def recommend_from_packet(packet: dict[str, Any]) -> dict[str, Any]:
    pr = packet["pr"]
    head = pr.get("headRefOid")
    classification = packet.get("classification", {})
    artifacts = packet.get("artifacts") or {}
    architecture_scope = packet.get("architecture_scope") or {}
    latest_commit = packet.get("latest_maintainer_review_commit")
    review_decision = pr.get("reviewDecision")
    queue = bool(
        pr.get("isInMergeQueue") or pr.get("mergeQueueEntry") or pr.get("autoMergeRequest")
    )
    local_event = packet.get("local_current_event") or {}
    local_event_type = local_event.get("event_type")
    local_outcome = local_event.get("outcome")
    local_event_timestamp = local_event.get("timestamp")
    freshness = packet.get("freshness") or {}
    author_followup_after_local_event = freshness.get(
        "author_followup_after_local_event",
        author_activity_after(pr, local_event_timestamp),
    )
    author_followup_after_maintainer_activity = freshness.get(
        "author_followup_after_maintainer_activity",
        author_followup_after_local_event,
    )
    head_changed_since_local_event = freshness.get("head_changed_since_local_event", False)
    comment_history_incomplete = freshness.get("comment_history_incomplete", False)
    parse_diff = packet.get("parse_diff") or {}
    parse_diff_after_local_event = timestamp_after(
        parse_diff.get("updated_at"), local_event_timestamp
    )
    author_policy = packet.get("author_policy", {})
    local_hold = local_event_type == "held" or local_outcome in HOLD_STATES
    local_block = is_block_event(local_event)
    conflicts_with_base = (
        pr.get("mergeStateStatus") == "DIRTY" or pr.get("mergeable") == "CONFLICTING"
    )
    requested_changes_expiry = requested_changes_expiry_state(
        packet, local_block, author_followup_after_local_event
    )

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
    elif artifacts.get("hold") or architecture_scope.get("hold"):
        action = "hold"
        reason = "insufficient_admission_data"
    elif artifacts.get("decline"):
        action = "decline"
        reason = "required_artifacts_current_head"
    elif architecture_scope.get("decline"):
        action = "decline"
        reason = "architecture_scope_not_authorized"
    elif (packet.get("contributor") or {}).get("standing") == "skip":
        # Explicit maintainer standing override (private-overrides.json). Ordered
        # after hard_stop deliberately: a skip-listed contributor touching guarded
        # paths still surfaces as request_changes — safety wins over the skip.
        action = "skip"
        reason = "contributor_standing_skip"
    elif (local_outcome or "").lower() == "defer-fe":
        action = "defer"
        reason = "local_defer_fe_current_head"
    elif local_hold and author_followup_after_maintainer_activity:
        action = "review"
        reason = "author_followup_after_maintainer_activity"
    elif local_hold and comment_history_incomplete:
        action = "review"
        reason = "author_activity_history_incomplete"
    elif local_hold and (packet.get("ci") or {}).get("state") != "green":
        action = "hold_ci"
        reason = "local_hold_current_head"
    elif local_hold and conflicts_with_base:
        action = "blocked"
        reason = "local_hold_current_head"
    elif local_block and author_followup_after_maintainer_activity and conflicts_with_base:
        action = "update_branch_for_handler"
        reason = "conflicting_after_author_followup"
    elif local_block and author_followup_after_maintainer_activity:
        action = "review"
        reason = "author_followup_after_maintainer_activity"
    elif local_block and comment_history_incomplete:
        action = "review"
        reason = "author_activity_history_incomplete"
    elif requested_changes_expiry["author_followup_after_warning"] and conflicts_with_base:
        action = "update_branch_for_handler"
        reason = "conflicting_after_author_followup"
    elif requested_changes_expiry["author_followup_after_warning"]:
        action = "review"
        reason = "author_followup_after_requested_changes_warning"
    elif requested_changes_expiry["close_due"]:
        action = "close_stale_changes_for_handler"
        reason = "requested_changes_expired"
    elif requested_changes_expiry["warning_due"]:
        action = "warn_stale_changes_for_handler"
        reason = "requested_changes_warning_due"
    elif head_changed_since_local_event and conflicts_with_base:
        action = "update_branch_for_handler"
        reason = "conflicting_after_head_change"
    elif head_changed_since_local_event:
        action = "review"
        reason = "head_changed_since_local_event"
    elif local_block and parse_diff_after_local_event:
        if conflicts_with_base:
            action = "update_branch_for_handler"
            reason = "conflicting_after_parse_diff_followup"
        else:
            action = "review"
            reason = "parse_diff_after_local_block"
    elif local_block:
        action = "blocked"
        reason = "local_block_current_head"
    elif classification.get("files_truncated"):
        # A truncated file list may hide a hard-stop path, so it must never silently
        # defer or pass to a handler. Current-head local terminal events are honored
        # above; otherwise force a manual review before any softer branch.
        action = "review"
        reason = "files_truncated_needs_manual_classification"
    elif review_decision == "APPROVED" and conflicts_with_base:
        action = "update_branch_for_handler"
        reason = "approved_conflicting"
    elif requested_changes_expiry["active"]:
        action = "blocked"
        reason = "changes_requested_current_head"
    elif conflicts_with_base:
        action = "update_branch_for_handler"
        reason = "conflicting"
    elif latest_commit and latest_commit != head and review_decision == "APPROVED":
        action = "dequeue_stale_for_handler" if queue else "review"
        reason = "stale_approval"
    elif queue and author_followup_after_maintainer_activity:
        action = "review"
        reason = "author_followup_after_maintainer_activity"
    elif queue and comment_history_incomplete:
        action = "review"
        reason = "author_activity_history_incomplete"
    elif queue and review_decision == "APPROVED":
        action = "queued"
        reason = (
            "already_in_merge_queue"
            if (pr.get("isInMergeQueue") or pr.get("mergeQueueEntry"))
            else "auto_merge_enabled"
        )
    elif (packet.get("proof") or {}).get("proof_gap"):
        action = "request_changes" if review_decision == "APPROVED" or queue else "review"
        reason = "proof_required_missing"
    elif classification.get("surface") == "frontend" and not author_policy.get(
        "frontend_review_allowed"
    ):
        action = "defer"
        reason = "frontend_policy"
    elif local_event_type == "approved_enqueued":
        action = "approve_ready_for_handler"
        reason = "local_approved_enqueued_live_check"
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

    # Advisory-only parse-diff hint (the comment job is continue-on-error/non-blocking):
    # a stale merge-base whose R2 baseline aged out shows "Baseline pending" forever, so
    # flag engine-surface review candidates to consider update-branch first. The
    # files_truncated safety reason from make_packet is preserved — it must not be masked.
    baseline_pending_after_maintainer_merge = (
        "engine" in (classification.get("path_classes") or {})
        and parse_diff.get("state") == "baseline_pending"
        and int(pr.get("maintainer_merge_commit_count") or 0) > 0
    )
    stale_changes_requested = (
        review_decision == "CHANGES_REQUESTED"
        and latest_commit is not None
        and latest_commit != head
    )
    if action not in {"decline", "hold"} and baseline_pending_after_maintainer_merge and (
        review_decision == "APPROVED" or stale_changes_requested
    ) and not (packet.get("proof") or {}).get("proof_gap"):
        action = "approve_ready_for_handler"
        reason = "baseline_pending_after_maintainer_merge_ready"
    elif (
        action == "review"
        and reason != "files_truncated_needs_manual_classification"
        and "engine" in (classification.get("path_classes") or {})
        and parse_diff.get("state") == "baseline_pending"
    ):
        reason = (
            "review_parse_baseline_pending_after_maintainer_merge"
            if baseline_pending_after_maintainer_merge
            else "review_parse_baseline_pending"
        )

    recommendation = {
        "pr": pr.get("number"),
        "head_sha": head,
        "advisory_action": action,
        "reason": reason,
        "requires_live_verification": action.endswith("_for_handler"),
        "policy_trace": packet.get("policy_trace", []),
        # The `recommend` command prints only this dict, so the advisory contributor
        # block (standing/scrutiny/recurrence) must ride along for skill consumers.
        "contributor": packet.get("contributor"),
        "gittensor": packet.get("gittensor"),
        "proof": packet.get("proof"),
        "artifacts": artifacts,
        "architecture_scope": architecture_scope,
    }
    audit_would_decline = []
    if artifacts.get("would_decline") and not artifacts.get("decline"):
        audit_would_decline.append(
            {"gate": "artifacts", "evidence": artifacts.get("failures", [])}
        )
    if architecture_scope.get("would_decline") and not architecture_scope.get("decline"):
        audit_would_decline.append(
            {
                "gate": "architecture_scope",
                "evidence": architecture_scope.get("evidence", {}),
            }
        )
    if audit_would_decline:
        recommendation["audit_would_decline"] = audit_would_decline
    if artifacts.get("mode") == "audit" and artifacts.get(
        "insufficient_admission_data"
    ):
        recommendation["audit_insufficient_admission_data"] = {
            "created_at": artifacts.get("created_at"),
            "created_at_status": artifacts.get("created_at_status"),
        }
    if action == "hold" and reason == "insufficient_admission_data":
        recommendation["hold_evidence"] = {
            "maintainer_blocker": True,
            "non_mutating": True,
            "implementation_diff_review_allowed": False,
            "created_at": artifacts.get("created_at"),
            "created_at_status": artifacts.get("created_at_status"),
        }
    if action == "decline":
        if reason == "architecture_scope_not_authorized":
            recommendation["decline_comment"] = architecture_scope.get("decline_comment")
            recommendation["decline_evidence"] = architecture_scope.get("evidence")
        else:
            failures = ", ".join(artifacts.get("failures") or [])
            recommendation["decline_comment"] = (
                "**Closed without implementation-diff review.** Required current-head "
                f"admission artifacts failed for `{head}`: {failures}. Open a fresh "
                "PR from current `main`, rerun `/engine-implementer`, complete and "
                "address a final `review-impl`, then rerun Gate A against that exact "
                "committed head. Merely including the required headings or PASS text "
                "is not validation; their content and SHA must match the current head."
            )
            recommendation["decline_evidence"] = artifacts
    if action in {"warn_stale_changes_for_handler", "close_stale_changes_for_handler"}:
        recommendation["requested_changes_expiry"] = requested_changes_expiry
    if action == "defer" and reason == "frontend_policy":
        label = packet.get("policy", {}).get("labels", {}).get("frontend_deferred")
        if label:
            recommendation["label_to_apply"] = label
    return recommendation


def parse_diff_comment_state(
    comments: list[dict[str, Any]], trusted_authors: set[str] | None = None
) -> dict[str, Any]:
    """Classify the parse-diff sticky comment from FULL comment bodies.

    Must run on raw (un-excerpted) comments — compact_pr_view truncates bodies to
    300 chars, which can drop the "signature(s)"/"Baseline pending" markers. The
    comment is edited in place on re-push, so updatedAt (not createdAt) is freshness.
    """
    trusted_authors = trusted_authors or {"github-actions"}
    for comment in comments:
        author_login = (comment.get("author") or {}).get("login")
        if author_login not in trusted_authors:
            continue
        body = comment.get("body") or ""
        if not body.lstrip().startswith(PARSE_DIFF_MARKER):
            continue
        if "Baseline pending" in body:
            state = "baseline_pending"
        elif "signature(s)" in body:
            state = "real_changes"
        else:
            state = "no_changes"
        return {"present": True, "state": state, "updated_at": comment.get("updatedAt")}
    return {"present": False, "state": "absent", "updated_at": None}


def make_packet(
    pr: dict[str, Any],
    policy: Policy,
    acting_login: str,
    mode: str,
    private_overrides: dict[str, Any],
    local_event: dict[str, Any] | None = None,
    contributor_summary: dict[str, Any] | None = None,
    gittensor: dict[str, Any] | None = None,
    first_block_event: dict[str, Any] | None = None,
    required_check_names: set[str] | None = None,
) -> dict[str, Any]:
    files = pr_files_from_view(pr)
    classification = classify_files(files, policy)
    changed_files = pr.get("changedFiles")
    if isinstance(changed_files, int) and changed_files > len(files):
        # GitHub truncated the file list (files(first:100) caps at 100), so the
        # classification is untrusted — a hard-stop path may be hidden past the cap.
        classification["surface"] = "files_truncated"
        classification["gate"] = "review"
        classification["files_truncated"] = True
    checks = status_summary(pr.get("statusCheckRollup", []), required_check_names)
    # Classify the parse-diff sticky comment from raw bodies, before compact_pr_view
    # excerpts them to 300 chars and would drop the marker substrings.
    parse_diff = parse_diff_comment_state(
        pr.get("comments", []), {"github-actions", acting_login}
    )
    artifacts = artifact_profile(pr, policy, files, classification)
    architecture_scope = architecture_scope_profile(
        pr, files, policy, private_overrides
    )
    compact_pr = compact_pr_view(pr, acting_login)
    author_policy = {
        "frontend_review_allowed": frontend_review_allowed(
            compact_pr.get("author_login"), private_overrides
        )
    }
    proof = proof_profile(pr, contributor_summary, gittensor)
    if (
        files
        and classification.get("surface") == "unknown"
        and all(path.startswith("docs/") for path in files)
    ):
        # Docs-only maintenance has no runtime or parser boundary to prove. Keep
        # the risk flags visible for reviewer context, but do not turn contributor
        # scrutiny into a queue-safety proof blocker.
        proof["proof_required"] = False
        proof["proof_gap"] = False
    elif low_risk_refactor_proof_override(
        pr=compact_pr,
        files=files,
        classification=classification,
        checks=checks,
        parse_diff=parse_diff,
        proof=proof,
    ):
        proof["proof_satisfied"] = True
        proof["proof_gap"] = False
        proof["proof_override"] = "low_risk_parser_refactor_green_no_parse_changes"
    packet = {
        "schema_version": 1,
        "completeness": "complete" if mode == "full" else "triage",
        "acting_login": acting_login,
        "pr": compact_pr,
        "files": files,
        "classification": classification,
        "ci": checks,
        "parse_diff": parse_diff,
        "artifacts": artifacts,
        "architecture_scope": architecture_scope,
        "latest_maintainer_review_commit": latest_review_commit(pr, acting_login),
        "domain": {"rules_domain": policy.rules_domain},
        "policy": {
            "labels": {
                "frontend_deferred": policy.frontend_deferred_label,
                "quality": policy.quality_label,
            },
            "admission": {
                "mode": policy.admission_mode,
                "enforced_after": policy.admission_enforced_after,
                "accepted_issue_label": policy.architecture_accepted_issue_label,
                "architecture_span_threshold": policy.architecture_scope_span_threshold,
            },
            "architecture_scope": {
                "mode": policy.architecture_scope_mode,
                "span_threshold": policy.architecture_scope_span_threshold,
            },
            "requested_changes": {
                "warning_after_days": policy.requested_changes_warning_after_days,
                "close_after_warning_days": policy.requested_changes_close_after_warning_days,
                "warning_marker": REQUESTED_CHANGES_EXPIRY_MARKER,
            },
        },
        "author_policy": author_policy,
        "contributor": contributor_summary,
        "gittensor": gittensor,
        "proof": proof,
        "policy_trace": policy_trace(
            classification, (contributor_summary or {}).get("standing")
        )
        + [
            "architecture_issue_evidence:"
            + json_dumps(
                {
                    "records": architecture_scope["evidence"]["closing_issue_records"],
                    "count": architecture_scope["evidence"]["closing_issue_count"],
                    "complete": architecture_scope["evidence"][
                        "closing_issue_evidence_complete"
                    ],
                }
            )
        ],
        "local_current_event": local_event,
        "local_first_block_event": first_block_event,
    }
    packet["recommendation"] = recommend_from_packet(packet)
    return packet


def policy_trace(classification: dict[str, Any], standing: str | None = None) -> list[str]:
    # Trace records MATCHED patterns, not fired actions: a merged PR with hard-stop
    # paths still traces matched:hard_stop, and a skip-standing contributor traces
    # matched:standing_skip even when hard_stop wins the action ladder.
    trace = [
        "terminal_self",
        "hard_stop",
        "admission",
        "architecture_scope",
        "private_override",
        "standing",
        "safety_queue_freshness",
        "path_policy",
        "default",
    ]
    if classification.get("hard_stop_paths"):
        trace.append("matched:hard_stop")
    if standing == "skip":
        trace.append("matched:standing_skip")
    if classification.get("surface") == "frontend":
        trace.append("matched:frontend")
    if classification.get("surface") == "mixed":
        trace.append("matched:mixed")
    return trace


# ─── GraphQL queries and PR-node normalization ───────────────────────────────


def pr_node_fields(
    *,
    comments_last: int,
    include_full_reviews: bool,
    include_pr_body: bool,
    include_review_body: bool,
    include_comment_body: bool,
    status_contexts_first: int | None,
) -> str:
    """GraphQL selection set for a PR node, shared by the scan and single-PR queries.

    Only static field names are interpolated (counts and field toggles) — never
    user input, which travels as GraphQL variables.
    """
    pr_body = "body " if include_pr_body else ""
    review_body = " body" if include_review_body else ""
    comment_body = " body" if include_comment_body else ""
    full_reviews = (
        f"reviews(first:50){{nodes{{author{{login}} state submittedAt commit{{oid}}{review_body}}}}} "
        if include_full_reviews
        else ""
    )
    status_rollup = (
        "commits(last:20){nodes{commit{oid messageHeadline authors(first:10){nodes{name email user{login}}} "
        "statusCheckRollup{state contexts(first:"
        f"{status_contexts_first}"
        "){nodes{__typename "
        "... on CheckRun{name status conclusion} "
        "... on StatusContext{context state}}}}}}}"
        if status_contexts_first is not None
        else (
            "commits(last:20){nodes{commit{oid messageHeadline authors(first:10){nodes{name email user{login}}} "
            "statusCheckRollup{state}}}}"
        )
    )
    comments = (
        f"comments(last:{comments_last})"
        + "{totalCount pageInfo{hasNextPage endCursor} nodes{author{login} createdAt updatedAt"
        + comment_body
        + "}} "
    )
    return (
        f"number title {pr_body}state isDraft url createdAt updatedAt headRefName headRefOid "
        "baseRefName mergeStateStatus reviewDecision changedFiles "
        "author{login} "
        "closingIssuesReferences(first:20){totalCount pageInfo{hasNextPage endCursor} nodes{number state labels(first:20){totalCount pageInfo{hasNextPage endCursor} nodes{name}}}} "
        "labels(first:100){totalCount pageInfo{hasNextPage endCursor} nodes{name}} "
        "assignees(first:10){nodes{login}} "
        "isInMergeQueue mergeQueueEntry{position state} autoMergeRequest{enabledAt} "
        "files(first:100){totalCount pageInfo{hasNextPage endCursor} nodes{path}} "
        f"latestReviews(first:20){{nodes{{author{{login}} state submittedAt commit{{oid}}{review_body}}}}} "
        f"{full_reviews}"
        f"{comments}"
        f"{status_rollup}"
    )


SCAN_PR_QUERY = (
    "query($owner:String!,$name:String!,$first:Int!,$after:String){"
    "repository(owner:$owner,name:$name){"
    "pullRequests(states:[OPEN], first:$first, after:$after,"
    " orderBy:{field:CREATED_AT, direction:DESC}){"
    "pageInfo{hasNextPage endCursor}"
    "nodes{"
    + pr_node_fields(
        comments_last=100,
        include_full_reviews=True,
        include_pr_body=True,
        include_review_body=False,
        include_comment_body=True,
        status_contexts_first=80,
    )
    + "}"
    "}}}"
)

SINGLE_PR_QUERY = (
    "query($owner:String!,$name:String!,$number:Int!){"
    "repository(owner:$owner,name:$name){"
    "pullRequest(number:$number){"
    + pr_node_fields(
        comments_last=30,
        include_full_reviews=True,
        include_pr_body=True,
        include_review_body=True,
        include_comment_body=True,
        status_contexts_first=80,
    )
    + "}"
    "}}"
)


def graphql_nodes(container: Any) -> list[dict[str, Any]]:
    if not isinstance(container, dict):
        return []
    return [node for node in container.get("nodes", []) if isinstance(node, dict)]


def graphql_connection_complete(container: Any) -> bool:
    if not isinstance(container, dict) or not isinstance(container.get("totalCount"), int):
        return False
    page_info = container.get("pageInfo")
    return (
        isinstance(page_info, dict)
        and page_info.get("hasNextPage") is False
        and container["totalCount"] == len(graphql_nodes(container))
    )


def graphql_rollup_contexts(node: dict[str, Any]) -> list[dict[str, Any]]:
    commits = graphql_nodes(node.get("commits"))
    if not commits:
        return []
    rollup = (commits[-1].get("commit") or {}).get("statusCheckRollup")
    if not isinstance(rollup, dict):
        return []
    state = (rollup.get("state") or "").upper()
    if "contexts" not in rollup:
        if state in {"SUCCESS"}:
            return [
                {
                    "name": "statusCheckRollup",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                }
            ]
        if state in {"FAILURE", "ERROR"}:
            return [
                {
                    "name": "statusCheckRollup",
                    "status": "COMPLETED",
                    "conclusion": state,
                }
            ]
        if state:
            return [{"name": "statusCheckRollup", "status": "IN_PROGRESS", "conclusion": None}]
        return []
    checks = []
    for ctx in graphql_nodes(rollup.get("contexts")):
        if ctx.get("__typename") == "StatusContext":
            # Map legacy commit statuses onto the CheckRun shape status_summary expects:
            # a terminal state becomes COMPLETED with its state as the conclusion.
            state = ctx.get("state")
            checks.append(
                {
                    "name": ctx.get("context"),
                    "status": "COMPLETED" if state in {"SUCCESS", "ERROR", "FAILURE"} else "IN_PROGRESS",
                    "conclusion": state,
                }
            )
        else:
            checks.append(
                {
                    "name": ctx.get("name"),
                    "status": ctx.get("status"),
                    "conclusion": ctx.get("conclusion"),
                }
            )
    return checks


def graphql_commit_authors(node: dict[str, Any]) -> list[dict[str, Any]]:
    commits = []
    for item in graphql_nodes(node.get("commits")):
        commit = item.get("commit") or {}
        commits.append(
            {
                "oid": commit.get("oid"),
                "messageHeadline": commit.get("messageHeadline"),
                "authors": [
                    {
                        "login": ((author.get("user") or {}).get("login")),
                        "name": author.get("name"),
                        "email": author.get("email"),
                    }
                    for author in graphql_nodes(commit.get("authors"))
                ],
            }
        )
    return commits


def graphql_reviews(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {
            "author": {"login": (review.get("author") or {}).get("login")},
            "state": review.get("state"),
            "submittedAt": review.get("submittedAt"),
            "commit": review.get("commit"),
            "body": review.get("body"),
        }
        for review in nodes
    ]


def normalize_graphql_pr(node: dict[str, Any]) -> dict[str, Any]:
    """Adapt a GraphQL PR node into the gh `--json`-style shape downstream code reads."""
    latest_reviews = graphql_reviews(graphql_nodes(node.get("latestReviews")))
    full_reviews = graphql_nodes(node.get("reviews"))
    closing_issue_connection = node.get("closingIssuesReferences")
    closing_issue_nodes = graphql_nodes(closing_issue_connection)
    closing_issue_records = sorted(
        {
            (
                int(issue["number"]),
                str(issue.get("state") or ""),
                tuple(
                    sorted(
                        {
                            str(label.get("name"))
                            for label in graphql_nodes(issue.get("labels"))
                            if label.get("name")
                        },
                        key=str.casefold,
                    )
                ),
                graphql_connection_complete(issue.get("labels")),
            )
            for issue in closing_issue_nodes
            if issue.get("number") is not None
        },
        key=lambda record: (record[0], record[1], tuple(name.casefold() for name in record[2])),
    )
    labels_connection = node.get("labels")
    files_connection = node.get("files")
    return {
        "number": node.get("number"),
        "title": node.get("title"),
        "body": node.get("body"),
        "state": node.get("state"),
        "isDraft": node.get("isDraft"),
        "url": node.get("url"),
        "createdAt": node.get("createdAt"),
        "updatedAt": node.get("updatedAt"),
        "headRefName": node.get("headRefName"),
        "headRefOid": node.get("headRefOid"),
        "baseRefName": node.get("baseRefName"),
        "mergeStateStatus": node.get("mergeStateStatus"),
        "reviewDecision": node.get("reviewDecision"),
        "changedFiles": node.get("changedFiles"),
        "author": {"login": (node.get("author") or {}).get("login")},
        "closingIssuesReferences": [
            {
                "number": number,
                "state": state,
                "labels": [{"name": name} for name in labels],
                "labelsComplete": labels_complete,
            }
            for number, state, labels, labels_complete in closing_issue_records
        ],
        "closingIssuesReferencesCount": (
            closing_issue_connection.get("totalCount")
            if isinstance(closing_issue_connection, dict)
            else None
        ),
        "closingIssuesReferencesComplete": graphql_connection_complete(
            closing_issue_connection
        )
        and all(graphql_connection_complete(issue.get("labels")) for issue in closing_issue_nodes),
        "labels": [{"name": label.get("name")} for label in graphql_nodes(labels_connection)],
        "labelsComplete": graphql_connection_complete(labels_connection),
        "assignees": [{"login": a.get("login")} for a in graphql_nodes(node.get("assignees"))],
        "isInMergeQueue": node.get("isInMergeQueue"),
        "mergeQueueEntry": node.get("mergeQueueEntry"),
        "autoMergeRequest": node.get("autoMergeRequest"),
        "files": [{"path": f.get("path")} for f in graphql_nodes(files_connection)],
        "filesComplete": graphql_connection_complete(files_connection),
        "comments": [
            {
                "author": {"login": (c.get("author") or {}).get("login")},
                "createdAt": c.get("createdAt"),
                "updatedAt": c.get("updatedAt"),
                "body": c.get("body"),
            }
            for c in graphql_nodes(node.get("comments"))
        ],
        "commentsComplete": graphql_connection_complete(node.get("comments")),
        "latestReviews": latest_reviews,
        "reviews": graphql_reviews(full_reviews) if full_reviews else latest_reviews,
        "commits": graphql_commit_authors(node),
        "statusCheckRollup": graphql_rollup_contexts(node),
    }


def fetch_open_prs(repo: str, limit: int) -> list[dict[str, Any]]:
    owner, name = repo.split("/", 1)
    nodes: list[dict[str, Any]] = []
    cursor: str | None = None
    while len(nodes) < limit:
        page_size = min(limit - len(nodes), SCAN_PAGE_SIZE)
        variables = [
            "-f",
            f"owner={owner}",
            "-f",
            f"name={name}",
            "-F",
            f"first={page_size}",
        ]
        if cursor:
            variables += ["-f", f"after={cursor}"]
        result = run_json(["gh", "api", "graphql", "-f", f"query={SCAN_PR_QUERY}", *variables])
        connection = ((result.get("data") or {}).get("repository") or {}).get("pullRequests") or {}
        nodes.extend(graphql_nodes(connection))
        page_info = connection.get("pageInfo", {})
        if not page_info.get("hasNextPage"):
            break
        cursor = page_info.get("endCursor")
    return nodes[:limit]


def gh_pr_view(repo: str, pr_number: int) -> dict[str, Any]:
    owner, name = repo.split("/", 1)
    result = run_json(
        [
            "gh",
            "api",
            "graphql",
            "-f",
            f"owner={owner}",
            "-f",
            f"name={name}",
            "-F",
            f"number={int(pr_number)}",
            "-f",
            f"query={SINGLE_PR_QUERY}",
        ]
    )
    node = ((result.get("data") or {}).get("repository") or {}).get("pullRequest")
    return normalize_graphql_pr(node or {})


def required_status_check_names(repo: str, base_ref: str | None) -> set[str] | None:
    """Return the actual branch-protection check names for a PR base branch.

    `statusCheckRollup` is deliberately broader than merge requirements. The
    branch-protection endpoint is GitHub's source of truth for legacy required
    status checks; a failed lookup returns ``None`` so callers preserve the
    conservative all-checks fallback instead of treating unknown requirements as
    green.
    """
    if not base_ref:
        return None
    try:
        required = run_json(
            [
                "gh",
                "api",
                "repos/"
                f"{repo}/branches/{urllib.parse.quote(base_ref, safe='')}/"
                "protection/required_status_checks",
            ]
        )
    except subprocess.CalledProcessError:
        print(
            f"could not read required status checks for {repo}@{base_ref}; "
            "using the conservative all-checks fallback",
            file=sys.stderr,
        )
        return None
    if not isinstance(required, dict):
        return None
    names = {
        str(name)
        for name in required.get("contexts", [])
        if isinstance(name, str) and name
    }
    names.update(
        str(check.get("context"))
        for check in required.get("checks", [])
        if isinstance(check, dict) and check.get("context")
    )
    return names


def required_checks_by_base(
    repo: str, prs: list[dict[str, Any]]
) -> dict[str, set[str] | None]:
    return {
        base_ref: required_status_check_names(repo, base_ref)
        for base_ref in sorted(
            {
                str(pr.get("baseRefName"))
                for pr in prs
                if pr.get("baseRefName")
            },
            key=str.casefold,
        )
    }


# ─── Commands ────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class ReviewContext:
    """Everything a packet needs besides the PR node itself, loaded once per command.

    One event-log read feeds the head-freshness index, contributor analytics, and
    signal recurrence for every packet a command builds — scan amortizes it across
    the whole sweep; inspect/recommend build it for their single PR.
    """

    policy: Policy
    private_overrides: dict[str, Any]
    acting_login: str
    local_events: dict[tuple[int, str], dict[str, Any]]
    local_latest_events: dict[int, dict[str, Any]]
    first_block_events: dict[tuple[int, str], dict[str, Any]]
    analytics_model: dict[str, Any]
    signal_occurrences: dict[str, list[dict[str, Any]]]
    gittensor_index: dict[str, dict[str, Any]]
    gittensor_warning: str | None


def load_review_context(args: argparse.Namespace) -> ReviewContext:
    events = all_events(args.state_dir)
    gittensor_records, gittensor_warning = fetch_gittensor_records(
        getattr(args, "gittensor_api_url", DEFAULT_GITTENSOR_API_URL)
    )
    return ReviewContext(
        policy=load_policy(args.config),
        private_overrides=load_private_overrides(args.state_dir),
        acting_login=args.acting_login or gh_user(),
        local_events=latest_events_by_pr_head(events),
        local_latest_events=latest_events_by_pr(events),
        first_block_events=first_block_events_by_pr_head(events),
        analytics_model=build_analytics_model(
            events,
            days=None,
            author=None,
            min_prs=ANALYTICS_DEFAULT_MIN_PRS,
            include_open=True,
        ),
        signal_occurrences=collect_signal_occurrences(events),
        gittensor_index=build_gittensor_index(gittensor_records),
        gittensor_warning=gittensor_warning,
    )


def packet_for_pr(
    context: ReviewContext,
    pr: dict[str, Any],
    mode: str,
    required_check_names: set[str] | None = None,
) -> dict[str, Any]:
    """Assemble the full packet for one normalized PR view."""
    pr_number = int(pr.get("number") or 0)
    head_key = (pr_number, pr.get("headRefOid") or "")
    local_event = context.local_events.get(head_key)
    local_latest_event = context.local_latest_events.get(pr_number)
    first_block_event = context.first_block_events.get(head_key)
    contributor_summary = build_contributor_summary(
        (pr.get("author") or {}).get("login"),
        pr_number,
        context.analytics_model,
        context.signal_occurrences,
        context.private_overrides,
    )
    gittensor = gittensor_summary(
        (pr.get("author") or {}).get("login"),
        context.gittensor_index,
        context.gittensor_warning,
    )
    packet = make_packet(
        pr,
        context.policy,
        context.acting_login,
        mode,
        context.private_overrides,
        local_event,
        contributor_summary,
        gittensor,
        first_block_event,
        required_check_names,
    )
    packet["local_latest_event"] = local_latest_event
    packet["freshness"] = review_freshness(
        packet["pr"], context.acting_login, local_event, local_latest_event
    )
    packet["recommendation"] = recommend_from_packet(packet)
    return packet


def candidate_sort_key(candidate: dict[str, Any]) -> tuple[Any, ...]:
    action = candidate.get("advisory_action") or ""
    created = candidate.get("created_at") or ""
    updated = candidate.get("updated_at") or ""
    pr_number = candidate.get("pr") or 0
    order = CANDIDATE_ACTION_ORDER.get(action, 99)
    if action == "review":
        return (order, created, pr_number)
    if action in {
        "close_stale_changes_for_handler",
        "dequeue_stale_for_handler",
        "update_branch_for_handler",
        "approve_ready_for_handler",
        "warn_stale_changes_for_handler",
    }:
        return (order, updated, created, pr_number)
    return (order, pr_number)


def scan_candidate(pr: dict[str, Any], packet: dict[str, Any]) -> dict[str, Any]:
    """Project a full packet down to the token-minimal triage row scan prints."""
    contributor = packet.get("contributor") or {}
    return {
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
        "parse_diff": packet["parse_diff"],
        "artifacts": packet.get("artifacts"),
        "architecture_scope": packet.get("architecture_scope"),
        "freshness": packet.get("freshness"),
        "review_decision": pr.get("reviewDecision"),
        "is_in_merge_queue": packet["pr"].get("isInMergeQueue"),
        "merge_queue_entry": packet["pr"].get("mergeQueueEntry"),
        "auto_merge_request": packet["pr"].get("autoMergeRequest"),
        "advisory_action": packet["recommendation"]["advisory_action"],
        "reason": packet["recommendation"]["reason"],
        "policy_trace": packet["policy_trace"],
        "standing": contributor.get("standing"),
        "scrutiny": contributor.get("scrutiny"),
        "first_contribution": contributor.get("first_contribution"),
        "gittensor": packet.get("gittensor"),
        "proof": packet.get("proof"),
    }


def command_scan(args: argparse.Namespace) -> int:
    context = load_review_context(args)
    # The repo-wide scan uses small GraphQL pages, but includes individual check
    # contexts so its CI verdict can be restricted to the base branch's actual
    # merge requirements rather than every third-party status.
    nodes = fetch_open_prs(args.repo, args.limit)
    prs = [normalize_graphql_pr(node) for node in nodes]
    required_checks = required_checks_by_base(args.repo, prs)
    candidates = []
    for pr in prs:
        packet = packet_for_pr(
            context,
            pr,
            "full",
            required_checks.get(pr.get("baseRefName") or ""),
        )
        candidates.append(scan_candidate(pr, packet))

    candidates.sort(key=candidate_sort_key)
    candidates_by_action: dict[str, list[dict[str, Any]]] = {}
    for candidate in candidates:
        candidates_by_action.setdefault(candidate["advisory_action"], []).append(candidate)
    action_counts = {action: len(items) for action, items in candidates_by_action.items()}
    artifact_passes = sum(
        1 for candidate in candidates if (candidate.get("artifacts") or {}).get("passes")
    )
    artifact_failures: dict[str, int] = {}
    for candidate in candidates:
        for failure in (candidate.get("artifacts") or {}).get("failures") or []:
            artifact_failures[failure] = artifact_failures.get(failure, 0) + 1
    architecture_triggered = sum(
        1
        for candidate in candidates
        if (candidate.get("architecture_scope") or {}).get("triggered")
    )
    architecture_would_decline = sum(
        1
        for candidate in candidates
        if (candidate.get("architecture_scope") or {}).get("would_decline")
    )
    output = {
        "acting_login": context.acting_login,
        "completeness": "triage",
        "action_counts": action_counts,
        "candidates_by_action": candidates_by_action,
        "audit_summary": {
            "prs": len(candidates),
            "artifact_passes": artifact_passes,
            "artifact_pass_rate": (
                artifact_passes / len(candidates) if candidates else None
            ),
            "artifact_failures": artifact_failures,
            "architecture_triggered": architecture_triggered,
            "architecture_would_decline": architecture_would_decline,
        },
    }
    if len(candidates) == args.limit:
        output["warnings"] = [
            f"open PR count reached --limit {args.limit}; increase --limit"
        ]
    print(json_dumps(output))
    return 0


def event_skeleton(pr_number: int, compact_pr: dict[str, Any]) -> dict[str, Any]:
    # Timestamp is prefilled because event_id hashes it: an agent that fills the
    # skeleton and pipes it to `record --event-json -` gets idempotent retries.
    return {
        "event_type": "<FILL: review|changes_requested|blocked|decline|approved_enqueued|deferred|held|requested_changes_warning|stale_changes_closed|review_correction>",
        "pr": pr_number,
        "head_sha": compact_pr.get("headRefOid"),
        "author": compact_pr.get("author_login"),
        "timestamp": now_iso(),
        "outcome": "<FILL or omit>",
        "summary": "<FILL>",
        "signals": f"<FILL or omit: [] or subset of {sorted(QUALITY_SIGNAL_VOCAB)}>",
    }


def command_inspect(args: argparse.Namespace) -> int:
    context = load_review_context(args)
    pr = gh_pr_view(args.repo, args.pr)
    packet = packet_for_pr(
        context,
        pr,
        args.mode,
        required_status_check_names(args.repo, pr.get("baseRefName")),
    )
    if args.emit_event:
        packet["event_skeleton"] = event_skeleton(args.pr, packet["pr"])
        signals = proof_tracking_signals(packet)
        if signals:
            packet["event_skeleton"]["signals"] = signals
    print(json_dumps(packet))
    return 0


def command_recommend(args: argparse.Namespace) -> int:
    context = load_review_context(args)
    pr = gh_pr_view(args.repo, args.pr)
    packet = packet_for_pr(
        context,
        pr,
        "full",
        required_status_check_names(args.repo, pr.get("baseRefName")),
    )
    recommendation = packet["recommendation"]
    if packet["completeness"] != "complete" and recommendation["advisory_action"].endswith("_for_handler"):
        recommendation = {
            "pr": args.pr,
            "head_sha": pr.get("headRefOid"),
            "advisory_action": "hold_ci",
            "reason": "insufficient_data",
            "requires_live_verification": False,
            "policy_trace": packet.get("policy_trace", []),
            "contributor": packet.get("contributor"),
        }
    if args.emit_event:
        recommendation = dict(recommendation)
        recommendation["event_skeleton"] = event_skeleton(args.pr, packet["pr"])
        signals = proof_tracking_signals(packet)
        if signals:
            recommendation["event_skeleton"]["signals"] = signals
    print(json_dumps(recommendation))
    return 0


def read_event_arg(value: str) -> dict[str, Any]:
    if value == "-":
        return json.loads(sys.stdin.read())
    return json.loads(Path(value).read_text(encoding="utf-8"))


def event_validation_error(
    event: dict[str, Any], existing_events: list[dict[str, Any]] | None = None
) -> str | None:
    event_type = event.get("event_type")
    if event_type not in ALLOWED_EVENT_TYPES:
        return f"event_type {event_type!r} is not in the allowed vocabulary"
    outcome = event.get("outcome")
    if outcome is not None and outcome not in ALLOWED_OUTCOMES:
        return f"outcome {outcome!r} is not in the allowed vocabulary"
    if event_type != "quality_entry" and not isinstance(event.get("pr"), int):
        return "pr (int) is required for non-quality_entry events"
    signals = event.get("signals")
    if signals is not None:
        if not isinstance(signals, list) or not all(
            isinstance(signal, str) for signal in signals
        ):
            return "signals must be a list of strings"
        unknown = sorted(set(signals) - QUALITY_SIGNAL_VOCAB)
        if unknown:
            return f"signals {unknown} are not in the allowed vocabulary"
    if event_type == "review_correction":
        target_id = event.get("corrects_event_id")
        if not isinstance(target_id, str) or not target_id:
            return "review_correction requires corrects_event_id"
        if not signals:
            return "review_correction requires a non-empty signals subset"
        if len(signals) != len(set(signals)):
            return "review_correction signals must not contain duplicates"
        if event.get("outcome") is not None:
            return "review_correction must not carry an outcome"
        target = next(
            (
                candidate
                for candidate in existing_events or []
                if candidate.get("event_id") == target_id
            ),
            None,
        )
        if target is None:
            return f"review_correction target {target_id!r} was not found"
        if target.get("event_type") == "review_correction":
            return "review_correction cannot target another correction"
        for field in ("pr", "head_sha"):
            if event.get(field) != target.get(field):
                return f"review_correction {field} must match the target event"
        event_author = event.get("author")
        target_author = target.get("author")
        if (
            event_author is None
            or target_author is None
            or fold_login(str(event_author)) != fold_login(str(target_author))
        ):
            return "review_correction author must match the target event"
        target_signals = {
            canonical
            for signal in target.get("signals") or []
            if (canonical := canonical_signal(str(signal))) is not None
        }
        if not set(signals).issubset(target_signals):
            return "review_correction signals must be an exact subset of target signals"
        idempotent_retry = any(
            candidate.get("event_id") == event.get("event_id")
            and candidate.get("event_type") == "review_correction"
            and candidate.get("corrects_event_id") == target_id
            and candidate.get("signals") == signals
            for candidate in existing_events or []
        )
        already_retracted = {
            canonical
            for candidate in existing_events or []
            if candidate.get("event_type") == "review_correction"
            and candidate.get("corrects_event_id") == target_id
            for signal in candidate.get("signals") or []
            if (canonical := canonical_signal(str(signal))) is not None
        }
        if not idempotent_retry and set(signals) & already_retracted:
            return "review_correction signals include an already-retracted occurrence"
    return None


def command_record(args: argparse.Namespace) -> int:
    event = read_event_arg(args.event_json)
    # Lower-case the outcome before normalization so the write-time value (and the
    # event_id that hashes it) is the canonical lowercase form.
    if isinstance(event.get("outcome"), str):
        event["outcome"] = event["outcome"].lower()
    normalized = normalize_event(event)
    existing_events = all_events(args.state_dir)
    error = event_validation_error(normalized, existing_events)
    if error is not None and not args.force:
        print(
            json_dumps(
                {
                    "inserted": False,
                    "error": error,
                    "allowed_event_types": sorted(ALLOWED_EVENT_TYPES),
                    "allowed_outcomes": sorted(ALLOWED_OUTCOMES),
                    "allowed_signals": sorted(QUALITY_SIGNAL_VOCAB),
                }
            )
        )
        return 1
    inserted = append_event(args.state_dir, normalized)
    result = {"inserted": inserted, "event_id": normalized["event_id"]}
    if error is not None:
        result["forced"] = True
    print(json_dumps(result))
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
    # The recognized tokens are the signal vocabulary itself — one authority with
    # record-time validation and the event_skeleton hint.
    signals = [token for token in sorted(QUALITY_SIGNAL_VOCAB) if token in body]
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


def command_check_skill_sync(args: argparse.Namespace) -> int:
    # `.agents/skills` is a symlink to `.claude/skills`, so a byte-compare is
    # vacuous. Verify the symlink still points at the canonical directory instead.
    link = REPO_ROOT / ".agents/skills"
    expected = (REPO_ROOT / ".claude/skills").resolve()
    is_symlink = link.is_symlink()
    resolved_target = link.resolve() if is_symlink else None
    synced = is_symlink and resolved_target == expected
    print(
        json_dumps(
            {
                "synced": synced,
                "is_symlink": is_symlink,
                "target": str(resolved_target) if resolved_target is not None else None,
            }
        )
    )
    return 0 if synced else 1


def command_compact(args: argparse.Namespace) -> int:
    all_state_events = all_events(args.state_dir)
    effective_signals = effective_signals_by_event(all_state_events)
    events = filtered_events_by_days(all_state_events, args.days)
    prs: dict[str, dict[str, Any]] = {}
    contributors: dict[str, dict[str, Any]] = {}
    for event in events:
        if event.get("event_type") == "review_correction":
            continue
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
                fold_login(str(author)),
                {"login": author, "events": 0, "signals": {}, "latest_timestamp": None},
            )
            entry["events"] += 1
            entry["latest_timestamp"] = event.get("timestamp")
            event_signals = effective_signals.get(
                str(event.get("event_id") or ""), []
            )
            for signal in list((event.get("quality") or {}).get("signals") or []) + list(
                event_signals
            ):
                canonical = canonical_signal(str(signal))
                if canonical is None:
                    continue
                entry["signals"][canonical] = entry["signals"].get(canonical, 0) + 1
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
    events = all_events(args.state_dir)
    model = build_analytics_model(
        events,
        days=args.days,
        author=args.author,
        min_prs=args.min_prs,
        include_open=args.include_open or args.refresh_github,
    )
    if args.refresh_github:
        # Refresh and open-PR filtering only mutate the PR rows; contributor
        # aggregation + repo medians are recomputed exactly once afterward.
        apply_github_refresh(model, args.repo)
        if not args.include_open:
            model["prs"] = [pr for pr in model["prs"] if not pr["is_open_or_pending"]]
        model["filters"]["include_open"] = args.include_open
        finalize_contributor_model(model, min_prs=args.min_prs, author=args.author, refreshed=True)
    model["contributors"] = sorted_contributors(model["contributors"], args.sort, args.limit)
    if args.format == "json":
        print(json.dumps(model, indent=2, sort_keys=True))
    else:
        print(render_analytics_ascii(model, args))
    return 0


# ─── CLI wiring ──────────────────────────────────────────────────────────────


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
    parser.add_argument(
        "--gittensor-api-url",
        default=DEFAULT_GITTENSOR_API_URL,
        help="Set empty to disable public Gittensor PR-history enrichment.",
    )


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
    inspect.add_argument("--emit-event", action="store_true")
    inspect.set_defaults(func=command_inspect)

    recommend = sub.add_parser("recommend")
    add_common(recommend)
    recommend.add_argument("pr", type=int)
    recommend.add_argument("--emit-event", action="store_true")
    recommend.set_defaults(func=command_recommend)

    record = sub.add_parser("record")
    add_state(record)
    record.add_argument("--event-json", required=True)
    record.add_argument("--force", action="store_true")
    record.set_defaults(func=command_record)

    import_cmd = sub.add_parser("import")
    add_state(import_cmd)
    import_cmd.add_argument("--tracker", type=existing_path)
    import_cmd.add_argument("--quality", type=existing_path)
    import_cmd.set_defaults(func=command_import)

    compact = sub.add_parser("compact")
    add_state(compact)
    compact.add_argument("--days", type=int, default=None)
    compact.set_defaults(func=command_compact)

    analytics = sub.add_parser("analytics")
    add_state(analytics)
    analytics.add_argument("--author")
    analytics.add_argument("--days", type=int, default=None)
    analytics.add_argument("--min-prs", type=int, default=ANALYTICS_DEFAULT_MIN_PRS)
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

    skill_sync = sub.add_parser("check-skill-sync")
    skill_sync.set_defaults(func=command_check_skill_sync)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    finalize_state_dir(args)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
