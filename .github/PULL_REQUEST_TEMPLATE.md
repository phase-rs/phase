## Summary

<!-- What this PR changes and why. One or two sentences. -->

## Implementation method (required)

Method: /engine-implementer
<!-- Or: Method: not-applicable — <specific non-engine reason> -->

> [!NOTE]
> Any change to `crates/engine/` game logic — parser, effects, resolver,
> targeting, rules behavior — is expected to go through `/engine-implementer`.
> The "not used" box is for changes that genuinely fall outside that scope.

## CR references

<!-- `CR XXX.Y` annotations added or touched, or "None" for non-rules changes. -->

## Verification

- [ ] Required checks ran clean, or the exact CI-owned alternative is stated below.
- [ ] Gate A output below is for the current committed head.
- [ ] Final review-impl below is clean for the current committed head.
- [ ] Both anchors cite existing analogous code at the same seam.

- `<exact command or CI check>` — <exact result>

<!-- Commands run and exact results. Every required box must be checked. -->

## Gate A

Gate A PASS head=<40-hex-sha> base=<40-hex-sha>

## Anchored on

- path/to/existing.rs:123 — analogous authority/pattern
- path/to/existing.rs:456 — second analogous authority/pattern

## Final review-impl

Final review-impl PASS head=<40-hex-sha>

## Claimed parse impact

- <Exact Card Name>
<!-- Optional manual quality evidence; not an admission artifact. -->
