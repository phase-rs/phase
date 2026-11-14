---
name: pr-flow-audit
description: "Use for the weekly measurement of PR review flow in phase.rs: record the week's metrics, rate a sample of review findings, check rater drift, judge due change-ledger entries, and write a one-page report. Wraps scripts/pr_flow_metrics.py."
---

# PR Flow Audit

Run on Monday from the canonical checkout (the data lives untracked under `.agents/pr-flow-audit/weekly/`). `M` below is `python3 scripts/pr_flow_metrics.py`. Every command exits 0 on success or on an already-recorded no-op, and 2 on a refusal that wrote nothing. A refusal is not a result: read the message, fix the cause, rerun.

1. **Record metrics.** `M append --last-week`, then `M append --settled-due`. Appending is idempotent per week, age class and definitions digest.
2. **Sample.** `M sample --week <latest settled week>` prints the sample PRs and their round heads (3 with the most rounds, 5 random). The latest settled week is the newest week whose metrics record has `age_class: settled`.
3. **Rate.** Brief two Sonnet-class agents with `rubric.md` verbatim and half of the PRs each. They write a ratings TSV with every column except `severity` (they write `UNSURE`). The lead (Opus-class) then sets `severity` on every row. Save as `weekly/<week>/ratings.tsv`.
4. **Drift.** `M sample-drift --week <week> --ratings <tsv>` prints the seeded rows as a table (`pr round fid anchor finding`, identification only). Save it as `weekly/<week>/drift-items.tsv`. A fresh Opus-class agent re-rates exactly those rows blind, given only `drift-items.tsv`, the sample listing and `rubric.md`, never the ratings TSV, and writes `weekly/<week>/drift.tsv` with columns `pr round fid location origin late severity`. The drift rater rates `severity` itself on every row; writing `UNSURE` there is for step 3's raters only.
5. **Record the sample.** `M sample-record --week <week> --ratings <tsv> --drift <tsv>`. It validates both files, computes every number and flags axes whose drift check failed. A failed axis (including one with no comparable drift rows) is `unreliable`: the ledger excludes that week's sample values for every metric that reads the axis, in baselines and verdicts alike, and two consecutive failures on one axis make a rubric revision one of the proposals.
6. **Due entries.** `M ledger show --due` lists each landed change whose `review_after` has passed, with weekly baseline and post values, the baseline range, the post median and the mechanical `rule_verdict`.
7. **Judge.** One Opus-class pass reads that output, the last 6 records of each kind in `history.jsonl` and the sample record. For each due entry it writes a verdict JSON with exactly `id`, `week`, `verdict` and `note`, and runs `M ledger verdict <file>`. The command computes the per-target evidence itself, through the same eligibility rules as `ledger show --due`, and stores it with the `rule_verdict`; supplying `evidence` is refused. The verdict follows the rule: medians of weekly values against the baseline range, nothing pooled. A verdict that differs from `rule_verdict` needs a non-empty `note` explaining why.
8. **Report.** Write `weekly/<week>/report.md`, about one page: early and settled key metrics against their baseline ranges, verdicts, sample and drift results, and at most 3 proposals. A proposal is a draft change entry (hypothesis, primary target with version and age class, expected movement) with its evidence. Proposals are not entered into the ledger; the maintainer decides.

## Change ledger

- `M ledger add <file.json>`: the primary target needs a registered metric, a direction and a numeric `expected`.
- `M ledger baseline <id>` fills null baselines from weekly values; add `--refresh` to replace them.
- `M ledger landed <id> --commit <sha> --date <YYYY-MM-DD>` refuses while the primary baseline is null.

## Rules

- Never edit `history.jsonl`, `changes.jsonl` or a snapshot by hand. After a definition change, recompute with `M append --week <week> --input <snapshot>`.
- The module reads GitHub and the event log only. It writes to neither.
