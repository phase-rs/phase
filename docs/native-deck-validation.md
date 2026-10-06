# Native deck compatibility

`cargo deck-check CARD-DATA.JSON REQUESTS.JSON` exposes the engine's existing
`DeckCompatibilityRequest` / `DeckCompatibilityResult` protocol. After building,
the `deck-check` executable takes the same two paths. Supply a matching generated
card-data export. Stdout is an ordered JSON result array; stderr carries input or
output failures. Exit 0 means evaluation succeeded, including incompatible decks;
exit 2 means the operation failed. An empty request array produces `[]`.

For example, save this as `requests.json`:

```json
[
  {
    "main_deck": ["Forest"],
    "commander": ["Lathril, Blade of the Elves"],
    "selected_format": "Commander",
    "player_count": 4,
    "summary_only": false
  }
]
```

This deliberately incomplete deck receives the engine's invalid-deck verdict,
not a CLI failure. Card copies are repeated names. Format values use the existing
DTO's case-sensitive spelling. All supported request fields and result semantics
remain owned by `crates/engine/src/game/deck_validation.rs`; the executable does
not add legality or coverage rules.

POD-Lab's `pod_lab.fields.validate` is the external consumer: it validates complete
candidate/opponent decks against the exact gameplay dataset before freezing a run.
The Cargo alias supplies an in-repository entry point. `set-check --deck` already
provides coverage for parser development; use it for per-deck coverage, AST hashes
and snapshot differences. This adapter exists for the additional format-legality
and zoned-deck request protocol, rather than introducing that protocol into the
parser-audit CLI. It does not claim that parser support certifies correct gameplay.

Reproducible checks live inline in `deck_check.rs`. They cover DTO/result parity,
ordered mixed requests, empty batches, malformed input, missing/non-ASCII paths,
and output failure. With Tilt unavailable, run the `deck-check` binary tests in
the `phase-engine` package with the `cli` feature. Follow normal Tilt instructions
when it is running; do not launch competing builds.
