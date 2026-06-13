## Summary

Anikthea, Hand of Erebos creates a token that's a copy of the card it just exiled, but the token was copying Anikthea itself instead.

## Root Cause

The cross-clause tracked-set rewrite in `contains_implicit_tracked_set_pronoun` only recognized `"copy of that card"` as a tracked-set anaphor. Anikthea's Oracle text uses `"copy of it"`, so the rewrite never fired. The `CopyTokenOf` effect kept `ParentTarget`, which falls back to `source_id` (Anikthea) at runtime.

## Fix

Extend `copy_token_recall` to match both `"that card"` and `"it"` via `alt()`, so the tracked-set rewrite fires for either phrasing (CR 603.7).

```rust
let copy_token_recall = (
    take_until::<_, _, OracleError<'_>>("copy of "),
    tag("copy of "),
    alt((tag::<_, _, OracleError<'_>>("that card"), tag("it"))),
)
    .parse(lower)
    .is_ok();
```

## Test

Added `issue_377_exile_and_copy_of_it_uses_tracked_set` which parses Anikthea's exact Oracle text and asserts the copy target resolves to `TrackedSet { id: TrackedSetId(0) }`.

Closes #377
