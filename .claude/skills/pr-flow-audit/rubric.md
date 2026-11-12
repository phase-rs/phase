# Review-finding rubric (version 1)

You rate the findings in the maintainer's formal change-request reviews of a few PRs. Rate every review id shown, including a review now DISMISSED that requested changes at that head. The lead gives you the PRs and, for each, its round heads: `round N`, a commit oid, review ids and a time. A round is one head with a change request; use only the review ids shown. Read the review bodies and the diff at that head (`gh pr view`, `gh api`, `git fetch origin <sha>`). Everything is read-only.

## Output

A tab-separated file with this header, one row per finding:

`pr	round	fid	location	origin	late	severity	latent	bot_treatment	outcome	late_evidence	anchor	finding`

- `round` is the round number printed for the PR. `fid` is `R<round>-F<i>`, with `i` the finding's 1-based order within that round's review body or bodies.
- `anchor` (`path:line` or a short locator) and `finding` (one short sentence) are required and must not be empty: they identify the row to a blind re-rater. Further extra columns are allowed and ignored.
- Use exactly the values below. When you cannot tell, write `UNSURE`. Do not guess.
- Raters write `severity` as `UNSURE`; the lead sets it.

## Axes

**location**, where the finding points:
- `IN_DIFF`: lines the PR adds or changes.
- `ADJACENT`: unchanged code in a file the PR changes, or code the diff calls or is called by.
- `ELSEWHERE`: any other file.
- `PROCESS`: the PR body, tests-as-evidence, CI, template or review process rather than code.

**origin**, where the defect came from:
- `ORIGINAL`: present in the PR's first head.
- `FIX_INDUCED`: introduced by a change made in answer to an earlier round.
- `MAIN_DRIFT`: caused by changes on main or by a merge of main.
- `REVIEWER_REVERSAL`: contradicts an earlier maintainer finding or instruction.
- `FALSE_POSITIVE`: the finding is wrong, shown by the code, the rules text or a later withdrawal.

**late**, whether the finding could have been raised earlier:
- Round-1 rows are `-`. Rows from round 2 on are never `-`.
- For a round-k row (k ≥ 2) anchored at `path:line`, `LATE` means the anchored lines are identical at round k-1's head and the finding was not raised at or before round k-1.
- Check identity with `git fetch origin <sha>`, then `git diff <head k-1> <head k> -- <path>`; the anchored lines must be unchanged, even when they appear as context in a changed hunk. Otherwise `NOT_LATE`.
- Record the diff command and its result in `late_evidence`. A head you cannot fetch gives `UNSURE`.

**severity**, set by the lead:
- `BLOCKER` only when the finding establishes a concrete failure: wrong behaviour of a named printed card or the PR's claimed card, reproduced or derived clause by clause; a false CR citation; a test that does not discriminate the PR's own claim; a security issue or hard stop; a broken build or red CI.
- `SHOULD`: a real defect, or a mandatory-architecture violation, without such a failure.
- `NIT`: no behavioural or contract effect.
- A row with `latent = yes` is at most `SHOULD`. When undecided between `BLOCKER` and `SHOULD`, choose `SHOULD` unless the finding or its evidence names the concrete failure. A possibility the code admits is not a behaviour until a card's text produces it.

**latent**: `yes` when the finding's own text or evidence calls the problem latent, synthetic, admitted-but-unproduced, or says no printed card establishes it; otherwise `no`.

**bot_treatment**, how the review treats the finding: `blocking` when it must be fixed before approval, `non-blocking` when the review labels it optional, a nit or a follow-up.

**outcome**, what happened to it by the PR's last head: `fixed`, `open` (still unresolved), `ignored` (the author never answered it), `disputed-withdrawn` (the maintainer withdrew it after dispute), `abandoned` (the PR closed unmerged with it open).
