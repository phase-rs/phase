---
name: changelog
description: Generate a Discord-ready "What's New" changelog for phase.rs from recent git history. Use when Codex is asked for a changelog, release notes, Discord update, recent shipped changes, or the next sequential changelog batch from a prior tip hash.
---

# Changelog

Generate a user-facing Discord changelog from git history. The input may be empty, a date/time, a commit ref, a tag, or a sequential "next batch" request.

## Range Selection

Always run `git fetch origin` first and use `origin/main` unless the user names another branch.

Do not use `git log --since` as authoritative in this repo. Squash-merge commit dates are non-monotonic, and `--since` can silently drop commits. Use one of these methods:

- **Commit hash or tag:** use graph reachability.
  ```bash
  git log --no-merges <ref>..origin/main --format="%h %s" | cat -n
  ```
- **Date/time:** convert the user input to a Unix epoch yourself, then filter `%ct`.
  ```bash
  cutoff=$(date -j -f "%Y-%m-%dT%H:%M:%S %z" "2026-05-22T21:05:00 -0700" "+%s")
  git log origin/main --no-merges --format="%ct %cI %h %s" \
    | awk -v c="$cutoff" '$1 >= c' | sort -rn | cat -n
  ```
- **Empty input:** default to the last seven days using the same epoch-filter method.

If no timezone is supplied, assume Mountain Time and state the offset used. Convert named timezones yourself before computing the epoch.

## Commit Reading

Read commit bodies, not only subjects, for every non-obvious commit:

```bash
git show -s --format="%s%n%n%b" <hash>
```

Cross-check the final changelog against the full commit count so no commit is silently dropped.

## Writing Rules

- Output the changelog in a single fenced code block.
- Start with `🎴 What's New in phase.rs`.
- Use emoji-headed sections only when they have content, usually in this order:
  - `✨ New Cards & Mechanics`
  - `🛠️ Cards That Now Work Right`
  - `⚔️ Combat & Gameplay`
  - `🖥️ Interface`
  - `🌍 Localization`
  - Other sections such as `🤖 AI` or `🌐 Multiplayer` when warranted.
- Use `•` bullets.
- Consolidate related commits; do not mirror commits one-to-one.
- Use player-facing language. Avoid implementation descriptions unless needed for clarity.
- Name concrete cards or mechanics in parentheses when helpful.
- Order by user impact.
- Omit internal-only changes unless they have visible impact.

## Footer

Outside the code block:

- List omitted commits and why they were omitted.
- State the new tip hash so the next sequential batch can use `<tip>..origin/main`.
