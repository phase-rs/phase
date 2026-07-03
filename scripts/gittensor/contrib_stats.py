#!/usr/bin/env python3
"""Gittensor contributor impact analysis for phase.rs.

Splits the repository's contributions into two cohorts — gittensor miners vs.
everyone else (core team + outside collaborators) — and computes comparative
statistics for a recurring update to the gittensor subnet owners.

Attribution model (why this is trustworthy)
--------------------------------------------
This is *adversarial* attribution: the cohort being measured is paid, and git
author name/email are attacker-controlled (a miner can commit as
`matt.evans.dev@gmail.com` and it survives squash-merge). So identity is derived
from an unspoofable source wherever possible:

  * The repo squash-merges through a merge queue, so every commit on the measured
    ref carries `(#N)` in its subject. We join `N -> gh pr list -> author.login`,
    a GitHub-authenticated identity that cannot be forged in a commit.
  * Only for rare direct pushes (no `(#N)`) do we fall back to email-based
    coalescing via the whitelist. Display-name matching is intentionally NOT a
    resolution path — it is the easiest field to spoof.

The whitelist (scripts/contributors.toml) then maps each login to a group.

Data sources
------------
* `git log --numstat` on the measured ref -> commits, code LOC, area mix, churn.
* `gh pr list` -> the N->login map (complete via the API, independent of the
  local clone's depth).

LOC is reported twice: gross (everything) and code-only (excluding generated &
data files: data/, lockfiles, *.d.ts, fixtures/*.json, node_modules) so a cohort
touching the 93 MB card database can't dwarf one writing hand-authored code.

Requires FULL history: a shallow clone's graft-boundary commit dumps the entire
tree as one author's additions, so the tool refuses to run on one.

Usage
-----
    python3 scripts/gittensor/contrib_stats.py                       # origin/main, all-time
    python3 scripts/gittensor/contrib_stats.py --since 2026-06-01    # windowed
    python3 scripts/gittensor/contrib_stats.py --ref v1.2.3 --triage # list unclassified
    python3 scripts/gittensor/contrib_stats.py --repo-dir ~/phase-analysis   # full clone
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

VALID_GROUPS = {"gittensor", "nongittensor", "bot"}

# ── Repo-area bucketing (first matching prefix wins) ─────────────────────────
# Only applied to non-generated paths, so data/ never reaches here.
AREA_RULES: list[tuple[str, str]] = [
    ("crates/engine/src/parser", "Parser"),
    ("crates/engine", "Engine"),
    ("crates/phase-ai", "AI"),
    ("crates/phase-server", "Server"),
    ("crates/server-core", "Server"),
    ("crates/lobby-broker", "Server"),
    ("crates/seat-reducer", "Server"),
    ("crates/engine-wasm", "WASM"),
    ("crates/draft-wasm", "WASM"),
    ("crates/", "Other crates"),
    ("client/src-tauri", "Desktop"),
    ("client/", "Frontend"),
    ("mtgish/", "mtgish"),
    ("scripts/", "Tooling"),
    ("docs/", "Docs"),
    (".claude/", "Agent tooling"),
    (".agents/", "Agent tooling"),
    ("deploy/", "Infra"),
    ("docker/", "Infra"),
    ("supabase/", "Infra"),
    ("lobby-worker/", "Infra"),
]


def area_of(path: str) -> str:
    for prefix, area in AREA_RULES:
        if path.startswith(prefix):
            return area
    return "Other"


# ── Generated / non-code files (excluded from the code-only metric) ──────────
_GEN_SUFFIXES = (".lock", ".d.ts", ".min.js", ".min.css", ".wasm", ".map", ".snap")
_GEN_NAMES = {
    "card-data.json", "card-data.json.new", "coverage-data.json",
    "coverage-history.json", "coverage-summary.json", "pnpm-lock.yaml",
    "package-lock.json", "yarn.lock",
}


def is_generated(path: str) -> bool:
    p = path.lower()
    if p.startswith("data/") or "node_modules/" in p:
        return True
    if p.endswith(_GEN_SUFFIXES):
        return True
    if p.rsplit("/", 1)[-1] in _GEN_NAMES:
        return True
    if "fixtures/" in p and p.endswith(".json"):
        return True
    return False


# ── Whitelist / identity resolution ──────────────────────────────────────────
# GitHub noreply email -> login. Logins are [A-Za-z0-9-]; `[^@+]+` keeps a
# spreadsheet-mangled numeric id (e.g. "6.60056e+06+Whovencroft@...") from being
# swallowed as the login by the plain form.
_NOREPLY_ID = re.compile(r"^\d+\+([A-Za-z0-9-]+)@users\.noreply\.github\.com$")
_NOREPLY_PLAIN = re.compile(r"^([^@+]+)@users\.noreply\.github\.com$")
_PR_NUM = re.compile(r"\(#(\d+)\)")


class Whitelist:
    """Maps a GitHub login (primary) or a git email (fallback) to a group."""

    def __init__(self, path: Path):
        data = tomllib.loads(path.read_text())
        self.group_of: dict[str, str] = {}        # canonical login -> group
        self.email_to_login: dict[str, str] = {}  # alias email     -> canonical login
        self.login_alias: dict[str, str] = {}     # any login       -> canonical login
        seen_emails: dict[str, str] = {}
        for entry in data.get("contributor", []):
            login = entry["login"].lower()
            group = entry["group"]
            if group not in VALID_GROUPS:
                raise SystemExit(
                    f"{path}: contributor '{login}' has invalid group '{group}' "
                    f"(must be one of {sorted(VALID_GROUPS)})")
            self.group_of[login] = group
            self.login_alias[login] = login
            for alias in entry.get("aliases", []):
                a = alias.lower()
                if "@" in a:
                    if a in seen_emails and seen_emails[a] != login:
                        raise SystemExit(
                            f"{path}: email alias '{a}' is claimed by both "
                            f"'{seen_emails[a]}' and '{login}' — a shared email "
                            f"must belong to exactly one contributor.")
                    seen_emails[a] = login
                    self.email_to_login[a] = login
                else:
                    self.login_alias[a] = login

    def by_login(self, login: str) -> tuple[str, str]:
        """Primary path: resolve a GitHub PR-author login to (canon, group)."""
        canon = self.login_alias.get(login.lower(), login.lower())
        return canon, self.group_of.get(canon, "unclassified")

    def by_commit(self, name: str, email: str) -> tuple[str, str]:
        """Fallback path (direct pushes only): resolve git (name, email).

        Name is deliberately not consulted — it is the easiest field to spoof.
        """
        email_l = email.lower()
        if email_l in self.email_to_login:
            return self.by_login(self.email_to_login[email_l])
        for rx in (_NOREPLY_ID, _NOREPLY_PLAIN):
            m = rx.match(email_l)
            if m:
                return self.by_login(m.group(1))
        return email_l or name.lower(), "unclassified"  # key by email for triage


# ── git / gh helpers ─────────────────────────────────────────────────────────
def run(cmd: list[str]) -> str:
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        sys.stderr.write(f"command failed: {' '.join(cmd)}\n{res.stderr}\n")
        sys.exit(1)
    return res.stdout


def git(repo: Path, *args: str) -> str:
    return run(["git", "-C", str(repo), *args])


def pr_login_map(repo_slug: str, limit: int) -> tuple[dict[int, dict], bool]:
    """N -> {login, is_bot} for merged PRs, straight from the GitHub API."""
    out = run(["gh", "pr", "list", "--repo", repo_slug, "--state", "merged",
               "--limit", str(limit), "--json", "number,author"])
    prs = json.loads(out)
    m: dict[int, dict] = {}
    for pr in prs:
        author = pr.get("author") or {}
        m[pr["number"]] = {"login": author.get("login", ""),
                           "is_bot": author.get("is_bot", False)}
    return m, len(prs) >= limit


def gittensor_pr_map(repo_slug: str, api_url: str | None) -> tuple[dict[int, dict], str | None]:
    """Fetch Gittensor-attributed PRs for repo_slug from the public API.

    Returns PR number -> API record. If the API is unavailable, returns an empty
    map plus a warning string so callers can fall back to the local whitelist.
    """
    if not api_url:
        return {}, None
    try:
        req = urllib.request.Request(
            api_url,
            headers={"User-Agent": "phase-rs-gittensor-impact/1.0"},
        )
        with urllib.request.urlopen(req, timeout=30) as response:
            records = json.load(response)
    except (OSError, urllib.error.URLError, json.JSONDecodeError) as exc:
        return {}, f"Gittensor API unavailable ({exc}); falling back to contributors.toml"

    repo = repo_slug.lower()
    pr_records: dict[int, dict] = {}
    for record in records:
        if not isinstance(record, dict):
            continue
        if str(record.get("repository", "")).lower() != repo:
            continue
        number = record.get("pullRequestNumber")
        if not isinstance(number, int):
            continue
        # The git history only contains merged PRs, so open Gittensor records
        # cannot match commits anyway. Restricting to merged rows keeps the
        # metadata honest and avoids treating future-open PR numbers as impact.
        if record.get("mergedAt"):
            pr_records[number] = record
    return pr_records, None


# ── commit walk (unified attribution) ────────────────────────────────────────
REC, US = "\x00", "\x1f"                       # emitted by git's %x00 / %x1f
FMT = "%x00%H%x1f%an%x1f%ae%x1f%at%x1f%s"       # sha, name, email, unixtime, subject


def walk(repo: Path, ref: str, since: str | None, until: str | None,
         prmap: dict[int, dict], gittensor_prs: dict[int, dict], wl: Whitelist):
    cmd = ["log", ref, "--no-merges", "--no-renames", "--numstat", f"--format={FMT}"]
    if since:
        cmd.append(f"--since={since}")
    if until:
        cmd.append(f"--until={until}")

    def blank():
        return {"group": "unclassified", "name": "", "login": "", "commits": 0,
                "code_commits": 0, "prs": 0, "additions": 0, "deletions": 0,
                "code_additions": 0, "code_deletions": 0, "files": 0,
                "areas": defaultdict(int)}

    stats: dict[str, dict] = defaultdict(blank)
    first = last = None
    cur = None
    cur_has_code = False

    for line in git(repo, *cmd).splitlines():
        if line.startswith(REC):
            if cur is not None and cur_has_code:
                cur["code_commits"] += 1
            _sha, name, email, ts, subject = line[1:].split(US, 4)
            nums = _PR_NUM.findall(subject)
            pr_num = int(nums[-1]) if nums else None
            pr = prmap.get(pr_num) if pr_num else None
            if pr:
                login = wl.login_alias.get(pr["login"].lower(), pr["login"].lower())
                if pr["is_bot"]:
                    group = "bot"
                elif pr_num in gittensor_prs:
                    group = "gittensor"
                else:
                    group = wl.group_of.get(login, "unclassified")
            elif pr_num in gittensor_prs:
                login = str(gittensor_prs[pr_num].get("author") or f"pr-{pr_num}").lower()
                group = "gittensor"
            else:
                login, group = wl.by_commit(name, email)
            s = stats[login]
            s["group"] = group
            s["login"] = login
            if not s["name"]:
                s["name"] = name
            s["commits"] += 1
            if pr_num:
                s["prs"] += 1
            cur, cur_has_code = s, False
            t = int(ts)
            first = t if first is None else min(first, t)
            last = t if last is None else max(last, t)
        elif line.strip() and cur is not None:
            parts = line.split("\t")
            if len(parts) != 3:
                continue
            adds, dels, path = parts
            a = 0 if adds == "-" else int(adds)
            d = 0 if dels == "-" else int(dels)
            cur["additions"] += a
            cur["deletions"] += d
            cur["files"] += 1
            if not is_generated(path):
                cur["code_additions"] += a
                cur["code_deletions"] += d
                cur["areas"][area_of(path)] += a + d
                cur_has_code = True
    if cur is not None and cur_has_code:
        cur["code_commits"] += 1
    return stats, first, last


# ── aggregation & output ─────────────────────────────────────────────────────
def blank_group() -> dict:
    return {"contributors": 0, "logins": [], "commits": 0, "code_commits": 0,
            "prs": 0, "additions": 0, "deletions": 0, "code_additions": 0,
            "code_deletions": 0, "code_churn": 0, "files_touched": 0,
            "areas": defaultdict(int)}


def iso(ts: int | None) -> str | None:
    return datetime.fromtimestamp(ts, timezone.utc).isoformat() if ts else None


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ref", default="origin/main", help="git ref to measure")
    ap.add_argument("--repo-dir", type=Path, help="full clone to analyze "
                    "(default: this repo; must not be shallow)")
    ap.add_argument("--repo", help="owner/name for gh (default: auto-detect)")
    ap.add_argument("--since", help="git date, e.g. 2026-06-01 or '30 days ago'")
    ap.add_argument("--until", help="git date")
    ap.add_argument("--whitelist", type=Path,
                    default=Path(__file__).with_name("contributors.toml"))
    ap.add_argument("--gittensor-api-url", default="https://api.gittensor.io/prs",
                    help="public Gittensor PR API used for PR-number attribution "
                    "(empty string disables API attribution)")
    ap.add_argument("-o", "--out", default="contrib-stats.json", type=Path)
    ap.add_argument("--pr-limit", type=int, default=6000)
    ap.add_argument("--allow-shallow", action="store_true",
                    help="bypass the shallow-clone guard (numbers will be wrong)")
    ap.add_argument("--triage", action="store_true",
                    help="print unclassified contributors and exit")
    args = ap.parse_args()

    repo = args.repo_dir or Path(git(Path.cwd(), "rev-parse", "--show-toplevel").strip())
    if not args.allow_shallow and \
            git(repo, "rev-parse", "--is-shallow-repository").strip() == "true":
        sys.exit(
            f"ERROR: {repo} is a SHALLOW clone — a graft-boundary commit would\n"
            f"attribute the entire tree to one author. Get full history first:\n"
            f"  git -C {repo} fetch --unshallow\n"
            f"or point --repo-dir at a full clone. (--allow-shallow to override.)")
    sha = git(repo, "rev-parse", args.ref).strip()
    slug = args.repo or run(["gh", "repo", "view", "--json", "nameWithOwner",
                             "-q", ".nameWithOwner"]).strip()

    wl = Whitelist(args.whitelist)
    gittensor_prs, api_warning = gittensor_pr_map(slug, args.gittensor_api_url or None)
    if api_warning:
        sys.stderr.write(f"WARNING: {api_warning}\n")
    prmap, capped = pr_login_map(slug, args.pr_limit)
    stats, first, last = walk(repo, args.ref, args.since, args.until, prmap, gittensor_prs, wl)

    if args.triage:
        rows = sorted(((k, v) for k, v in stats.items() if v["group"] == "unclassified"),
                      key=lambda kv: -kv[1]["commits"])
        if not rows:
            print("No unclassified contributors for this ref/window.")
            return
        print(f"{'commits':>8} {'prs':>4}  {'name':<26}  triage-key")
        print("-" * 72)
        for key, v in rows:
            print(f"{v['commits']:>8} {v['prs']:>4}  {v['name'][:26]:<26}  {key}")
        print(f"\n{len(rows)} unclassified. Add to {args.whitelist} and re-run.")
        return

    groups = {"gittensor": blank_group(), "nongittensor": blank_group()}
    bots = {"commits": 0, "prs": 0}
    unclassified: list[dict] = []
    per_contributor: dict[str, dict] = {}

    for login, s in stats.items():
        g = s["group"]
        per_contributor[login] = {
            "group": g, "name": s["name"], "commits": s["commits"],
            "code_commits": s["code_commits"], "prs": s["prs"],
            "code_net": s["code_additions"] - s["code_deletions"],
            "code_churn": s["code_additions"] + s["code_deletions"]}
        if g == "bot":
            bots["commits"] += s["commits"]
            bots["prs"] += s["prs"]
            continue
        if g == "unclassified":
            unclassified.append({"key": login, "name": s["name"],
                                 "commits": s["commits"], "prs": s["prs"]})
            continue
        gr = groups[g]
        gr["contributors"] += 1
        gr["logins"].append(login)
        for k in ("commits", "code_commits", "prs", "additions", "deletions",
                  "code_additions", "code_deletions"):
            gr[k] += s[k]
        gr["files_touched"] += s["files"]
        for area, n in s["areas"].items():
            gr["areas"][area] += n

    for gr in groups.values():
        gr["code_churn"] = gr["code_additions"] + gr["code_deletions"]
        gr["logins"].sort()
        gr["areas"] = dict(sorted(gr["areas"].items(), key=lambda kv: -kv[1]))
    unclassified.sort(key=lambda r: -r["commits"])

    result = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "repo": slug,
        "ref": args.ref,
        "ref_sha": sha,
        "window": {"since": args.since, "until": args.until,
                   "first_commit": iso(first), "last_commit": iso(last)},
        "total_commits": sum(s["commits"] for s in stats.values()),
        "pr_limit_hit": capped,
        "gittensor_api": {
            "url": args.gittensor_api_url or None,
            "merged_prs_for_repo": len(gittensor_prs),
            "warning": api_warning,
        },
        "groups": groups,
        "bots": bots,
        "unclassified": unclassified,
        "per_contributor": per_contributor,
    }
    args.out.write_text(json.dumps(result, indent=2))

    g, n = groups["gittensor"], groups["nongittensor"]
    def row(label, key):
        return f"{label:>16}{g[key]:>14}{n[key]:>16}\n"
    sys.stderr.write(
        f"\n{slug} @ {args.ref} ({sha[:10]})  "
        f"window: {args.since or 'all'} .. {args.until or 'now'}\n"
        f"{'':>16}{'gittensor':>14}{'non-gittensor':>16}\n"
        + row("contributors", "contributors") + row("commits", "commits")
        + row("code commits", "code_commits") + row("merged PRs", "prs")
        + row("code churn", "code_churn")
        + f"{'code net LOC':>16}{g['code_additions'] - g['code_deletions']:>14}"
        f"{n['code_additions'] - n['code_deletions']:>16}\n"
        f"{'unclassified':>16}{len(unclassified):>14} (run --triage)\n\n"
        f"Wrote {args.out}\n")
    if capped:
        sys.stderr.write(f"WARNING: hit --pr-limit={args.pr_limit}; raise it.\n")
    if gittensor_prs:
        sys.stderr.write(f"Gittensor API attribution: {len(gittensor_prs)} merged PR(s) for {slug}\n")


if __name__ == "__main__":
    main()
