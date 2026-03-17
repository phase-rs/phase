# Floodpits Drowner — Implementation Plan

**Oracle Text:**
> Flash
> Vigilance
> When this creature enters, tap target creature an opponent controls and put a stun counter on it.
> {1}{U}, {T}: Shuffle this creature and target creature with a stun counter on it into their owners' libraries.

## Current Status

- Flash: supported
- Vigilance: supported
- Stun counters: runtime exists (CR 122.1g untap replacement in `turns.rs`, counter storage in `game_object.rs`)
- PutCounter effect: exists and handles stun counters

## Gaps

### 1. ETB trigger missing stun counter sub_ability (parser fix)

The trigger's `execute` only contains the Tap effect. The "put a stun counter on it" is not chained as a `sub_ability`. The parser needs to emit:

```
Tap target creature an opponent controls
  └─ sub_ability: PutCounter { counter_type: "stun", count: 1, target: SameAsParent }
```

The target for the stun counter is the same creature that was tapped — needs a target reference back to the parent ability's target (not SelfRef, which would put the counter on Floodpits Drowner itself).

### 2. Target filter: "creature with a stun counter on it" (low complexity)

The activated ability targets "creature with a stun counter on it". This requires a `TargetFilter` / `FilterProp` variant that checks for counter presence on a permanent. Currently the filter system supports type, subtype, controller, and various properties — but not "has counter of type X".

Relevant types: `FilterProp` in `filter.rs`, `TargetFilter` in `oracle_target.rs`.

### 3. Activated ability: multi-object shuffle into owners' libraries (medium complexity)

The ability shuffles two objects — self and the targeted creature — into their *owners'* libraries (which may differ if the target is controlled but not owned by the same player). This needs:

- An effect that moves multiple specific objects (self + target) to their owners' libraries
- Each object goes to its *owner's* library (not controller's), then that library is shuffled
- The existing `ChangeZone` + `Shuffle` composition in the engine may be leverageable, but currently handles single-object zone changes

Existing patterns to trace: `ChangeZone` effect, `Shuffle` effect, and how the parser handles "shuffle into library" for cards like Jace (see `oracle_parser__snapshot_jace_the_mind_sculptor.snap`).
