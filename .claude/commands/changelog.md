Generate a Discord-ready "What's New" changelog from recent git history.

**Input:** `$ARGUMENTS` — either a date (e.g. "May 7", "2026-05-07", "May 7 1pm MST") or a commit ref (e.g. "abc1234", "v0.1.2"). If empty, default to the last 7 days.

**Step 1: Determine the git log range.**
- If the argument looks like a date/time, convert it to a `--since` flag with appropriate timezone handling (assume MST/MDT if no timezone given).
- If the argument looks like a commit hash or tag, use `<ref>..HEAD`.
- Exclude merge commits (`--no-merges`).

**Step 2: Read the commits.**
- Run `git log` with the determined range, fetching both subject lines and bodies (`--format="%H %s%n%b---"`).
- Read through all commits to understand the full scope of changes.

**Step 3: Synthesize into user-facing changelog.**
Group related commits and distill into a hyphen-prefixed list for Discord. Follow these rules:
- **User-facing language only.** Describe what players/users can now do, not internal implementation details. "Planeswalkers can now activate loyalty abilities at instant speed" not "Support instant-speed loyalty permissions".
- **Consolidate related commits.** Multiple commits that build toward one feature become one bullet. Don't mirror commits 1:1.
- **Skip internal-only changes.** Omit refactors, CI changes, feed refreshes, and code cleanup unless they have user-visible impact.
- **Concrete examples help.** Add parenthetical card names or mechanic names when they clarify what changed (e.g. "e.g. Teferi, Master of Time").
- **Keep it scannable.** Each bullet should be one line, two at most. No headers, no categories — just a flat list.
- **Order by impact.** Lead with the most exciting or broadly-applicable changes.

**Step 4: Output the result.**
Present the changelog in a single fenced code block so the user can copy-paste it directly into Discord. Do not add any preamble inside the code block — just the hyphen-prefixed list.
