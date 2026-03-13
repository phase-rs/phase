---
name: add-interactive-effect
description: Use when adding any effect or ability that requires player input mid-resolution — choices, selections, modal decisions, or any WaitingFor/GameAction round-trip. Covers the continuation pattern, engine handler wiring, AI legal actions, multiplayer routing, and frontend UI.
---

# Adding an Interactive Effect

Interactive effects pause game resolution to wait for player input, then resume. Examples: Scry (choose top/bottom), Dig (choose cards to keep), Surveil (choose graveyard/library), Search (choose from library), Reveal+Choose (pick opponent's card).

The core mechanism is the **continuation pattern**: `resolve_ability_chain()` detects a waiting state, stashes remaining sub-abilities in `pending_continuation`, and returns. When the player responds, `engine.rs` resumes the chain.

**Before you start:** Trace Scry end-to-end as the simplest example:
- Resolver: `effects/scry.rs` — sets `WaitingFor::ScryChoice`
- Engine handler: `engine.rs` — `(WaitingFor::ScryChoice, GameAction::SelectCards)` arm
- Continuation: `pending_continuation` resumed after player selects
- AI: `legal_actions.rs` — generates legal card selections
- Frontend: `ScryOverlay` component

---

## The Continuation Pattern

This is the most important architectural concept for interactive effects.

### Problem

A card says "Scry 2, then draw a card." This is parsed as `Scry { count: 2 }` with `sub_ability: Draw { count: 1 }`. But Scry requires player input — the engine can't just barrel through to Draw.

### Solution: `pending_continuation`

```
resolve_ability_chain(ability) called:
    ↓
resolve_effect(Scry) → sets state.waiting_for = ScryChoice
    ↓
resolve_ability_chain detects waiting state:
    Has sub_ability (Draw)?
        YES → store as state.pending_continuation
        Return Ok(()) — chain paused
    ↓
[Player makes scry selection via GameAction::SelectCards]
    ↓
engine.rs handler processes the selection
    ↓
Check state.pending_continuation.take():
    Some(continuation) → resolve_ability_chain(continuation)
    None → return to Priority
```

### Key code: `resolve_ability_chain()` — `crates/engine/src/game/effects/mod.rs`

```rust
// After resolve_effect() returns, check if we entered a waiting state
if matches!(state.waiting_for,
    WaitingFor::ScryChoice { .. }
    | WaitingFor::DigChoice { .. }
    | WaitingFor::SurveilChoice { .. }
    | WaitingFor::RevealChoice { .. }
    | WaitingFor::SearchChoice { .. }
    // ← ADD YOUR NEW VARIANT HERE
) {
    // Stash remaining chain as continuation
    let mut sub_clone = sub.as_ref().clone();
    if sub_clone.targets.is_empty() && !ability.targets.is_empty() {
        sub_clone.targets = ability.targets.clone();  // propagate parent targets
    }
    state.pending_continuation = Some(Box::new(sub_clone));
    return Ok(());
}
```

**If you skip adding your variant to this match, sub-abilities after your interactive effect will execute immediately, bypassing the player choice entirely. This is the #1 source of bugs for interactive effects.**

### `pending_continuation` storage — `crates/engine/src/types/game_state.rs`

```rust
pub pending_continuation: Option<Box<ResolvedAbility>>,
```

### Target propagation

When the continuation is created, parent targets propagate down if the sub-ability has no targets of its own. This allows chains like "Exile target creature. Its controller gains life equal to its power" to work — the sub-ability receives the creature target from the parent.

---

## Checklist — Adding a New Interactive Effect

### Phase 1 — WaitingFor + GameAction

- [ ] **`crates/engine/src/types/game_state.rs` — `WaitingFor` enum**
  Add a variant carrying enough data for the frontend to render the choice UI:
  ```rust
  YourChoice {
      player: PlayerId,
      // Data the frontend needs to display options:
      cards: Vec<ObjectId>,     // if choosing cards
      options: Vec<String>,     // if choosing from named options
      // etc.
  },
  ```
  The `player` field is required — it determines who must act.

- [ ] **`crates/engine/src/types/actions.rs` — `GameAction` enum**
  Add a variant for the player's response. Reuse `SelectCards` or `SelectTargets` if they fit. Only create a new variant if the response shape is genuinely different:
  ```rust
  YourResponse {
      selection: YourSelectionType,
  },
  ```

### Phase 2 — Effect Resolver

- [ ] **`crates/engine/src/game/effects/<name>.rs` — resolver**
  The resolver does three things:
  1. Compute the choices available to the player
  2. Set `state.waiting_for = WaitingFor::YourChoice { ... }`
  3. Emit `GameEvent::EffectResolved`

  Pattern (from `effects/scry.rs`):
  ```rust
  pub fn resolve(state: &mut GameState, ability: &ResolvedAbility, events: &mut Vec<GameEvent>) -> Result<(), EffectError> {
      let player = ability.controller;
      // 1. Compute choices
      let cards = /* ... */;
      // 2. Set waiting state
      state.waiting_for = WaitingFor::YourChoice { player, cards };
      // 3. Emit event
      events.push(GameEvent::EffectResolved { kind: EffectKind::from(&ability.effect), source_id: ability.source_id });
      Ok(())
  }
  ```

- [ ] **`crates/engine/src/game/effects/mod.rs` — wire resolver + continuation match**
  Two changes:
  1. Add `Effect::YourEffect { .. } => your_module::resolve(state, ability, events)` to `resolve_effect()`
  2. **Add `WaitingFor::YourChoice { .. }` to the continuation match in `resolve_ability_chain()`** — this is critical

### Phase 3 — Engine Handler

- [ ] **`crates/engine/src/game/engine.rs` — `apply()` match arm**
  Add a `(WaitingFor::YourChoice { .. }, GameAction::YourResponse { .. })` arm:

  ```rust
  (WaitingFor::YourChoice { player, cards, .. }, GameAction::YourResponse { selection }) => {
      // 1. Validate the response
      // 2. Apply the choice to game state
      // 3. Resume continuation if present:
      if let Some(continuation) = state.pending_continuation.take() {
          // Optionally inject the chosen card/target into continuation's targets:
          // continuation.targets = vec![TargetRef::Object(chosen_id)];
          effects::resolve_ability_chain(state, &continuation, &mut events, 0)
              .map_err(|e| EngineError::InvalidAction(format!("{:?}", e)))?;
      }
      // 4. Return next waiting state (usually Priority, unless continuation set a new one)
      if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
          state.waiting_for.clone()
      } else {
          WaitingFor::Priority { player: state.active_player }
      }
  }
  ```

  **Key detail**: After resuming the continuation, check `state.waiting_for` — the continuation might have entered another interactive state (chained choices).

### Phase 4 — AI Legal Actions

- [ ] **`crates/phase-ai/src/legal_actions.rs` — `get_legal_actions()`**
  Add a match arm generating all legal responses for your `WaitingFor` variant:

  ```rust
  WaitingFor::YourChoice { cards, .. } => {
      // Generate all valid selections the AI could make
      cards.iter().map(|&id| GameAction::YourResponse { selection: id }).collect()
  }
  ```

  Consider:
  - Are all combinations valid, or only specific ones?
  - Is there a "decline" / "choose nothing" option?
  - For large choice sets, does the AI need a subset? (e.g., SearchChoice limits to first N)

### Phase 5 — Multiplayer Routing

- [ ] **`crates/server-core/src/session.rs` — `acting_player()`**
  Add a match arm extracting the `player` field from your `WaitingFor` variant:

  ```rust
  WaitingFor::YourChoice { player, .. } => Some(*player),
  ```

  This ensures the server only accepts actions from the correct player.

### Phase 6 — Frontend

- [ ] **`client/src/adapter/types.ts` — `WaitingFor` + `GameAction` types**
  Add TypeScript discriminated union variants. `tsify` may auto-generate these from Rust — check `client/src/wasm/` for generated types and whether manual overrides exist in `types.ts`.

- [ ] **`client/src/pages/GamePage.tsx` or `client/src/components/` — UI component**
  Render the choice when `waitingFor.type === "YourChoice"`. Follow existing patterns:
  - `ScryOverlay` — card reordering (top/bottom)
  - `DigOverlay` — card selection (keep N)
  - `SurveilOverlay` — binary per-card choice (library/graveyard)
  - `ChoiceModal` — simple option selection (buttons)
  - `SearchOverlay` — filtered card selection from list

### Phase 7 — Multiplayer State Filtering (if hidden info)

- [ ] **`crates/server-core/src/filter.rs` — `filter_state_for_player()`**
  If your interactive effect reveals hidden information (opponent's hand, library cards, face-down cards):
  1. Track revealed cards via `state.revealed_cards` during the choice
  2. Clear revealed status after the choice is made
  3. Ensure `filter_state_for_player()` respects the revealed set

### Phase 8 — Tests

- [ ] Resolver test: effect sets correct `WaitingFor` with expected choices
- [ ] Engine round-trip test: set up waiting state → submit action → verify state change
- [ ] Continuation test: effect with sub_ability → interactive pause → resume → sub_ability executes
- [ ] AI test: `get_legal_actions()` returns valid options for the waiting state
- [ ] `cargo test -p engine && cargo clippy --all-targets -- -D warnings`

---

## Interactive Replacement Effects

When a replacement effect (not a regular effect) needs player input, the pattern is different because the choice must happen **before the zone change** (see `add-replacement-effect` skill, Interactive Replacements section).

The key difference:
- **Regular interactive effect**: resolver sets WaitingFor, engine handler resumes via continuation
- **Interactive replacement**: the replacement pipeline pauses, stores the pending `ProposedEvent`, waits for choice, then executes the zone change with the choice applied

For interactive replacements, you need to:
1. Add WaitingFor + GameAction as above
2. Instead of an effect resolver, modify the replacement pipeline in `replacement.rs` to detect and pause
3. In `engine.rs`, handle the response by applying the choice, then resuming the stored zone change
4. This is more complex — see `add-replacement-effect` skill for the full pattern

---

## Reference: Existing Interactive Effects

| Effect | WaitingFor | GameAction | Complexity |
|--------|-----------|------------|------------|
| **Scry** | `ScryChoice { player, cards }` | `SelectCards { cards }` | Simple — binary per-card (top/bottom) |
| **Dig** | `DigChoice { player, cards, keep_count }` | `SelectCards { cards }` | Medium — select exactly N cards |
| **Surveil** | `SurveilChoice { player, cards }` | `SelectCards { cards }` | Simple — binary per-card (library/graveyard) |
| **RevealHand** | `RevealChoice { player, cards, filter }` | `SelectCards { cards }` | Medium — select from filtered set, clears revealed state after |
| **SearchLibrary** | `SearchChoice { player, cards, count }` | `SelectCards { cards }` | Complex — filter library, "fail to find" rule, multi-card select |
| **Replacement** | `ReplacementChoice { player, count, descriptions }` | `ChooseReplacement { index }` | Different pattern — index-based selection |

Note how Scry, Dig, Surveil, RevealHand, and SearchLibrary all reuse `GameAction::SelectCards`. Only create a new `GameAction` variant if the response shape is genuinely different from card selection.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| **Missing continuation match in `resolve_ability_chain()`** | Sub-abilities after your effect execute immediately, bypassing player choice | Add your `WaitingFor` variant to the match block in `effects/mod.rs` |
| Missing `acting_player()` arm in `session.rs` | Server rejects all actions for this state in multiplayer | Add the match arm |
| Missing AI legal actions | AI hangs forever waiting for a response it can't generate | Add match arm in `legal_actions.rs` |
| Not clearing revealed state after choice | Opponent's hidden cards remain visible permanently | Clear `state.revealed_cards` in the engine handler |
| Resuming continuation without checking `state.waiting_for` | Continuation might set another interactive state, but you overwrite it with Priority | Check waiting_for after `resolve_ability_chain` returns |
| Not propagating targets to continuation | Sub-ability can't reference the chosen card | Copy parent targets when `sub_clone.targets.is_empty()` |
| Creating new `GameAction` when `SelectCards` works | Unnecessary type proliferation | Reuse `SelectCards` unless response shape is genuinely different |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update if stale**: function names, file paths, or enum variants that moved
3. **Add new patterns**: if you added a new interactive effect, add it to the reference table

### Verification

```bash
rg -q "fn resolve_ability_chain" crates/engine/src/game/effects/mod.rs && \
rg -q "pending_continuation" crates/engine/src/types/game_state.rs && \
rg -q "fn resolve_effect" crates/engine/src/game/effects/mod.rs && \
rg -q "fn get_legal_actions" crates/phase-ai/src/legal_actions.rs && \
rg -q "fn acting_player" crates/server-core/src/session.rs && \
rg -q "enum WaitingFor" crates/engine/src/types/game_state.rs && \
rg -q "enum GameAction" crates/engine/src/types/actions.rs && \
rg -q "ScryChoice" crates/engine/src/game/effects/mod.rs && \
echo "✓ add-interactive-effect skill references valid" || \
echo "✗ STALE — update skill references"
```
