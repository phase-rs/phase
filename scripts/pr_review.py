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
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_POLICY = REPO_ROOT / ".agents/pr-review-policy.toml"


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
    checks = packet.get("ci", {})
    classification = packet.get("classification", {})
    latest_commit = packet.get("latest_maintainer_review_commit")
    review_decision = pr.get("reviewDecision")
    queue = bool(pr.get("isInMergeQueue"))
    local_event = packet.get("local_current_event") or {}
    local_event_type = local_event.get("event_type")
    local_outcome = local_event.get("outcome")

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
    elif local_event_type == "held":
        action = "blocked"
        reason = "local_hold_current_head"
    elif local_event_type in {
        "review_blocked",
        "changes_requested",
        "blocked",
    } or local_outcome in {
        "changes_requested",
        "reviewed_request_changes",
        "blocked",
    }:
        action = "blocked"
        reason = "local_block_current_head"
    elif latest_commit and latest_commit != head and review_decision == "APPROVED":
        action = "dequeue_stale_for_handler" if queue else "review"
        reason = "stale_approval"
    elif queue and review_decision == "APPROVED":
        action = "queued"
        reason = "already_in_merge_queue"
    elif checks.get("state") == "failed":
        action = "request_changes"
        reason = "ci_failed"
    elif checks.get("state") in {"pending", "unknown"}:
        action = "hold_ci"
        reason = "ci_not_green"
    elif classification.get("surface") == "frontend":
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


def make_packet(pr: dict[str, Any], policy: Policy, acting_login: str, mode: str) -> dict[str, Any]:
    files = pr_files_from_view(pr)
    classification = classify_files(files, policy)
    checks = status_summary(pr.get("statusCheckRollup", []))
    compact_pr = compact_pr_view(pr, acting_login)
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
        "isInMergeQueue mergeQueueEntry{position state}"
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
        return {"isInMergeQueue": None, "mergeQueueEntry": None}
    pull = result.get("data", {}).get("repository", {}).get("pullRequest", {})
    return {
        "isInMergeQueue": pull.get("isInMergeQueue"),
        "mergeQueueEntry": pull.get("mergeQueueEntry"),
    }


def command_scan(args: argparse.Namespace) -> int:
    policy = load_policy(args.config)
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
        packet = make_packet(pr, policy, acting_login, "light")
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
            packet = make_packet(pr, policy, acting_login, "full")
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
    acting_login = args.acting_login or gh_user()
    pr = gh_pr_view(args.repo, args.pr)
    packet = make_packet(pr, policy, acting_login, args.mode)
    print(json_dumps(packet))
    return 0


def command_recommend(args: argparse.Namespace) -> int:
    policy = load_policy(args.config)
    acting_login = args.acting_login or gh_user()
    pr = gh_pr_view(args.repo, args.pr)
    packet = make_packet(pr, policy, acting_login, "full")
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
