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

## In-app changelog entry (`client/public/changelog.json`)

After producing the Discord block, emit the SAME content as one structured entry
appended to the in-app changelog so preview/staging/production surface it too.
One run → both outputs. One entry per batch.

`client/public/changelog.json` is the canonical, committed feed (newest-first,
ids append-only and never reordered). Prepend a new entry:

```json
{
  "id": <(entries[0].id ?? 0) + 1>,
  "date": "<run date, YYYY-MM-DD>",
  "title": "<short specific headline — see title rule below>",
  "tags": [<one tag per non-empty section, see map below>],
  "body": "<the emoji-sectioned bullets, verbatim from the Discord block but WITHOUT the `🎴 What's New in phase.rs` header line>",
  "discordUrl": "<optional link to the matching #announcements post>"
}
```

Section emoji → tag (the `tags` array mirrors which sections the body contains):

| Section | Tag |
|---------|-----|
| `✨ New Cards & Mechanics` | `new-cards` |
| `🛠️ Cards That Now Work Right` | `card-fixes` |
| `⚔️ Combat & Gameplay` | `gameplay` |
| `🖥️ Interface` | `interface` |
| `🌍 Localization` | `localization` |
| `🤖 AI` | `ai` |
| `🌐 Multiplayer` | `multiplayer` |

The `tags` union is closed (the frontend has a tag→label/color lookup over
exactly these values) — do not invent new tags. The `body` is rendered as plain
text (newlines preserved); no Markdown/HTML.

**Title rule:** lead with the batch's single most notable item — a new
mechanic, format, or marquee fix — phrased as a specific ~3–8 word headline
(e.g. `"Planechase format arrives"`, `"Dark Depths makes Marit Lage"`,
`"Stickers land, plus 30+ mana sources that finally tap"`). Do NOT generate a
tag-join like `"New cards & Card fixes"` or `"Gameplay, Interface & AI"`: the
modal already shows colored tag chips, so a tag-named title is redundant and
unscannable. Each title must be distinct from existing ones.

Then regenerate the tiny pointer the app reads on every load:

```bash
bun scripts/gen-changelog-meta.ts
```

This rewrites `client/public/changelog-meta.json` (`{ latestId }`) and asserts
the changelog invariants (newest-first, unique ids). CI fails if the committed
meta drifts from `changelog.json`, so always run it before committing.

Commit both `changelog.json` and `changelog-meta.json`. The preview snapshot
updates on the next push to main; production picks it up at the nightly release.

## Footer

Outside the code block:

- List omitted commits and why they were omitted.
- State the new tip hash so the next sequential batch can use `<tip>..origin/main`,
  and record it in `scripts/changelog/state.json` (`{ "lastTip": "<hash>" }`) —
  the generation-side watermark (distinct from the user-facing
  `lastSeenChangelogId`).
