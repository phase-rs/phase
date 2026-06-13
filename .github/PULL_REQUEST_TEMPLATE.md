## Summary

<!-- What this PR changes and why. One or two sentences. -->

## Implementation method (required)

How was the engine/parser logic in this PR produced? Check exactly one:

- [ ] Produced via the `/engine-implementer` pipeline (plan → review-plan → implement → review-impl → commit)
- [ ] **Not** `/engine-implementer` — explain why below

If you did **not** use `/engine-implementer`, state why (e.g. frontend-only
change, docs/CI/tooling change, release chore, or a fix too small to warrant
the pipeline):

> _your reason here_

> [!NOTE]
> Any change to `crates/engine/` game logic — parser, effects, resolver,
> targeting, rules behavior — is expected to go through `/engine-implementer`.
> The "not used" box is for changes that genuinely fall outside that scope.

## CR references

<!-- `CR XXX.Y` annotations added or touched, or "None" for non-rules changes. -->

## Verification

<!-- Commands run and their results, or a note on how CI covers this change. -->
