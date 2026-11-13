#!/usr/bin/env python3
"""Weekly PR review-flow metrics, change ledger and review-sample statistics.

Reads GitHub (read-only GraphQL through pr_review.run_json) and the pr_review
event log (through pr_review.all_events). Each live measurement is frozen into
an immutable snapshot; every metric is computed from a snapshot alone, so a
record can always be recomputed exactly. Nothing here writes to GitHub or to
the event log.
"""

from __future__ import annotations

import argparse
import collections
import csv
import fcntl
import functools
import gzip
import hashlib
import json
import math
import os
import random
import re
import statistics
import subprocess
import sys
import tempfile
from datetime import UTC, date, datetime, timedelta
from pathlib import Path
from typing import Any, Callable

import pr_review

SCHEMA_VERSION = 1
RUBRIC_VERSION = 1
SETTLED_AGE_DAYS = 28
DEFAULT_DATA_DIR = pr_review.REPO_ROOT / ".agents/pr-flow-audit/weekly"
BOTS = frozenset({
    "coderabbitai", "github-actions", "dependabot", "codecov", "codspeed-hq", "vercel",
    "cloudflare-workers-and-pages", "gemini-code-assist", "copilot-pull-request-reviewer",
    "netlify", "superagent-security", "chatgpt-codex-connector",
})
CODERABBIT = "coderabbitai"
SIZE_BUCKETS = (("<200", 0, 200), ("200-999", 200, 1000), ("1000-1999", 1000, 2000), (">=2000", 2000, 10**12))
SPELL_LONG_HOURS = 72
PICKUP_LATE_HOURS = (24, 72)
QUEUED_REASONS = {"approved_enqueued", "enqueued", "queued"}

# field path -> (version, source, one-line definition). A definition change bumps the
# version, which changes definitions_digest and permits a second record per week.
REGISTRY: dict[str, tuple[int, str, str]] = {
    "counts": (1, "GH", "External PRs opened/merged/closed-unmerged/open in the window; maintainer-authored excluded."),
    "skill_rounds": (1, "GH", "Per PR, maintainer reviews in CHANGES_REQUESTED or DISMISSED (v1 rounds_mw)."),
    "rounds_all": (1, "GH", "skill_rounds plus CodeRabbit reviews with at least one actionable comment."),
    "share_first_review_approved": (1, "GH", "First maintainer review APPROVED / PRs with any maintainer review."),
    "share_ge4_rounds": (1, "GH", "skill_rounds >= 4 / all external PRs in the window."),
    "share_zero_or_one_round_merged": (1, "GH", "Merged PRs with skill_rounds <= 1 / merged PRs."),
    "rounds_by_size_bucket": (1, "GH", "Mean skill_rounds by additions+deletions bucket."),
    "coderabbit.reviews": (1, "GH", "CodeRabbit review count."),
    "coderabbit.inline_comments": (1, "GH", "Sum of comments.totalCount on CodeRabbit reviews."),
    "coderabbit.reviews_per_pr_mean": (1, "GH", "Mean CodeRabbit reviews per external PR."),
    "coderabbit.inline_per_pr_mean": (1, "GH", "Mean CodeRabbit inline comments per external PR."),
    "coderabbit.share_of_bot_items": (1, "GH", "CodeRabbit items / items by the acting login, CodeRabbit and BOTS (review, inline comment, issue comment)."),
    "notification_load_per_pr": (1, "GH", "Per reviewer class: reviews + issue comments + inline comments per PR (median, p90)."),
    "conflict_episodes_per_pr_mean": (1, "GH", "Proxy: main-merge commits + force pushes per PR."),
    "main_merges_per_pr_mean": (1, "GH", "Merge commits from main per PR, by commit headline."),
    "pushes_after_first_review_median": (1, "GH", "Force pushes + commit bursts (>10 min apart) after the first bot review."),
    "time_open_to_first_review_h_median": (1, "GH", "Hours from creation to the first review by either bot class."),
    "time_first_review_to_merge_h": (1, "GH", "Hours from the first review to merge, merged PRs only."),
    "abandonment": (1, "GH", "rounds_all >= 2 and (closed unmerged, or open and stale > 7 days)."),
    "per_author_top10": (1, "GH", "Top 10 authors by PR count with median skill_rounds and live open PR count."),
    "right_censored": (1, "GH", "Open PRs in the window and their mean skill_rounds so far (lower bound)."),
    "truncation_repaired_prs": (1, "GH", "PRs whose reviews or comments needed follow-up pages."),
    "pipeline_level": (2, "GH", "none / template_only / full from pr_review.artifact_profile on the PR body; full means both SHA lines are filled, for any head."),
    "rounds_v2.per_pr": (1, "GH", "Distinct commit oids over maintainer reviews whose effective state is CHANGES_REQUESTED."),
    "rounds_v2.share_ge4": (1, "GH", "v2 rounds >= 4 / external PRs."),
    "rounds_v2.share_zero_or_one_round_merged": (1, "GH", "Merged PRs with v2 rounds <= 1 / merged PRs."),
    "rounds_v2.by_size_bucket": (1, "GH", "Mean v2 rounds by size bucket."),
    "rounds_v2.right_censored_mean": (1, "GH", "Mean v2 rounds over open PRs."),
    "rounds_v2.same_head_supplements": (1, "GH", "Effective CHANGES_REQUESTED reviews beyond the first per (PR, oid)."),
    "rounds_v2.dismissed_approvals_excluded": (1, "GH", "DISMISSED reviews whose previous state was APPROVED."),
    "rounds_v2.dismissed_state_unknown": (1, "GH", "DISMISSED reviews with no matching dismissal event (counted as CHANGES_REQUESTED)."),
    "rounds_v2.null_commit_reviews": (1, "GH", "Effective CHANGES_REQUESTED reviews with no commit (excluded)."),
    "rounds_v2.main_merge_head_share": (1, "GH", "Share of v2 round heads that are merge commits from main."),
    "maintainer_posts_per_pr": (1, "GH", "Maintainer reviews + issue comments per PR, inline comments excluded."),
    "log.coverage": (1, "LOG", "Cohort PRs, PRs with events, events used, untimed events dropped, last event time."),
    "log.held": (1, "LOG", "Held spells: count, censored, hours, share over 72 h, share with 72 h of silence."),
    "log.warned": (1, "LOG", "Requested-changes warning spells: count, censored, hours."),
    "log.queue_exit": (1, "LOG", "Queued spells followed by a non-terminal state; return and unreturned distributions."),
    "log.assignee_gate": (1, "GH+LOG", "Foreign assignee intervals and the observations and material events inside them."),
    "log.head_pickup": (1, "GH+LOG", "Hours from a head's arrival to the first logged event on it; an upper bound, since a normal push is dated by its commit."),
    "log.maintainer_fixup_pushes": (1, "LOG", "fixup_push events on cohort PRs."),
}
SAMPLE_PREFIX = "sample."
BOOKKEEPING_KEYS = frozenset({
    "kind", "schema_version", "week", "window", "fetched_at", "computed_at", "age_days",
    "age_class", "definitions", "definitions_digest", "snapshot", "graphql_calls",
    "main_head_sha", "acting_login", "caveats",
})
CAVEATS = (
    "Maintainer-login reviews on others' PRs are treated as skill output; CodeRabbit is a Bot account.",
    "v1 skill_rounds counts CHANGES_REQUESTED + DISMISSED; rounds_v2 counts distinct heads with an effective CHANGES_REQUESTED.",
    "Rounds for still-open PRs are right-censored; open spells are measured up to fetched_at.",
    "conflict_episodes is a proxy (main merges + force pushes); GitHub exposes no conflict event.",
    "Abandonment staleness is measured at fetched_at.",
    "Window is by PR created date (UTC); PR state is as of fetched_at.",
)

RATING_COLUMNS = ("pr", "round", "fid", "location", "origin", "late", "severity", "latent", "bot_treatment", "outcome", "late_evidence", "anchor", "finding")
IDENTITY_COLUMNS = ("pr", "round", "fid", "anchor", "finding")
DRIFT_COLUMNS = ("pr", "round", "fid", "location", "origin", "late", "severity")
VOCAB = {
    "location": {"IN_DIFF", "ADJACENT", "ELSEWHERE", "PROCESS", "UNSURE"},
    "origin": {"ORIGINAL", "FIX_INDUCED", "MAIN_DRIFT", "REVIEWER_REVERSAL", "FALSE_POSITIVE", "UNSURE"},
    "late": {"LATE", "NOT_LATE", "UNSURE", "-"},
    "severity": {"BLOCKER", "SHOULD", "NIT", "UNSURE"},
    "latent": {"yes", "no"},
    "bot_treatment": {"blocking", "non-blocking", "UNSURE"},
    "outcome": {"fixed", "open", "ignored", "disputed-withdrawn", "abandoned", "UNSURE"},
}
DRIFT_THRESHOLDS = {"location": 16, "origin": 15, "late": 16, "severity": 14}
# The drift axes each sample.post_r1 statistic reads; a week whose record lists any of them as unreliable
# is ineligible for that metric. sample.<axis>_unsure fields read their own axis (see required_axes).
POST_R1_AXES = {
    "late_share": ("late",), "fix_induced_share": ("origin",), "blocker_share": ("severity",),
    "location_share": ("location",), "late_blocker_count": ("late", "severity"),
    "fix_induced_blocker_count": ("origin", "severity"), "nit_blocking": ("severity",), "latent_blocking": (),
}
SAMPLE_TOP, SAMPLE_RANDOM, SAMPLE_ROUND_CAP, DRIFT_SIZE = 3, 5, 6, 20

CHANGE_KEYS = {"id", "date", "phase", "hypothesis", "review_after", "switch", "targets", "note"}
TARGET_KEYS = {"metric", "version", "age_class", "role", "direction", "baseline", "expected", "measurable_from"}
ROLES = {"primary", "secondary", "guard"}
DIRECTIONS = {"decrease", "increase", "hold"}
AGE_CLASSES = {"early", "settled"}
VERDICTS = {"improved", "no_change", "regressed", "inconclusive"}
VERDICT_KEYS = {"id", "week", "verdict", "note"}


class Refusal(Exception):
    """A validation or completeness failure: exit 2, nothing written."""


def utcnow() -> datetime:
    return datetime.now(UTC).replace(microsecond=0)


def iso(value: datetime) -> str:
    return value.isoformat().replace("+00:00", "Z")


def parse_time(value: str | None) -> datetime | None:
    return pr_review.parse_event_datetime(value)


def parse_date(value: Any, what: str) -> date:
    try:
        return date.fromisoformat(value)
    except (TypeError, ValueError):
        raise Refusal(f"{what} must be YYYY-MM-DD, got {value!r}") from None


# ─── statistics helpers ──────────────────────────────────────────────────────


def pct(values: list[float], q: float) -> float | None:
    values = sorted(values)
    if not values:
        return None
    k = (len(values) - 1) * q
    i = int(k)
    j = min(i + 1, len(values) - 1)
    return round(values[i] + (values[j] - values[i]) * (k - i), 2)


def med(values: list[float]) -> float | None:
    return round(statistics.median(values), 2) if values else None


def mean(values: list[float]) -> float | None:
    return round(statistics.mean(values), 2) if values else None


def dist(values: list[float]) -> dict[str, Any]:
    return {"n": len(values), "median": med(values), "mean": mean(values), "p75": pct(values, 0.75),
            "p90": pct(values, 0.9), "max": max(values) if values else None}


def share(numerator: int, denominator: int) -> float | None:
    return round(numerator / denominator, 3) if denominator else None


def hours_block(values: list[float]) -> dict[str, Any]:
    return {"n": len(values), "median": med(values), "p90": pct(values, 0.9), "total": round(sum(values), 2)}


# ─── weeks and file helpers ──────────────────────────────────────────────────


def week_window(week: str) -> tuple[date, date]:
    match = re.fullmatch(r"(\d{4})-W(\d{2})", week or "")
    if not match:
        raise Refusal(f"week must look like 2026-W37, got {week!r}")
    try:
        monday = date.fromisocalendar(int(match[1]), int(match[2]), 1)
    except ValueError:
        raise Refusal(f"not an ISO week: {week}") from None
    return monday, monday + timedelta(days=6)


def week_of(day: date) -> str:
    year, number, _ = day.isocalendar()
    return f"{year}-W{number:02d}"


def window_end(until: str) -> datetime:
    """The instant the window's last day ends (exclusive)."""
    return datetime.combine(parse_date(until, "until") + timedelta(days=1), datetime.min.time(), tzinfo=UTC)


def age_days(fetched_at: datetime, until: str) -> float:
    return (fetched_at - window_end(until)).total_seconds() / 86400


def age_class_of(days: float) -> str:
    return "settled" if days >= SETTLED_AGE_DAYS else "early"


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    with path.open("r", encoding="utf-8") as file:
        return [json.loads(line) for line in file if line.strip()]


def locked_append(path: Path, decide: Callable[[list[dict[str, Any]]], dict[str, Any] | None]) -> bool:
    """Append decide(existing) under an exclusive lock on the data directory.

    The directory is a stable inode, so existence, validation and the append are one critical section. The file
    is opened only after decide accepted: a Refusal or a no-op never creates, truncates or removes it.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    lock = os.open(path.parent, os.O_RDONLY)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX)
        record = decide(read_jsonl(path))
        if record is None:
            return False
        with path.open("a", encoding="utf-8") as file:
            file.write(json.dumps(record, sort_keys=True) + "\n")
            file.flush()
            os.fsync(file.fileno())
        return True
    finally:
        os.close(lock)


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def get_path(record: Any, dotted: str) -> Any:
    for part in dotted.split("."):
        if not isinstance(record, dict) or part not in record:
            return None
        record = record[part]
    return record


def leaf_paths(value: Any, prefix: str = "") -> list[str]:
    if isinstance(value, dict) and value:
        return [path for key, child in value.items() for path in leaf_paths(child, f"{prefix}.{key}" if prefix else key)]
    return [prefix]


def registry_prefix(metric: str) -> str | None:
    matches = [path for path in REGISTRY if metric == path or metric.startswith(path + ".")]
    return max(matches, key=len) if matches else None


def definitions() -> dict[str, int]:
    return {path: entry[0] for path, entry in REGISTRY.items()}


def definitions_digest(defs: dict[str, int]) -> str:
    return hashlib.sha256(json.dumps(sorted(defs.items())).encode()).hexdigest()


# ─── GitHub fetch (read-only GraphQL) ────────────────────────────────────────

TIMELINE_TYPES = (
    "HEAD_REF_FORCE_PUSHED_EVENT,PULL_REQUEST_COMMIT,MERGED_EVENT,CLOSED_EVENT,REOPENED_EVENT,"
    "REVIEW_DISMISSED_EVENT,ASSIGNED_EVENT,UNASSIGNED_EVENT"
)
LOGIN = "... on User{login} ... on Bot{login} ... on Mannequin{login} ... on Organization{login}"
TIMELINE_FIELDS = (
    "__typename ... on HeadRefForcePushedEvent{createdAt afterCommit{oid}} "
    "... on PullRequestCommit{commit{oid committedDate parents{totalCount} messageHeadline}} "
    "... on MergedEvent{createdAt} ... on ClosedEvent{createdAt} ... on ReopenedEvent{createdAt} "
    "... on ReviewDismissedEvent{createdAt previousReviewState review{databaseId}} "
    "... on AssignedEvent{createdAt assignee{" + LOGIN + "}} "
    "... on UnassignedEvent{createdAt assignee{" + LOGIN + "}}"
)
REVIEW_FIELDS = "databaseId author{login} state submittedAt commit{oid} body comments{totalCount}"
COMMENT_FIELDS = "author{login} createdAt"
PAGE = "pageInfo{hasNextPage endCursor}"
PR_FIELDS = (
    "number author{login __typename} createdAt mergedAt closedAt state isDraft additions deletions "
    "changedFiles body headRefOid commits{totalCount} "
    f"reviews(first:100){{totalCount {PAGE} nodes{{{REVIEW_FIELDS}}}}} "
    f"comments(first:100){{totalCount {PAGE} nodes{{{COMMENT_FIELDS}}}}} "
    f"timelineItems(first:100,itemTypes:[{TIMELINE_TYPES}]){{{PAGE} nodes{{{TIMELINE_FIELDS}}}}}"
)
Q_SEARCH = (
    "query Search($q:String!,$c:String){search(query:$q,type:ISSUE,first:20,after:$c)"
    f"{{issueCount {PAGE} nodes{{... on PullRequest{{{PR_FIELDS}}}}}}}}}"
)
Q_COUNT = "query Count($q:String!){search(query:$q,type:ISSUE,first:1){issueCount}}"
Q_HEAD = "query Head($owner:String!,$name:String!){repository(owner:$owner,name:$name){defaultBranchRef{target{oid}}}}"
Q_CONNECTION = (
    "query %s($owner:String!,$name:String!,$n:Int!,$c:String){repository(owner:$owner,name:$name)"
    "{pullRequest(number:$n){%s(first:100,after:$c%s){" + PAGE + " nodes{%s}}}}}"
)
FOLLOW_UPS = (
    ("reviews", "Reviews", "", REVIEW_FIELDS),
    ("comments", "Comments", "", COMMENT_FIELDS),
    ("timelineItems", "Timeline", f",itemTypes:[{TIMELINE_TYPES}]", TIMELINE_FIELDS),
)


class Github:
    """Read-only GraphQL through pr_review.run_json; counts calls and refuses incomplete answers."""

    def __init__(self, repo: str) -> None:
        self.owner, _, self.name = repo.partition("/")
        self.calls = 0

    def query(self, text: str, **variables: Any) -> dict[str, Any]:
        argv = ["gh", "api", "graphql", "-f", "query=" + text]
        for key, value in variables.items():
            argv += ["-F", f"{key}={value}"] if isinstance(value, int) else ["-f", f"{key}={value}"]
        self.calls += 1
        try:
            result = pr_review.run_json(argv)
        except subprocess.CalledProcessError as exc:
            raise Refusal(f"GitHub query failed after retries: {(exc.stderr or '').strip()[:200]}") from exc
        if not isinstance(result, dict) or result.get("errors") or not isinstance(result.get("data"), dict):
            raise Refusal("GitHub GraphQL returned errors or no data")
        return result["data"]

    def repo_query(self, text: str, **variables: Any) -> dict[str, Any]:
        return self.query(text, owner=self.owner, name=self.name, **variables)["repository"]

    def search(self, repo: str, qualifier: str) -> dict[int, dict[str, Any]]:
        """Every PR matching the query; refuses when the count differs from issueCount."""
        query = f"repo:{repo} is:pr created:{qualifier}"
        found: dict[int, dict[str, Any]] = {}
        cursor = None
        while True:
            variables = {"q": query, **({"c": cursor} if cursor else {})}
            page = self.query(Q_SEARCH, **variables)["search"]
            for node in page["nodes"]:
                if not isinstance(node, dict):
                    raise Refusal(f"search {qualifier} returned a null node")
                found[node["number"]] = node
            if not page["pageInfo"]["hasNextPage"]:
                break
            if page["pageInfo"]["endCursor"] in (None, cursor):
                raise Refusal(f"search {qualifier} pagination did not advance")
            cursor = page["pageInfo"]["endCursor"]
        if len(found) != page["issueCount"]:
            raise Refusal(f"search {qualifier} returned {len(found)} PRs for issueCount {page['issueCount']}")
        return found

    def drain(self, node: dict[str, Any], field: str, op: str, extra: str, fields: str) -> bool:
        """Follow hasNextPage on one connection of one PR; True when pages were added."""
        connection = node[field]
        repaired = False
        while connection["pageInfo"]["hasNextPage"]:
            cursor = connection["pageInfo"]["endCursor"]
            if not cursor:
                raise Refusal(f"#{node['number']} {field}: no cursor to continue from")
            page = self.repo_query(Q_CONNECTION % (op, field, extra, fields), n=node["number"], c=cursor)["pullRequest"][field]
            if page["pageInfo"]["hasNextPage"] and page["pageInfo"]["endCursor"] == cursor:
                raise Refusal(f"#{node['number']} {field}: pagination did not advance")
            connection["nodes"] += page["nodes"]
            connection["pageInfo"] = page["pageInfo"]
            repaired = True
        return repaired

    def fetch_cohort(self, repo: str, since: str, until: str) -> tuple[list[dict[str, Any]], list[int]]:
        window = f"{since}..{until}"
        if self.query(Q_COUNT, q=f"repo:{repo} is:pr created:{window}")["search"]["issueCount"] <= 1000:
            nodes = self.search(repo, window)
        else:
            nodes = {}
            day = parse_date(since, "since")
            while day <= parse_date(until, "until"):
                nodes |= self.search(repo, f"{day}..{day}")
                day += timedelta(days=1)
        repaired = []
        for number, node in nodes.items():
            touched = [self.drain(node, *follow) for follow in FOLLOW_UPS]
            if touched[0] or touched[1]:
                repaired.append(number)
            for field in ("reviews", "comments"):
                if not pr_review.graphql_connection_complete(node[field]):
                    raise Refusal(f"#{number} {field} are incomplete after pagination")
        return list(nodes.values()), repaired


# ─── population and per-PR analysis ──────────────────────────────────────────


def author_of(pr: dict[str, Any]) -> str:
    return (pr["author"] or {}).get("login", "ghost")


def population(prs: list[dict[str, Any]], acting: str) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """(all non-bot PRs, external PRs)."""
    kept = [p for p in prs if author_of(p) != "dependabot" and (p["author"] or {}).get("__typename") != "Bot"]
    return kept, [p for p in kept if author_of(p) != acting]


def nfind(body: str | None) -> int:
    match = re.search(r"Actionable comments posted:\s*(\d+)", body or "")
    return int(match.group(1)) if match else (1 if "Some comments are outside" in (body or "") else 0)


def ismain(headline: str) -> bool:
    return bool(re.match(r"Merge (branch|remote-tracking branch) '?(origin/|upstream/)?main'?", headline)) or (
        "into main" in headline and headline.startswith("Merge")
    )


def effective_rounds(pr: dict[str, Any], acting: str) -> dict[str, Any]:
    """CR-state reviews by the maintainer, deduplicated per commit oid (rounds_v2)."""
    previous = {
        n["review"]["databaseId"]: n["previousReviewState"]
        for n in pr["timelineItems"]["nodes"]
        if n["__typename"] == "ReviewDismissedEvent" and n.get("review")
    }
    out = {"heads": collections.OrderedDict(), "supplements": 0, "approvals_excluded": 0, "state_unknown": 0, "null_commit": 0}
    reviews = sorted(
        (r for r in pr["reviews"]["nodes"] if r["author"] and r["submittedAt"] and r["author"]["login"] == acting),
        key=lambda r: parse_time(r["submittedAt"]),
    )
    for review in reviews:
        state = review["state"]
        if state == "DISMISSED":
            state = previous.get(review.get("databaseId"))
            if state is None:
                out["state_unknown"] += 1
                state = "CHANGES_REQUESTED"
            elif state == "APPROVED":
                out["approvals_excluded"] += 1
        if state != "CHANGES_REQUESTED":
            continue
        oid = (review["commit"] or {}).get("oid")
        if not oid:
            out["null_commit"] += 1
            continue
        if oid in out["heads"]:
            out["supplements"] += 1
        out["heads"].setdefault(oid, []).append(review)
    return out


def analyze_pr(pr: dict[str, Any], fetched_at: datetime, acting: str, policy: Any) -> dict[str, Any]:
    author = author_of(pr)
    created, merged = parse_time(pr["createdAt"]), parse_time(pr["mergedAt"])
    reviews = sorted(
        (dict(r, t=parse_time(r["submittedAt"])) for r in pr["reviews"]["nodes"] if r["author"] and r["submittedAt"]),
        key=lambda r: r["t"],
    )
    cr = [r for r in reviews if r["author"]["login"] == CODERABBIT]
    mw = [r for r in reviews if r["author"]["login"] == acting]
    bot_reviews = sorted(cr + mw, key=lambda r: r["t"])
    first = bot_reviews[0]["t"] if bot_reviews else None
    f: dict[str, Any] = {"number": pr["number"], "author": author, "size": pr["additions"] + pr["deletions"]}
    f["outcome"] = "merged" if merged else ("closed_unmerged" if pr["state"] == "CLOSED" else "open")
    f["rounds_mw"] = sum(r["state"] in ("CHANGES_REQUESTED", "DISMISSED") for r in mw)
    f["rounds_cr"] = sum(nfind(r["body"]) > 0 for r in cr)
    f["rounds_all"] = f["rounds_mw"] + f["rounds_cr"]
    f["first_skill_state"] = mw[0]["state"] if mw else None
    f["has_bot_review"] = bool(bot_reviews)

    commits, force_pushes = [], []
    for node in pr["timelineItems"]["nodes"]:
        if node["__typename"] == "HeadRefForcePushedEvent":
            force_pushes.append(parse_time(node["createdAt"]))
        elif node["__typename"] == "PullRequestCommit":
            c = node["commit"]
            commits.append((parse_time(c["committedDate"]), c["parents"]["totalCount"], c["messageHeadline"], c["oid"]))
    main_merges = [(d, oid) for d, parents, headline, oid in commits if parents > 1 and ismain(headline)]
    f["main_merges"] = len(main_merges)
    pushes = sum(1 for t in force_pushes if first and t > first)
    bursts, last = 0, None
    for d in sorted(d for d, *_ in commits if first and d > first):
        if last is None or (d - last).total_seconds() > 600:
            bursts += 1
        last = d
    f["pushes_after_first"] = bursts + pushes
    f["conflict_episodes"] = len(main_merges) + len(force_pushes)

    issue_comments = collections.Counter((c["author"] or {}).get("login", "ghost") for c in pr["comments"]["nodes"])
    inline = collections.Counter()
    review_counts = collections.Counter(r["author"]["login"] for r in reviews)
    for r in reviews:
        inline[r["author"]["login"]] += r["comments"]["totalCount"]
    items = {login: review_counts[login] + inline[login] + issue_comments[login] for login in set(review_counts) | set(issue_comments)}
    f["cr_reviews"], f["cr_inline"] = review_counts[CODERABBIT], inline[CODERABBIT]
    f["cr_load"] = items.get(CODERABBIT, 0)
    f["skill_load"] = items.get(acting, 0)
    f["bot_items"] = sum(n for login, n in items.items() if login == acting or login in BOTS)
    f["maintainer_posts"] = review_counts[acting] + issue_comments[acting]
    f["h_open_to_first"] = round((first - created).total_seconds() / 3600, 2) if first else None
    f["h_first_to_merge"] = round((merged - first).total_seconds() / 3600, 2) if first and merged else None
    activity = [d for d, parents, *_ in commits if parents == 1]
    activity += [parse_time(c["createdAt"]) for c in pr["comments"]["nodes"] if (c["author"] or {}).get("login") == author]
    activity += [r["t"] for r in reviews if r["author"]["login"] == author]
    f["stale_days"] = round((fetched_at - (max(activity) if activity else created)).total_seconds() / 86400, 1)
    f["abandon"] = f["rounds_all"] >= 2 and (
        f["outcome"] == "closed_unmerged" or (f["outcome"] == "open" and f["stale_days"] > 7)
    )
    profile = pr_review.artifact_profile(
        {"body": pr["body"], "headRefOid": pr["headRefOid"], "createdAt": pr["createdAt"]}, policy
    )
    if profile["gate_a_head"] and profile["final_review_head"]:
        f["level"] = "full"
    elif len(profile["missing_relevant_h2_headings"]) < len(pr_review.REQUIRED_ARTIFACT_H2_HEADINGS):
        f["level"] = "template_only"
    else:
        f["level"] = "none"

    rounds = effective_rounds(pr, acting)
    main_heads = {oid for _, oid in main_merges}
    f["v2_heads"] = [
        {"round": i, "oid": oid, "review_ids": [r.get("databaseId") for r in rs], "submittedAt": rs[0]["submittedAt"],
         "main_merge": oid in main_heads}
        for i, (oid, rs) in enumerate(rounds["heads"].items(), start=1)
    ]
    f["rounds_v2"] = len(rounds["heads"])
    f["v2_counters"] = {k: rounds[k] for k in ("supplements", "approvals_excluded", "state_unknown", "null_commit")}
    return f


def metrics_from_prs(
    all_prs: list[dict[str, Any]], external: list[dict[str, Any]], fetched_at: datetime, acting: str, open_counts: dict[str, int]
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
    analyzed = {p["number"]: analyze_pr(p, fetched_at, acting, policy) for p in all_prs}
    E = [analyzed[p["number"]] for p in external]
    merged = [r for r in E if r["outcome"] == "merged"]
    reviewed = [r for r in E if r["has_bot_review"]]
    first_states = [r["first_skill_state"] for r in E if r["first_skill_state"]]
    out: dict[str, Any] = {
        "counts": {
            "prs_opened": len(E), "prs_merged": len(merged),
            "prs_closed_unmerged": sum(r["outcome"] == "closed_unmerged" for r in E),
            "prs_open": sum(r["outcome"] == "open" for r in E),
            "maintainer_authored_excluded": len(all_prs) - len(E),
        },
        "skill_rounds": dist([r["rounds_mw"] for r in E]),
        "rounds_all": dist([r["rounds_all"] for r in E]),
        "share_first_review_approved": share(sum(s == "APPROVED" for s in first_states), len(first_states)),
        "share_ge4_rounds": share(sum(r["rounds_mw"] >= 4 for r in E), len(E)),
        "share_zero_or_one_round_merged": share(sum(r["rounds_mw"] <= 1 for r in merged), len(merged)),
        "rounds_by_size_bucket": {
            name: {"n": len(x), "mean_rounds": mean(x)}
            for name, lo, hi in SIZE_BUCKETS
            for x in [[r["rounds_mw"] for r in E if lo <= r["size"] < hi]]
        },
    }
    out["coderabbit"] = {
        "reviews": sum(r["cr_reviews"] for r in E), "inline_comments": sum(r["cr_inline"] for r in E),
        "reviews_per_pr_mean": mean([r["cr_reviews"] for r in E]),
        "inline_per_pr_mean": mean([r["cr_inline"] for r in E]),
        "share_of_bot_items": share(sum(r["cr_load"] for r in E), sum(r["bot_items"] for r in E)),
    }
    out["notification_load_per_pr"] = {
        "skill": {"median": med([r["skill_load"] for r in E]), "p90": pct([r["skill_load"] for r in E], 0.9)},
        "coderabbit": {"median": med([r["cr_load"] for r in E]), "p90": pct([r["cr_load"] for r in E], 0.9)},
    }
    out["conflict_episodes_per_pr_mean"] = mean([r["conflict_episodes"] for r in E])
    out["main_merges_per_pr_mean"] = mean([r["main_merges"] for r in E])
    out["pushes_after_first_review_median"] = med([r["pushes_after_first"] for r in reviewed])
    out["time_open_to_first_review_h_median"] = med([r["h_open_to_first"] for r in reviewed])
    first_to_merge = [r["h_first_to_merge"] for r in reviewed if r["h_first_to_merge"] is not None]
    out["time_first_review_to_merge_h"] = {"median": med(first_to_merge), "p90": pct(first_to_merge, 0.9)}
    abandoned = [r for r in E if r["abandon"]]
    out["abandonment"] = {
        "count": len(abandoned), "closed_unmerged": sum(r["outcome"] == "closed_unmerged" for r in abandoned),
        "open_stale_gt7d": sum(r["outcome"] == "open" for r in abandoned), "prs": [r["number"] for r in abandoned],
    }
    out["per_author_top10"] = [
        {"author": author, "n": n, "median_rounds": med([r["rounds_mw"] for r in E if r["author"] == author]),
         "open_prs_now": open_counts.get(author)}
        for author, n in collections.Counter(r["author"] for r in E).most_common(10)
    ]
    open_prs = [r for r in E if r["outcome"] == "open"]
    out["right_censored"] = {"open_prs_in_window": len(open_prs), "mean_rounds_so_far": mean([r["rounds_mw"] for r in open_prs])}

    def cell(rows: list[dict[str, Any]]) -> dict[str, Any]:
        states = [r["first_skill_state"] for r in rows if r["first_skill_state"]]
        return {"n": len(rows), "share_first_review_approved": share(sum(s == "APPROVED" for s in states), len(states)),
                "mean_rounds": mean([r["rounds_mw"] for r in rows]), "mean_rounds_v2": mean([r["rounds_v2"] for r in rows])}

    out["pipeline_level"] = {}
    for level in ("none", "template_only", "full"):
        rows = [r for r in E if r["level"] == level]
        out["pipeline_level"][level] = cell(rows) | {
            "by_size_bucket": {name: cell([r for r in rows if lo <= r["size"] < hi]) for name, lo, hi in SIZE_BUCKETS}
        }
    heads = [h for r in E for h in r["v2_heads"]]
    v2 = [r["rounds_v2"] for r in E]
    merged_v2 = [r["rounds_v2"] for r in merged]
    counters = {k: sum(r["v2_counters"][k] for r in E) for k in ("supplements", "approvals_excluded", "state_unknown", "null_commit")}
    out["rounds_v2"] = {
        "per_pr": dist(v2),
        "share_ge4": share(sum(n >= 4 for n in v2), len(E)),
        "share_zero_or_one_round_merged": share(sum(n <= 1 for n in merged_v2), len(merged_v2)),
        "by_size_bucket": {
            name: {"n": len(x), "mean_rounds": mean(x)}
            for name, lo, hi in SIZE_BUCKETS
            for x in [[r["rounds_v2"] for r in E if lo <= r["size"] < hi]]
        },
        "right_censored_mean": mean([r["rounds_v2"] for r in open_prs]),
        "same_head_supplements": counters["supplements"],
        "dismissed_approvals_excluded": counters["approvals_excluded"],
        "dismissed_state_unknown": counters["state_unknown"],
        "null_commit_reviews": counters["null_commit"],
        "main_merge_head_share": share(sum(h["main_merge"] for h in heads), len(heads)),
    }
    posts = [r["maintainer_posts"] for r in E]
    out["maintainer_posts_per_pr"] = {"median": med(posts), "mean": mean(posts), "p90": pct(posts, 0.9)}
    return out, E


# ─── log-derived fields ──────────────────────────────────────────────────────


def event_state(event: dict[str, Any]) -> str | None:
    """The state a material event puts the PR in; None for events that do not change state."""
    if event["event_type"] in pr_review.NON_MATERIAL_EVENT_TYPES:
        return None
    if pr_review.is_block_event(event):
        return "blocked"
    outcome = pr_review.canonical_outcome(event)
    if outcome.reason == "negative_review":  # e.g. `request_changes`, which is_block_event does not list
        return "blocked"
    if outcome.state in {"merged", "closed"} or outcome.reason == "pruned":
        return "terminal"
    if outcome.reason == "requested_changes_warning":
        return "warned"
    if outcome.state in pr_review.HOLD_STATES:
        return "held"
    if outcome.state == "deferred":
        return "deferred"
    if outcome.state == "accepted" and outcome.reason in QUEUED_REASONS:
        return "queued"
    if outcome.reason == "approved":
        return "approved"
    return None


def hours_between(start: datetime, end: datetime) -> float:
    return (end - start).total_seconds() / 3600


def spells_for(timed: list[tuple[datetime, dict[str, Any]]], close: datetime | None, fetched_at: datetime) -> list[dict[str, Any]]:
    """Maximal runs of one state on one head, ended by the next state/head, the PR's close, or fetched_at.

    Transitions after the close are ignored: a closed PR accrues no further time.
    """
    spells: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    ended_by_head: dict[str, Any] | None = None  # closed by a state-neutral event; its next state is still unknown
    for t, event in timed:
        if close and t > close:
            break
        state = event_state(event)
        head = event.get("head_sha")
        if state is None and not (current and head and head != current["head"]):
            continue
        if state is not None and ended_by_head:
            ended_by_head["next"], ended_by_head = state, None
        if current and (state != current["state"] or head != current["head"]):
            current["end"], current["next"] = t, state
            spells.append(current)
            ended_by_head, current = (current if state is None else None), None
        if current is None and state not in (None, "terminal"):
            current = {"state": state, "head": event.get("head_sha"), "start": t, "next": None}
    if current:
        current["censored"] = close is None
        current["end"] = close or fetched_at
        spells.append(current)
    for spell in spells:
        spell.setdefault("censored", False)
        spell["hours"] = hours_between(spell["start"], spell["end"])
    return spells


def longest_gap_hours(spell: dict[str, Any], times: list[datetime]) -> float:
    marks = [spell["start"]] + [t for t in times if spell["start"] < t < spell["end"]] + [spell["end"]]
    return max(hours_between(a, b) for a, b in zip(marks, marks[1:]))


def foreign_intervals(pr: dict[str, Any], acting: str, end: datetime) -> list[tuple[str, datetime, datetime]]:
    """Foreign assignee intervals clipped to the PR's open interval [createdAt, end]."""
    opened: dict[str, datetime] = {}
    intervals = []
    nodes = [n for n in pr["timelineItems"]["nodes"] if n["__typename"] in ("AssignedEvent", "UnassignedEvent") and n.get("assignee")]
    for node in sorted(nodes, key=lambda n: parse_time(n["createdAt"])):
        login, t = pr_review.fold_login(node["assignee"]["login"]), parse_time(node["createdAt"])
        if node["__typename"] == "AssignedEvent":
            opened.setdefault(login, t)
        elif login in opened:
            intervals.append((login, opened.pop(login), t))
    intervals += [(login, start, end) for login, start in opened.items()]
    created = parse_time(pr["createdAt"])
    clipped = [(login, max(start, created), min(stop, end)) for login, start, stop in intervals]
    return [i for i in clipped if i[0] != pr_review.fold_login(acting) and i[1] < i[2]]


def head_arrivals(pr: dict[str, Any]) -> dict[str, datetime]:
    """A head's arrival: its force-push time, else the committedDate of its first PullRequestCommit.

    GitHub records no push time for a normal push, and a commit can be pushed long
    after it was made, so latencies from these arrivals are upper bounds.
    """
    arrivals: dict[str, datetime] = {}
    for node in pr["timelineItems"]["nodes"]:
        if node["__typename"] == "HeadRefForcePushedEvent" and node.get("afterCommit"):
            arrivals[node["afterCommit"]["oid"]] = parse_time(node["createdAt"])
        elif node["__typename"] == "PullRequestCommit":
            arrivals.setdefault(node["commit"]["oid"], parse_time(node["commit"]["committedDate"]))
    return arrivals


def log_fields(snapshot: dict[str, Any], external: list[dict[str, Any]], fetched_at: datetime, acting: str) -> dict[str, Any]:
    by_pr: dict[int, list[tuple[datetime, dict[str, Any]]]] = collections.defaultdict(list)
    fixups = 0
    for event in snapshot["log"]["events"]:
        if event.get("event_type") == "review_correction":
            continue
        fixups += event.get("event_type") == "fixup_push"
        by_pr[int(event["pr"])].append((parse_time(event["timestamp"]), event))

    held, warned, exits, silent = [], [], [], []
    gate = collections.Counter()
    latencies, without_arrival, negative, pickup_heads = [], 0, 0, 0
    for pr in external:
        timed = by_pr.get(pr["number"], [])
        close = parse_time(pr["closedAt"] or pr["mergedAt"])
        merged = parse_time(pr["mergedAt"])
        times = [t for t, _ in timed]
        spells = spells_for(timed, close, fetched_at)
        held += [s for s in spells if s["state"] == "held"]
        warned += [s for s in spells if s["state"] == "warned"]
        silent += [s for s in spells if s["state"] == "held" and longest_gap_hours(s, times) > SPELL_LONG_HOURS]
        queued_times = [t for t, e in timed if event_state(e) == "queued" and not (close and t > close)]
        for spell in spells:
            if spell["state"] == "queued" and spell["next"] not in (None, "terminal", "queued"):
                returns = [t for t in queued_times if t > spell["end"]] + ([merged] if merged and merged >= spell["end"] else [])
                exits.append({"exit": spell["end"], "returned": min(returns) if returns else None})
        # assignee gate
        intervals = foreign_intervals(pr, acting, close or fetched_at)
        if intervals:
            gate["prs"] += 1
            author = pr_review.fold_login(author_of(pr))
            gate["author_assigned_prs"] += any(login == author for login, _, _ in intervals)
            for t, event in timed:
                inside = [login for login, start, end in intervals if start <= t < end]
                if not inside:
                    continue
                if event["event_type"] == "observation":
                    gate["skip_events"] += 1
                    gate["author_assigned_skip_events"] += author in inside
                elif event["event_type"] not in pr_review.NON_MATERIAL_EVENT_TYPES:
                    gate["material_events_in_foreign"] += 1
        # head pickup
        arrivals = head_arrivals(pr)
        first_seen: dict[str, datetime] = {}
        for t, event in timed:
            if event.get("head_sha"):
                first_seen.setdefault(event["head_sha"], t)
        known = sorted((h for h in first_seen if h in arrivals), key=lambda h: arrivals[h])
        without_arrival += len(first_seen) - len(known)
        # The earliest logged head is taken as the one the PR opened with, unless a force-push
        # after opening put it there; commit dates cannot tell an opening head from a later push.
        opened = parse_time(pr["createdAt"])
        pushed_after_open = {
            node["afterCommit"]["oid"] for node in pr["timelineItems"]["nodes"]
            if node["__typename"] == "HeadRefForcePushedEvent" and node.get("afterCommit")
            and parse_time(node["createdAt"]) > opened
        }
        for head in known[0 if known and known[0] in pushed_after_open else 1:]:
            pickup_heads += 1
            latency = hours_between(arrivals[head], first_seen[head])
            if latency < 0:
                negative += 1
            else:
                latencies.append(latency)

    returned = [hours_between(e["exit"], e["returned"]) for e in exits if e["returned"]]
    unreturned = [hours_between(e["exit"], fetched_at) for e in exits if not e["returned"]]
    last = max((t for ts in by_pr.values() for t, _ in ts), default=None)
    return {
        "coverage": {
            "cohort_prs": len(external), "prs_with_events": sum(1 for p in external if p["number"] in by_pr),
            "events_used": len(snapshot["log"]["events"]),
            "untimed_events_dropped": snapshot["log"]["coverage"]["untimed_events_dropped"],
            "last_event_timestamp": iso(last) if last else None,
        },
        "held": {
            "spells": len(held), "censored": sum(s["censored"] for s in held),
            "hours": hours_block([s["hours"] for s in held]),
            "over_72h_share": share(sum(s["hours"] > SPELL_LONG_HOURS for s in held), len(held)),
            "silence_over_72h_share": share(len(silent), len(held)),
        },
        "warned": {"spells": len(warned), "censored": sum(s["censored"] for s in warned),
                   "hours": hours_block([s["hours"] for s in warned])},
        "queue_exit": {
            "exits": len(exits), "returned": len(returned), "return_hours": hours_block(returned),
            "returned_over_72h": sum(h > SPELL_LONG_HOURS for h in returned),
            "unreturned": len(unreturned), "unreturned_over_72h": sum(h > SPELL_LONG_HOURS for h in unreturned),
        },
        "assignee_gate": {k: gate[k] for k in (
            "prs", "author_assigned_prs", "skip_events", "author_assigned_skip_events", "material_events_in_foreign")},
        "head_pickup": {
            "heads": pickup_heads, "latency_hours": hours_block(latencies),
            "over_24h": sum(h > PICKUP_LATE_HOURS[0] for h in latencies),
            "over_72h": sum(h > PICKUP_LATE_HOURS[1] for h in latencies),
            "negative_dropped": negative, "without_arrival": without_arrival,
        },
        "maintainer_fixup_pushes": fixups,
    }


# ─── snapshots and records ───────────────────────────────────────────────────

def select_events(state_dir: Path, cohort: set[int], fetched_at: datetime) -> dict[str, Any]:
    """Cohort events exactly as pr_review.all_events returns them, so a later definition can read any field."""
    if not (state_dir / "review-events.jsonl").exists():
        raise Refusal(f"event log not found under {state_dir}")
    events = pr_review.all_events(state_dir)
    if not events:
        raise Refusal("event log is empty")
    selected, untimed = [], 0
    for event in events:
        try:
            pr = int(event.get("pr"))
        except (TypeError, ValueError):
            continue
        if pr not in cohort:
            continue
        t = parse_time(event.get("timestamp"))
        if t is None:
            untimed += 1
        elif t <= fetched_at:
            selected.append(event)
    return {"events": selected, "coverage": {"events_total_read": len(events), "untimed_events_dropped": untimed}}


def build_snapshot(repo: str, state_dir: Path, acting: str | None, since: str, until: str, week: str | None) -> dict[str, Any]:
    github = Github(repo)
    if acting is None:
        acting = pr_review.gh_user()
        github.calls += 1
    head = github.repo_query(Q_HEAD)["defaultBranchRef"]["target"]["oid"]
    prs, repaired = github.fetch_cohort(repo, since, until)
    _, external = population(prs, acting)
    open_counts = {}
    for author, _ in collections.Counter(author_of(p) for p in external).most_common(10):
        open_counts[author] = github.query(Q_COUNT, q=f"repo:{repo} is:pr is:open author:{author}")["search"]["issueCount"]
    fetched_at = utcnow()
    log = select_events(state_dir, {p["number"] for p in external}, fetched_at)
    return {
        "kind": "pr_flow_snapshot", "schema_version": SCHEMA_VERSION, "repo": repo, "week": week,
        "window": {"since": since, "until": until}, "fetched_at": iso(fetched_at), "acting_login": acting,
        "github": {"prs": prs, "main_head_sha": head, "open_pr_counts": open_counts,
                   "graphql_calls": github.calls, "truncation_repaired_prs": repaired},
        "log": log, "complete": True,
    }


def snapshot_name(snapshot: dict[str, Any]) -> str:
    fetched = parse_time(snapshot["fetched_at"])
    age = age_class_of(age_days(fetched, snapshot["window"]["until"]))
    return f"snapshots/{snapshot['week']}-{age}-{fetched.strftime('%Y%m%dT%H%M%SZ')}.json.gz"


def write_snapshot(data_dir: Path, snapshot: dict[str, Any]) -> tuple[str, str]:
    """Write exclusively (never overwrite); returns (relative path, sha256)."""
    relative = snapshot_name(snapshot)
    target = data_dir / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    payload = gzip.compress(json.dumps(snapshot, sort_keys=True).encode(), mtime=0)
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as tmp:
        tmp.write(payload)
        tmp.flush()
        os.fsync(tmp.fileno())
    try:
        os.link(tmp.name, target)
    except FileExistsError:
        raise Refusal(f"snapshot {relative} already exists; refusing to overwrite") from None
    finally:
        os.unlink(tmp.name)
    return relative, hashlib.sha256(payload).hexdigest()


def read_snapshot(path: Path) -> dict[str, Any]:
    with gzip.open(path, "rt", encoding="utf-8") as file:
        return json.load(file)


def relative_to(data_dir: Path, path: Path) -> str:
    try:
        return str(path.resolve().relative_to(data_dir.resolve()))
    except ValueError:
        return str(path.resolve())


def compute_record(snapshot: dict[str, Any], snapshot_ref: dict[str, str] | None) -> dict[str, Any]:
    fetched_at = parse_time(snapshot["fetched_at"])
    acting = snapshot["acting_login"]
    github = snapshot["github"]
    all_prs, external = population(github["prs"], acting)
    metrics, _ = metrics_from_prs(all_prs, external, fetched_at, acting, github["open_pr_counts"])
    days = age_days(fetched_at, snapshot["window"]["until"])
    defs = definitions()
    return {
        "kind": "metrics", "schema_version": SCHEMA_VERSION, "week": snapshot["week"], "window": snapshot["window"],
        "fetched_at": snapshot["fetched_at"], "computed_at": iso(utcnow()),
        "age_days": round(days, 2), "age_class": age_class_of(days),
        "definitions": defs, "definitions_digest": definitions_digest(defs),
        "snapshot": snapshot_ref, "graphql_calls": github["graphql_calls"],
        "main_head_sha": github["main_head_sha"], "acting_login": acting, "caveats": list(CAVEATS),
        **metrics, "truncation_repaired_prs": github["truncation_repaired_prs"],
        "log": log_fields(snapshot, external, fetched_at, acting),
    }


def record_key(record: dict[str, Any]) -> tuple:
    if record["kind"] == "metrics":
        return ("metrics", record["week"], record["age_class"], record["definitions_digest"])
    return ("sample", record["week"], record["rubric_version"])


def append_record(data_dir: Path, record: dict[str, Any]) -> bool:
    def decide(existing: list[dict[str, Any]]) -> dict[str, Any] | None:
        return None if any(record_key(r) == record_key(record) for r in existing if r["kind"] == record["kind"]) else record

    return locked_append(data_dir / "history.jsonl", decide)


def validate_input_snapshot(snapshot: dict[str, Any], repo: str, week: str) -> None:
    if snapshot.get("kind") != "pr_flow_snapshot" or snapshot.get("schema_version") != SCHEMA_VERSION:
        raise Refusal("not a pr_flow_snapshot of this schema version")
    if snapshot.get("repo") != repo:
        raise Refusal(f"snapshot is for {snapshot.get('repo')}, not {repo}")
    since, until = week_window(week)
    if snapshot.get("week") != week or snapshot.get("window") != {"since": str(since), "until": str(until)}:
        raise Refusal(f"snapshot window is not exactly {week}")
    if snapshot.get("complete") is not True:
        raise Refusal("snapshot is not marked complete")


def current_key_exists(data_dir: Path, week: str, age_class: str) -> bool:
    digest = definitions_digest(definitions())
    return any(
        r["kind"] == "metrics" and (r["week"], r["age_class"], r["definitions_digest"]) == (week, age_class, digest)
        for r in read_jsonl(data_dir / "history.jsonl")
    )


def append_week(args: argparse.Namespace, week: str) -> None:
    since, until = week_window(week)
    if until >= utcnow().date():
        raise Refusal(f"{week} has not ended (until {until})")
    data_dir = args.data_dir
    if args.input:
        path = args.input
        snapshot = read_snapshot(path)
        validate_input_snapshot(snapshot, args.repo, week)
        ref = {"path": relative_to(data_dir, path), "sha256": sha256_file(path)}
        for record in read_jsonl(data_dir / "history.jsonl"):
            if (record.get("snapshot") or {}).get("path") == ref["path"] and record["snapshot"]["sha256"] != ref["sha256"]:
                raise Refusal(f"{ref['path']} no longer matches the hash recorded in history")
    else:
        prospective = age_class_of(age_days(utcnow(), str(until)))
        if current_key_exists(data_dir, week, prospective):
            print(f"{week} {prospective}: already recorded")
            return
        snapshot = build_snapshot(args.repo, args.state_dir, args.acting_login, str(since), str(until), week)
        path_text, digest = write_snapshot(data_dir, snapshot)
        ref = {"path": path_text, "sha256": digest}
    record = compute_record(snapshot, ref)
    if append_record(data_dir, record):
        print(f"{week} {record['age_class']}: recorded ({ref['path']})")
    else:
        print(f"{week} {record['age_class']}: already recorded")


def weeks_for(args: argparse.Namespace) -> list[str]:
    today = utcnow().date()
    if args.week:
        return [args.week]
    if args.last_week:
        return [week_of(today - timedelta(days=7))]
    history = [r for r in read_jsonl(args.data_dir / "history.jsonl") if r["kind"] == "metrics"]
    if not history:
        return []
    digest = definitions_digest(definitions())
    done = {r["week"] for r in history if r["age_class"] == "settled" and r["definitions_digest"] == digest}
    weeks, monday = [], week_window(min(r["week"] for r in history))[0]
    while age_days(utcnow(), str(monday + timedelta(days=6))) >= SETTLED_AGE_DAYS:
        if week_of(monday) not in done:
            weeks.append(week_of(monday))
        monday += timedelta(days=7)
    return weeks


# ─── review sample ───────────────────────────────────────────────────────────


def settled_snapshot(data_dir: Path, week: str) -> tuple[dict[str, Any], dict[str, str]]:
    records = [
        r for r in read_jsonl(data_dir / "history.jsonl")
        if r["kind"] == "metrics" and r["week"] == week and r["age_class"] == "settled"
    ]
    if not records:
        raise Refusal(f"no settled metrics record for {week}; run append first")
    ref = max(records, key=lambda r: (r["fetched_at"], r["computed_at"]))["snapshot"]
    path = data_dir / ref["path"]
    if sha256_file(path) != ref["sha256"]:
        raise Refusal(f"{ref['path']} does not match its recorded hash")
    return read_snapshot(path), ref


def select_sample(week: str, candidates: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """3 PRs with the most rounds (ties shuffled), then 5 random from the rest."""
    pool = sorted((c for c in candidates if c["rounds_v2"] >= 1), key=lambda c: c["number"])
    random.Random(week).shuffle(pool)
    pool.sort(key=lambda c: -c["rounds_v2"])
    rest = pool[SAMPLE_TOP:]
    return pool[:SAMPLE_TOP] + random.Random(f"{week}/random").sample(rest, min(SAMPLE_RANDOM, len(rest)))


def sample_for_week(data_dir: Path, week: str) -> tuple[list[dict[str, Any]], dict[str, str]]:
    snapshot, ref = settled_snapshot(data_dir, week)
    acting = snapshot["acting_login"]
    _, external = population(snapshot["github"]["prs"], acting)
    policy = pr_review.load_policy(pr_review.DEFAULT_POLICY)
    fetched_at = parse_time(snapshot["fetched_at"])
    analyzed = [analyze_pr(p, fetched_at, acting, policy) for p in external]
    return select_sample(week, analyzed), ref


def command_sample(args: argparse.Namespace) -> None:
    selected, ref = sample_for_week(args.data_dir, args.week)
    print(f"# sample for {args.week} from {ref['path']}")
    for pr in selected:
        print(f"PR {pr['number']} ({pr['rounds_v2']} v2 rounds, showing {min(pr['rounds_v2'], SAMPLE_ROUND_CAP)})")
        for head in pr["v2_heads"][:SAMPLE_ROUND_CAP]:
            ids = ",".join(str(i) for i in head["review_ids"])
            print(f"  round {head['round']}\t{head['oid']}\treview {ids}\t{head['submittedAt']}")


def read_tsv(path: Path, required: tuple[str, ...]) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as file:
        reader = csv.DictReader(file, delimiter="\t")
        missing = [c for c in required if c not in (reader.fieldnames or [])]
        if missing:
            raise Refusal(f"{path.name}: missing columns {missing}")
        return [{k: (v or "").strip() for k, v in row.items() if k} for row in reader]


def row_key(row: dict[str, str]) -> tuple[int, int, str]:
    try:
        return int(row["pr"]), int(row["round"]), row["fid"]
    except ValueError:
        raise Refusal(f"bad pr/round in row {row}") from None


def drift_keys(week: str, rows: list[dict[str, str]]) -> list[tuple[int, int, str]]:
    keys = sorted(row_key(r) for r in rows)
    return sorted(random.Random(f"{week}/drift").sample(keys, min(DRIFT_SIZE, len(keys))))


def validate_ratings(rows: list[dict[str, str]], printed: set[tuple[int, int]]) -> None:
    seen = set()
    for row in rows:
        key = row_key(row)
        if key in seen:
            raise Refusal(f"duplicate row key {key}")
        seen.add(key)
        if not row["finding"]:
            raise Refusal(f"{key}: finding is empty")
        if key[:2] not in printed:
            raise Refusal(f"{key[:2]} is not a printed sample round")
        for column, allowed in VOCAB.items():
            if row[column] not in allowed:
                raise Refusal(f"{key}: {column}={row[column]!r} is outside the vocabulary")
        if row["latent"] == "yes" and row["severity"] == "BLOCKER":
            raise Refusal(f"{key}: a latent finding cannot be a BLOCKER")
        if row["late"] in ("LATE", "NOT_LATE") and not row["late_evidence"]:
            raise Refusal(f"{key}: {row['late']} needs late_evidence")
        if (key[1] == 1) != (row["late"] == "-"):
            raise Refusal(f"{key}: late must be '-' exactly for round-1 rows")


def post_r1_stats(rows: list[dict[str, str]]) -> dict[str, Any]:
    post = [r for r in rows if int(r["round"]) >= 2]

    def known(column: str) -> list[dict[str, str]]:
        return [r for r in post if r[column] != "UNSURE"]

    stats: dict[str, Any] = {
        "late_share": share(sum(r["late"] == "LATE" for r in post), sum(r["late"] in ("LATE", "NOT_LATE") for r in post)),
        "fix_induced_share": share(sum(r["origin"] == "FIX_INDUCED" for r in known("origin")), len(known("origin"))),
        "blocker_share": share(sum(r["severity"] == "BLOCKER" for r in known("severity")), len(known("severity"))),
        "location_share": {
            v: share(sum(r["location"] == v for r in known("location")), len(known("location")))
            for v in sorted(VOCAB["location"] - {"UNSURE"})
        },
        "late_blocker_count": sum(r["late"] == "LATE" and r["severity"] == "BLOCKER" for r in post),
        "fix_induced_blocker_count": sum(r["origin"] == "FIX_INDUCED" and r["severity"] == "BLOCKER" for r in post),
        "nit_blocking": sum(r["severity"] == "NIT" and r["bot_treatment"] == "blocking" for r in post),
        "latent_blocking": sum(r["latent"] == "yes" and r["bot_treatment"] == "blocking" for r in post),
    }
    return stats


def cohens_kappa(pairs: list[tuple[str, str]]) -> float | None:
    if not pairs:
        return None
    n = len(pairs)
    observed = sum(a == b for a, b in pairs) / n
    first, second = collections.Counter(a for a, _ in pairs), collections.Counter(b for _, b in pairs)
    expected = sum(first[v] * second[v] for v in first) / (n * n)
    return None if expected == 1 else round((observed - expected) / (1 - expected), 3)


def drift_stats(primary: dict[tuple, dict[str, str]], redo: dict[tuple, dict[str, str]]) -> dict[str, Any]:
    out = {}
    for axis, threshold in DRIFT_THRESHOLDS.items():
        pairs = []
        for key, second in redo.items():
            first = primary[key]
            if "UNSURE" in (first[axis], second[axis]) or (axis == "late" and first[axis] == "-"):
                continue
            pairs.append((first[axis], second[axis]))
        agree = sum(a == b for a, b in pairs)
        needed = math.floor(threshold * len(pairs) / DRIFT_SIZE)
        out[axis] = {"agree": agree, "m": len(pairs), "threshold": threshold, "needed": needed,
                     "pass": bool(pairs) and agree >= needed, "kappa": cohens_kappa(pairs)}
    return out


def validate_drift_rows(rows: list[dict[str, str]]) -> None:
    for row in rows:
        key = row_key(row)
        for axis in DRIFT_THRESHOLDS:
            if row[axis] not in VOCAB[axis]:
                raise Refusal(f"drift {key}: {axis}={row[axis]!r} is outside the vocabulary")
        if (key[1] == 1) != (row["late"] == "-"):
            raise Refusal(f"drift {key}: late must be '-' exactly for round-1 rows")


def sample_fields(prs: int, rounds_sampled: int, rows: list[dict[str, str]]) -> dict[str, Any]:
    post = [r for r in rows if int(r["round"]) >= 2]
    return {
        "prs": prs, "rounds_sampled": rounds_sampled,
        "rounds_with_findings": len({row_key(r)[:2] for r in rows}), "n_findings": len(rows), "n_post_r1": len(post),
        **{f"{axis}_unsure": sum(r[axis] == "UNSURE" for r in post) for axis in DRIFT_THRESHOLDS},
        "post_r1": post_r1_stats(rows),
    }


def command_sample_drift(args: argparse.Namespace) -> None:
    rows = read_tsv(args.ratings, RATING_COLUMNS)
    by_key = {row_key(r): r for r in rows}
    print("\t".join(IDENTITY_COLUMNS))
    for key in drift_keys(args.week, rows):
        row = by_key[key]
        print("\t".join((*(str(part) for part in key), row["anchor"], row["finding"])))


def command_sample_record(args: argparse.Namespace) -> None:
    selected, ref = sample_for_week(args.data_dir, args.week)
    printed = {(pr["number"], head["round"]) for pr in selected for head in pr["v2_heads"][:SAMPLE_ROUND_CAP]}
    rows = read_tsv(args.ratings, RATING_COLUMNS)
    validate_ratings(rows, printed)
    redo_rows = read_tsv(args.drift, DRIFT_COLUMNS)
    validate_drift_rows(redo_rows)
    if sorted(row_key(r) for r in redo_rows) != drift_keys(args.week, rows):
        raise Refusal("drift rows are not exactly the sample-drift selection")
    primary = {row_key(r): r for r in rows}
    drift = drift_stats(primary, {row_key(r): r for r in redo_rows})
    snapshot, _ = settled_snapshot(args.data_dir, args.week)
    record = {
        "kind": "sample", "schema_version": SCHEMA_VERSION, "week": args.week, "age_class": "settled",
        "rubric_version": RUBRIC_VERSION, "fetched_at": snapshot["fetched_at"], "computed_at": iso(utcnow()),
        "snapshot": ref,
        "sample": sample_fields(len(selected), len(printed), rows),
        "drift": drift, "unreliable_axes": [axis for axis, d in drift.items() if not d["pass"]],
    }
    print("recorded" if append_record(args.data_dir, record) else "already recorded")
    print(json.dumps({"drift": drift, "unreliable_axes": record["unreliable_axes"]}, sort_keys=True))


# ─── change ledger ───────────────────────────────────────────────────────────


def is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


@functools.cache
def metric_paths() -> frozenset[str]:
    """Every leaf path a record can carry: the leaves of a measurement over an empty cohort and an empty sample.

    Bucket names (size buckets, pipeline levels, reviewer classes, location values) come from the code that
    emits them, so the set is exact and finite; test_emitted_paths_match_registry ties it to REGISTRY.
    """
    empty_log = {"log": {"events": [], "coverage": {"untimed_events_dropped": 0}}}
    metrics, _ = metrics_from_prs([], [], utcnow(), "", {})
    record = {**metrics, "truncation_repaired_prs": [], "log": log_fields(empty_log, [], utcnow(), "")}
    return frozenset(leaf_paths(record)) | {f"{SAMPLE_PREFIX}{p}" for p in leaf_paths(sample_fields(0, 0, []))}


def metric_known(metric: str) -> bool:
    return metric in metric_paths()


def metric_registered(metric: str, version: Any) -> bool:
    if not metric_known(metric):
        return False
    if metric.startswith(SAMPLE_PREFIX):
        return version == RUBRIC_VERSION
    return REGISTRY[registry_prefix(metric)][0] == version


def required_axes(metric: str) -> tuple[str, ...]:
    """The drift axes a sample metric reads; empty for non-sample metrics and for fields no axis rates."""
    if not metric.startswith(SAMPLE_PREFIX):
        return ()
    field = metric[len(SAMPLE_PREFIX):]
    if field.startswith("post_r1."):
        return POST_R1_AXES.get(field.split(".")[1], ())
    axis = field.removesuffix("_unsure")
    return (axis,) if axis != field and axis in DRIFT_THRESHOLDS else ()


def validate_change(change: Any, existing_ids: set[str]) -> None:
    if not isinstance(change, dict):
        raise Refusal("change must be a JSON object")
    if unknown := set(change) - CHANGE_KEYS:
        raise Refusal(f"unknown keys {sorted(unknown)}")
    if missing := (CHANGE_KEYS - {"note"}) - set(change):
        raise Refusal(f"missing keys {sorted(missing)}")
    if not re.fullmatch(r"C\d+", str(change["id"])):
        raise Refusal("id must look like C12")
    if change["id"] in existing_ids:
        raise Refusal(f"{change['id']} already exists")
    day, review_after = parse_date(change["date"], "date"), parse_date(change["review_after"], "review_after")
    if review_after < day:
        raise Refusal("review_after precedes date")
    for key in ("phase", "hypothesis", "switch"):
        if not isinstance(change[key], str) or not change[key].strip():
            raise Refusal(f"{key} must be a non-empty string")
    targets = change["targets"]
    if not isinstance(targets, list) or not targets:
        raise Refusal("targets must be a non-empty list")
    for target in targets:
        if not isinstance(target, dict) or (unknown := set(target) - TARGET_KEYS):
            raise Refusal(f"bad target {target!r}")
        if missing := {"metric", "version", "age_class", "role", "direction"} - set(target):
            raise Refusal(f"target {target.get('metric')!r} lacks {sorted(missing)}")
        if target["role"] not in ROLES or target["direction"] not in DIRECTIONS or target["age_class"] not in AGE_CLASSES:
            raise Refusal(f"target {target['metric']!r} has a value outside the vocabulary")
        for key in ("baseline", "expected"):
            if target.get(key) is not None and not is_number(target[key]):
                raise Refusal(f"target {target['metric']!r}: {key} must be a number or null")
        if target["metric"].startswith(SAMPLE_PREFIX) and target["age_class"] != "settled":
            raise Refusal(f"{target['metric']} is settled by construction")
        if target["role"] != "primary" and not metric_registered(target["metric"], target["version"]):
            if not target.get("measurable_from"):
                raise Refusal(f"unregistered {target['metric']!r} needs measurable_from")
    primaries = [t for t in targets if t["role"] == "primary"]
    if len(primaries) != 1:
        raise Refusal("exactly one primary target is required")
    primary = primaries[0]
    if not is_number(primary.get("expected")):
        raise Refusal("the primary target needs a numeric expected value")
    if not metric_registered(primary["metric"], primary["version"]):
        raise Refusal(f"primary {primary['metric']!r} version {primary['version']!r} is not in the registry")


def fold_ledger(records: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    state: dict[str, dict[str, Any]] = {}
    for record in records:
        if record["kind"] == "change":
            state[record["id"]] = {"change": record, "baselines": {}, "landed": None, "verdicts": []}
            continue
        entry = state[record["id"]]
        if record["kind"] == "baseline":
            entry["baselines"].update({(b["metric"], b["age_class"]): b["baseline"] for b in record["targets"]})
        elif record["kind"] == "landed":
            entry["landed"] = record
        else:
            entry["verdicts"].append(record)
    return state


def target_baseline(entry: dict[str, Any], target: dict[str, Any]) -> Any:
    return entry["baselines"].get((target["metric"], target["age_class"]), target.get("baseline"))


def primary_of(change: dict[str, Any]) -> dict[str, Any]:
    return next(t for t in change["targets"] if t["role"] == "primary")


def review_after_of(entry: dict[str, Any]) -> str:
    return (entry["landed"] or {}).get("review_after") or entry["change"]["review_after"]


def weekly_values(history: list[dict[str, Any]], target: dict[str, Any]) -> tuple[list[dict[str, Any]], int]:
    """One value per ISO week for the target's metric, version and age class (earliest fetch wins).

    A sample record whose unreliable axes include one the metric reads (required_axes) is ineligible.
    """
    metric, sample = target["metric"], target["metric"].startswith(SAMPLE_PREFIX)
    best: dict[str, dict[str, Any]] = {}
    for record in history:
        if record["age_class"] != target["age_class"] or record["kind"] != ("sample" if sample else "metrics"):
            continue
        if not re.fullmatch(r"\d{4}-W\d{2}", str(record.get("week"))):
            continue
        if sample:
            if record["rubric_version"] != target["version"] or set(required_axes(metric)) & set(record.get("unreliable_axes", [])):
                continue
        elif record["definitions"].get(registry_prefix(metric)) != target["version"]:
            continue
        key = (record["fetched_at"], record["computed_at"])
        if record["week"] not in best or key < best[record["week"]]["key"]:
            best[record["week"]] = {"key": key, "record": record}
    values, nulls = [], 0
    for week, item in sorted(best.items()):
        value = get_path(item["record"], metric)
        if not is_number(value):
            nulls += 1
            continue
        since, until = week_window(week)
        values.append({"week": week, "since": str(since), "until": str(until), "value": value})
    return values, nulls


def split_weeks(values: list[dict[str, Any]], landed: str) -> tuple[list[float], list[float]]:
    day = parse_date(landed, "landed date")
    return (
        [v["value"] for v in values if parse_date(v["until"], "until") < day],
        [v["value"] for v in values if parse_date(v["since"], "since") >= day],
    )


def is_bad(direction: str, side: str) -> bool:
    return side == {"decrease": "above", "increase": "below"}.get(direction) or (direction == "hold" and side != "inside")


def rule_verdict(entry: dict[str, Any], history: list[dict[str, Any]]) -> str:
    """The mechanical verdict of the ledger rule: medians of weekly values against baseline ranges, nothing pooled.

    A guard outside its range in its bad direction vetoes an improvement; a post median equal to a range edge is inside.
    """
    landed = entry["landed"]["date"]

    def position(target: dict[str, Any]) -> str | None:
        baseline, post = split_weeks(weekly_values(history, target)[0], landed)
        if len(baseline) < 2 or len(post) < 2:
            return None
        middle = statistics.median(post)
        return "above" if middle > max(baseline) else "below" if middle < min(baseline) else "inside"

    primary = primary_of(entry["change"])
    side = position(primary)
    if side is None:
        return "inconclusive"
    guards = [t for t in entry["change"]["targets"] if t["role"] == "guard"]
    if is_bad(primary["direction"], side) or any(g_side and is_bad(g["direction"], g_side) for g in guards for g_side in [position(g)]):
        return "regressed"
    return "improved" if side == {"decrease": "below", "increase": "above"}.get(primary["direction"]) else "no_change"


def ledger_add(args: argparse.Namespace) -> None:
    change = json.loads(args.file.read_text(encoding="utf-8"))

    def decide(existing: list[dict[str, Any]]) -> dict[str, Any]:
        validate_change(change, set(fold_ledger(existing)))
        return {"kind": "change", **change}

    locked_append(args.data_dir / "changes.jsonl", decide)
    print(f"added {change['id']}")


def ledger_baseline(args: argparse.Namespace) -> None:
    history = read_jsonl(args.data_dir / "history.jsonl")
    today = str(utcnow().date())

    def decide(existing: list[dict[str, Any]]) -> dict[str, Any] | None:
        entry = fold_ledger(existing).get(args.id)
        if entry is None:
            raise Refusal(f"unknown change {args.id}")
        cutoff = (entry["landed"] or {}).get("date", today)
        fills = []
        for target in entry["change"]["targets"]:
            if not metric_known(target["metric"]) or not metric_registered(target["metric"], target["version"]):
                continue
            if target_baseline(entry, target) is not None and not args.refresh:
                continue
            baseline, _ = split_weeks(weekly_values(history, target)[0], cutoff)
            if not baseline:
                skipped.append(target["metric"])
                continue
            fills.append({"metric": target["metric"], "age_class": target["age_class"],
                          "baseline": round(statistics.median(baseline), 4), "weeks": len(baseline)})
        if skipped and not fills:
            raise Refusal(f"no eligible weekly values before {cutoff} for {', '.join(skipped)}")
        if not fills:
            return None
        return {"kind": "baseline", "id": args.id, "computed_at": iso(utcnow()), "targets": fills}

    skipped: list[str] = []

    if locked_append(args.data_dir / "changes.jsonl", decide):
        print(f"baseline recorded for {args.id}" + (f"; no data yet for {', '.join(skipped)}" if skipped else ""))
    else:
        print(f"{args.id}: no null baselines to fill (use --refresh to replace)")


def ledger_landed(args: argparse.Namespace) -> None:
    def decide(existing: list[dict[str, Any]]) -> dict[str, Any]:
        entry = fold_ledger(existing).get(args.id)
        if entry is None:
            raise Refusal(f"unknown change {args.id}")
        if entry["landed"]:
            raise Refusal(f"{args.id} is already landed")
        if not re.fullmatch(r"[0-9a-f]{40}", args.commit):
            raise Refusal("commit must be a 40-hex sha")
        if parse_date(args.date, "date") < parse_date(entry["change"]["date"], "change date"):
            raise Refusal("landed date precedes the change date")
        if target_baseline(entry, primary_of(entry["change"])) is None:
            raise Refusal("the primary baseline is null; run `ledger baseline` first")
        review_after = review_after_of(entry)
        if args.review_after:
            if parse_date(args.review_after, "review_after") < parse_date(review_after, "review_after"):
                raise Refusal("review_after may only move later")
            review_after = args.review_after
        return {"kind": "landed", "id": args.id, "commit": args.commit, "date": args.date, "review_after": review_after}

    locked_append(args.data_dir / "changes.jsonl", decide)
    print(f"{args.id} landed")


def target_evidence(history: list[dict[str, Any]], target: dict[str, Any], landed_date: str) -> dict[str, Any]:
    """One target's eligible weekly values around the landing: the single computation behind `show --due` and verdicts."""
    baseline, post = split_weeks(weekly_values(history, target)[0], landed_date)
    return {
        "metric": target["metric"], "version": target["version"], "age_class": target["age_class"],
        "role": target["role"], "direction": target["direction"], "expected": target.get("expected"),
        "baseline_values": baseline, "post_values": post,
        "baseline_range": [min(baseline), max(baseline)] if baseline else None,
        "post_median": statistics.median(post) if post else None,
    }


def landed_evidence(history: list[dict[str, Any]], entry: dict[str, Any]) -> list[dict[str, Any]]:
    return [target_evidence(history, t, entry["landed"]["date"]) for t in entry["change"]["targets"] if metric_known(t["metric"])]


def ledger_verdict(args: argparse.Namespace) -> None:
    verdict = json.loads(args.file.read_text(encoding="utf-8"))
    history = read_jsonl(args.data_dir / "history.jsonl")

    def decide(existing: list[dict[str, Any]]) -> dict[str, Any]:
        if not isinstance(verdict, dict) or set(verdict) != VERDICT_KEYS:
            raise Refusal(f"verdict keys must be exactly {sorted(VERDICT_KEYS)}; evidence is computed, not supplied")
        entry = fold_ledger(existing).get(verdict["id"])
        if entry is None or not entry["landed"]:
            raise Refusal(f"{verdict['id']} is unknown or not landed")
        week_window(verdict["week"])
        if verdict["verdict"] not in VERDICTS:
            raise Refusal(f"verdict must be one of {sorted(VERDICTS)}")
        if not isinstance(verdict["note"], str):
            raise Refusal("note must be a string")
        rule = rule_verdict(entry, history)
        if verdict["verdict"] != rule and not verdict["note"].strip():
            raise Refusal(f"verdict {verdict['verdict']!r} departs from rule_verdict {rule!r}; explain it in note")
        return {"kind": "verdict", **verdict, "rule_verdict": rule, "evidence": landed_evidence(history, entry),
                "recorded_at": iso(utcnow())}

    locked_append(args.data_dir / "changes.jsonl", decide)
    print(f"verdict recorded for {verdict['id']}")


def ledger_show(args: argparse.Namespace) -> None:
    history = read_jsonl(args.data_dir / "history.jsonl")
    today = utcnow().date()
    out = []
    for change_id, entry in fold_ledger(read_jsonl(args.data_dir / "changes.jsonl")).items():
        landed = entry["landed"]
        if args.due and not (landed and parse_date(review_after_of(entry), "review_after") <= today):
            continue
        row: dict[str, Any] = {"id": change_id, "phase": entry["change"]["phase"], "landed": landed,
                               "review_after": review_after_of(entry), "verdicts": [v["verdict"] for v in entry["verdicts"]]}
        if args.due:
            row |= {"targets": landed_evidence(history, entry), "rule_verdict": rule_verdict(entry, history)}
        else:
            row["targets"] = [dict(t, baseline=target_baseline(entry, t)) for t in entry["change"]["targets"]]
        out.append(row)
    print(json.dumps(out, indent=2, sort_keys=True))


# ─── command line ────────────────────────────────────────────────────────────


def command_measure(args: argparse.Namespace) -> None:
    snapshot = build_snapshot(args.repo, args.state_dir, args.acting_login, args.since, args.until, None)
    print(json.dumps(compute_record(snapshot, None), sort_keys=True))


def command_append(args: argparse.Namespace) -> None:
    if args.input and not args.week:
        raise Refusal("--input requires --week")
    weeks = weeks_for(args)
    if not weeks:
        print("no weeks due")
    for week in weeks:
        append_week(args, week)


def build_parser() -> argparse.ArgumentParser:
    data = argparse.ArgumentParser(add_help=False)
    data.add_argument("--data-dir", type=Path, default=DEFAULT_DATA_DIR)
    live = argparse.ArgumentParser(add_help=False)
    pr_review.add_state(live)
    live.add_argument("--acting-login", default=None)
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    measure = sub.add_parser("measure", parents=[data, live], help="fetch live and print one record; writes nothing")
    measure.add_argument("--since", required=True)
    measure.add_argument("--until", required=True)
    measure.set_defaults(func=command_measure)

    append = sub.add_parser("append", parents=[data, live], help="record complete ISO weeks idempotently")
    group = append.add_mutually_exclusive_group(required=True)
    group.add_argument("--week")
    group.add_argument("--last-week", action="store_true")
    group.add_argument("--settled-due", action="store_true")
    append.add_argument("--input", type=Path, help="recompute from an existing snapshot (with --week)")
    append.set_defaults(func=command_append)

    sample = sub.add_parser("sample", parents=[data], help="print the deterministic review sample")
    sample.add_argument("--week", required=True)
    sample.set_defaults(func=command_sample)
    drift = sub.add_parser("sample-drift", parents=[data], help="print the seeded drift keys")
    drift.add_argument("--week", required=True)
    drift.add_argument("--ratings", type=Path, required=True)
    drift.set_defaults(func=command_sample_drift)
    record = sub.add_parser("sample-record", parents=[data], help="validate ratings and append the sample record")
    record.add_argument("--week", required=True)
    record.add_argument("--ratings", type=Path, required=True)
    record.add_argument("--drift", type=Path, required=True)
    record.set_defaults(func=command_sample_record)

    ledger = sub.add_parser("ledger", help="change ledger").add_subparsers(dest="ledger_command", required=True)
    add = ledger.add_parser("add", parents=[data])
    add.add_argument("file", type=Path)
    add.set_defaults(func=ledger_add)
    baseline = ledger.add_parser("baseline", parents=[data])
    baseline.add_argument("id")
    baseline.add_argument("--refresh", action="store_true")
    baseline.set_defaults(func=ledger_baseline)
    landed = ledger.add_parser("landed", parents=[data])
    landed.add_argument("id")
    landed.add_argument("--commit", required=True)
    landed.add_argument("--date", required=True)
    landed.add_argument("--review-after")
    landed.set_defaults(func=ledger_landed)
    verdict = ledger.add_parser("verdict", parents=[data])
    verdict.add_argument("file", type=Path)
    verdict.set_defaults(func=ledger_verdict)
    show = ledger.add_parser("show", parents=[data])
    show.add_argument("--due", action="store_true")
    show.set_defaults(func=ledger_show)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if hasattr(args, "state_dir"):
        pr_review.finalize_state_dir(args)
    try:
        args.func(args)
    except Refusal as exc:
        print(f"refused: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
