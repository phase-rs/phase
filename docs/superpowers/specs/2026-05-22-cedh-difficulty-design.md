# cEDH Difficulty — Design

**Status:** Draft for review
**Date:** 2026-05-22
**Owner:** AgilErck
**Scope:** Preset + bracket-5 game-setup lock + combo-recognition skeleton

---

## 1. Goal

Introduce an `AiDifficulty::CEDH` tier above `VeryHard` that is engineered to play cEDH (Commander Bracket 5) decks well as card coverage lands over time. The MVP delivers three things:

1. A dedicated cEDH AI preset that bypasses the existing 4-player paranoid search scaling.
2. A game-setup bracket lock: selecting cEDH on any AI seat cascades all AI seats to cEDH and restricts AI deck pickers to B5-legal decks.
3. A combo-recognition skeleton (forward prior-boost layer) that the existing planner uses to steer toward registered win conditions. The skeleton ships with the architecture, one stub combo entry, and the wiring — populating real cEDH combo lines is a follow-up phase.

Out of scope for this design is real combo-line content, archetype-specific tuning, backward-chaining combo synthesis, and multiplayer (`phase-server`) bracket lock support.

## 2. Non-goals

- **Real combo library.** The `ComboRegistry` ships with one stub entry as proof-of-life. Thoracle / Doomsday / Breach / Ad Naus / Food Chain / Dockside lines are follow-ups, gated on engine card coverage.
- **Backward-chaining synthesis.** The policy recognizes *registered* combos only; it does not infer new ones from card data.
- **Archetype tuning.** No Turbo / Storm / Stax / Midrange-specific policies or weights.
- **Stack-interaction depth.** No new "counter the counter" reasoning beyond the existing `ThreatWeightedReply` model.
- **Tutor target inference.** `ComboLinePolicy` boosts tutor priors when a piece is missing, but choosing *which* card to fetch stays with `policies/tutor.rs`.
- **AI cEDH deck pool seeding.** Users supply cEDH decks via the existing deck import flow; an empty AI catalog renders the existing "no decks" UI.
- **Multiplayer bracket lock.** `phase-server` cEDH gating is a follow-up; current scope is single-machine games with AI opponents.

## 3. Decisions captured during brainstorming

| Decision | Choice |
|---|---|
| MVP scope | Preset + lock + planner skeleton |
| Lock UX | Filter AI deck pickers to B5; leave human deck open with a warning chip |
| AI difficulty cascade | Auto-upgrade all other AI seats to cEDH with a one-time toast |
| Combo layer integration | `ComboLinePolicy` inside the existing policy registry |
| 4-player search budget | Skip paranoid scaling for cEDH; cEDH preset is calibrated for 4-player tables |
| Mulligan policy | Include a stub `CedhMulliganPolicy` in the skeleton |

## 4. Architecture

```
┌─ frontend (client/src/) ─────────────────────────────────────┐
│  AiDifficultyDropdown    add "cEDH" option                    │
│  AiOpponentConfig        cascade all AI to cEDH + toast       │
│  GameSetupPage           warning chip on non-B5 human deck    │
│  services/cedhLock.ts    NEW: single source of truth          │
│  services/aiDeckCatalog  filter AI pool by bracket            │
└──────────────────────────────────────────────────────────────┘
                              │
┌─ engine (crates/engine/) ───┴────────────────────────────────┐
│  database/legality.rs    NEW fn validate_cedh_bracket(...)    │
└──────────────────────────────────────────────────────────────┘
                              │
┌─ phase-ai (crates/phase-ai/) ┴───────────────────────────────┐
│  config.rs               AiDifficulty::CEDH variant + preset  │
│  features/mod.rs         add `is_cedh: bool` to DeckFeatures  │
│  combo/                  NEW module: line, detection, registry│
│  policies/combo_line.rs  NEW: TacticalPolicy (gates via       │
│                          activation() on features.is_cedh)    │
│  policies/mulligan/cedh_keepables.rs  NEW: MulliganPolicy     │
│                          (gates via features.is_cedh inside   │
│                          evaluate())                          │
│  policies/registry.rs    register ComboLinePolicy             │
│  policies/mulligan/mod.rs  register CedhKeepablesMulligan     │
└──────────────────────────────────────────────────────────────┘
```

The combo recognition layer is a **forward prior-boost mechanism**, not a backward-chaining planner. It knows which candidate actions progress registered combos and steers the existing beam-search planner toward them. Synthesizing new combo lines from card data is explicitly out of scope.

## 5. Components

### 5.1 Difficulty preset (`crates/phase-ai/src/config.rs`)

Add `AiDifficulty::CEDH` after `VeryHard` in the enum at `config.rs:36-42`. Extend `create_config()` with a new match arm:

| Knob | Value | Vs VeryHard |
|---|---|---|
| `temperature` | 0.2 | 0.3 → 0.2 |
| `risk_tolerance` | 0.4 | 0.45 → 0.4 |
| `interaction_patience` | 1.0 | 1.0 |
| `stabilize_bias` | 1.2 | 1.2 |
| `play_lookahead` | `true` | `true` |
| `combat_lookahead` | **`true`** | `false` → `true` (cEDH is the first tier to enable it; gates a combat projection in `combat_ai.rs:203` — verified safe, real compute cost paid inside the existing 1500ms budget) |
| `search.enabled` | `true` | `true` |
| `search.max_depth` | 3 | 3 |
| `search.max_nodes` | 96 | 64 → 96 |
| `search.max_branching` | 5 | 5 |
| `search.planner_mode` | `BeamPlusRollout` | same |
| `search.rollout_depth` | 2 | 2 |
| `search.rollout_samples` | 2 | 2 |
| `search.opponent_model` | `ThreatWeightedReply` | same |
| `search.time_budget_ms` | `AI_SEARCH_TIME_BUDGET_MS` | same (1500ms) |
| `search.threat_awareness` | `Full` | same |
| `search.projection_min_budget_ms` | 1500 | 2000 → 1500 |

**WASM scaling** at `config.rs:510-514` applies unmodified. For cEDH that produces `max_depth = 2`, `max_nodes = 64`, `rollout_depth = 2` — a step up from WASM-VeryHard-at-4-players while still inside browser constraints.

**4-player scaling** at `config.rs:543-563` is the only place the cEDH preset diverges from the existing scaling path:

```rust
match player_count {
    0..=2 => {}
    3..=4 => {
        if difficulty == AiDifficulty::CEDH {
            // cEDH plays exclusively at 4p — paranoid scaling
            // would cripple it. cEDH preset is already calibrated
            // for 4-player tables.
        } else {
            // existing paranoid path: cap depth at 2, reduce nodes to 2/3
            config.search.max_depth = config.search.max_depth.min(2);
            config.search.max_nodes = config.search.max_nodes * 2 / 3;
            config.search.max_branching = config.search.max_branching.min(4);
            config.search.rollout_depth = config.search.rollout_depth.min(1);
        }
    }
    _ => {
        // existing 5-6+ player path applies; cEDH preset will be clipped here
    }
}
```

Selecting cEDH at `player_count > 4` is rare in practice; it is **not** rejected at the engine layer for this MVP. The existing 5-6p path clips it. A follow-up may add a setup-time rejection.

### 5.2 Combo recognition module (`crates/phase-ai/src/combo/`)

```
combo/
  mod.rs           ComboRegistry, public re-exports, tests
  line.rs          ComboLine, ComboPiece, ComboStep, ComboReachability, WinKind
  detection.rs     ComboDetector trait + default impl
  registry.rs      registered combo lines (1 stub for skeleton)
```

**Core types (`combo/line.rs`):**

```rust
pub struct ComboLineId(pub u32);

pub struct ComboLine {
    pub id: ComboLineId,
    pub name: &'static str,
    pub pieces: Vec<ComboPiece>,
    pub mana_cost: ManaCost,
    pub action_sequence: Vec<ComboStep>,
    pub win_kind: WinKind,
}

pub enum ComboPiece {
    InHand(CardPredicate),
    OnBattlefield(CardPredicate),
    InGraveyard(CardPredicate),
    InLibrary(CardPredicate),  // tutorable
}

pub enum ComboStep {
    Cast { predicate: CardPredicate },
    Activate { predicate: CardPredicate, ability_index: u8 },
    Trigger { from: CardPredicate },
}

pub enum WinKind {
    ImmediateLoss,   // CR 104.2 explicit win/loss effect (Thoracle)
    InfiniteLoop,    // CR 726 infinite combat / mill / damage
    LethalDamage,    // burn / commander damage line
}

pub enum ComboReachability {
    NotReachable,
    ReachableThisTurn {
        missing_mana: u8,
        required_actions: Vec<GameAction>,
    },
    ReachableNextTurn {
        missing_pieces: Vec<ComboPiece>,
    },
    ReachableSoon {
        turns_estimated: u8,
    },
}
```

`CardPredicate` reuses existing engine card-data primitives — no new predicate language. It is a thin wrapper over the matching helpers in `engine::types::card_filter` so combo predicates compose with existing target/filter infrastructure.

**Detection (`combo/detection.rs`):**

```rust
pub trait ComboDetector: Send + Sync {
    fn assess(
        &self,
        state: &GameState,
        line: &ComboLine,
        ai: PlayerId,
    ) -> ComboReachability;
}
```

The default `DefaultComboDetector` walks `line.pieces`, checks zones via existing engine helpers (`zone_object_ids`, `TargetFilter::extract_in_zone`), and computes mana shortfall via `crate::zone_eval::available_mana`. No new engine primitives.

**Registry (`combo/registry.rs`):**

```rust
pub struct ComboRegistry {
    lines: Vec<ComboLine>,
    detector: Box<dyn ComboDetector>,
}

impl ComboRegistry {
    pub fn default() -> Self;
    pub fn reachable_lines(&self, state: &GameState, ai: PlayerId)
        -> Vec<(ComboLineId, ComboReachability)>;
}
```

**Proof-of-life combo:** picked at implementation time based on current engine coverage. Candidates listed in section 8 (open questions). The chosen combo is marked in code as a stub for skeleton wiring — not as the start of a real combo library.

**Explicitly NOT in the skeleton:**

- Stack interaction reasoning ("can I protect this combo from a counter?")
- Multi-line scoring ("combo A is better than combo B given the board")
- Tutor target inference ("if I cast Demonic Tutor, what should I grab?")
- Synthesis from card data (registry is hand-authored)

### 5.3 Combo policy (`crates/phase-ai/src/policies/combo_line.rs`)

Implements `TacticalPolicy`. The trait has four required methods (verified at `policies/registry.rs:152-165`): `id()`, `decision_kinds()`, `activation(features, state, player) -> Option<f32>`, and `verdict(ctx) -> PolicyVerdict`. **`activation()` is the gating point** — returning `None` opts the policy out for this candidate, paying zero cost.

`activation()` returns `Some(1.0)` only when `features.is_cedh` is true; otherwise `None`. Non-cEDH decks see the policy as a no-op.

`verdict()`:

1. Iterate `ComboRegistry::reachable_lines(state, ai)`. Cache the result keyed by `(quick_state_hash(state), ai)` so sibling search nodes reuse it.
2. If any line reports `ReachableThisTurn` and the candidate matches the next action in `required_actions`, return `PolicyVerdict::Score { delta: combo_progress_this_turn_bonus, reason }` (default `+15.0`).
3. If `ReachableNextTurn` and the candidate is a tutor / draw / ramp action that fetches a missing piece, return `Score { delta: combo_progress_next_turn_bonus, reason }` (default `+5.0`).
4. Otherwise `Score { delta: 0.0, reason }`.

Two new fields are added to `PolicyPenalties` (`config.rs`) so the bonuses are tunable through the same path as every other policy weight — no magic literals.

**Registration:** added to `PolicyRegistry::default()` at `policies/registry.rs:174-220` alongside the existing 35 policies. The `PolicyRegistry::shared()` `OnceLock` pattern remains unchanged — `activation()` does the gating, not registration.

**`DeckFeatures::is_cedh` field:** added as a new field on `DeckFeatures` at `features/mod.rs:42-54`. Populated from the deck's declared `CommanderBracketTier` at deck-analysis time: `is_cedh = (deck.tier == CommanderBracketTier::Cedh)`. Unlike the existing structural features (`LandfallFeature` etc.) which are detected from card text, `is_cedh` is a declaration-derived feature — this is intentional and documented at the field.

**Tests:** assert `activation()` returns `None` for `DeckFeatures::default()` (which has `is_cedh = false`), and returns `Some(1.0)` when `is_cedh = true`. Assert `verdict()` produces the expected boost on a state with the stub combo reachable.

### 5.4 cEDH mulligan stub (`crates/phase-ai/src/policies/mulligan/cedh_keepables.rs`)

`CedhKeepablesMulligan` implements `MulliganPolicy` (trait at `policies/mulligan/mod.rs:80-91`). Follows the existing naming convention (`AggroKeepablesMulligan`, `LandfallKeepablesMulligan`, etc.) and is registered alongside the others in `MulliganRegistry::default()` at `policies/mulligan/mod.rs:100-116`.

**Internal gating:** the mulligan trait has no `activation()` method — every registered policy is evaluated on every hand. So gating happens inside `evaluate()`: when `!features.is_cedh`, return `MulliganScore::Score { delta: 0.0, reason: PolicyReason::NotApplicable }`. Cheap no-op, matches the pattern used by other archetype-specific keepables policies.

**Stub heuristics (flagged as stub) when `features.is_cedh`:**

- `< 2 lands` or `> 4 lands` → `ForceMulligan { reason }`.
- No mana acceleration **and** no tutor **and** no interaction → `ForceMulligan { reason }`. Detection uses existing card-data feature tags (`is_fast_mana`, `is_tutor`, `is_counterspell`, `is_removal` — verify exact tag names at implementation time and substitute the actual ones).
- Otherwise `Score { delta: +1.0, reason }` (positive baseline so the cEDH-tagged hand is kept absent forced mulligans from other policies).

Real cEDH mulligan strategy ("keep only hands that win or stop the opponent from winning by turn 4") arrives when combo lines are populated; the mulligan policy can then ask `ComboRegistry::reachable_lines(hand_as_pseudo_state)` and make a real decision.

### 5.5 Engine-side bracket validation (`crates/engine/src/database/legality.rs`)

Per CLAUDE.md "the engine owns all logic." Bracket-lock enforcement at game-start is a rules-adjacent validation, so it lives in the engine.

```rust
pub fn validate_cedh_bracket(decks: &[&Deck])
    -> Result<(), BracketViolation>;
```

`BracketViolation` is a typed error reporting the offending deck. Implementation rule (confirmed against `bracket_estimate.rs:18-27` and the `estimator_never_returns_cedh` test at `:457`):

- `CommanderBracketTier::Cedh` is **manual-declaration only** — the bracket estimator algorithmically returns B1-B4 and never `Cedh`. There is no algorithmic test that promotes a B4 deck to cEDH.
- Therefore `validate_cedh_bracket` is a **tag check**: every deck in the game must have its tier explicitly set to `CommanderBracketTier::Cedh`. Any deck whose tier is `Exhibition`, `CoreCommander`, `UpgradedCommander`, or `Optimized` is rejected with a `BracketViolation` naming the deck and its declared tier.
- The user opts a deck into cEDH via the existing deck-builder tier-selection UI (already present per `BracketAuditPanel`/`BracketEstimateChip` tests). Tagging a casual deck as cEDH to bypass the warning chip is allowed — the user has declared intent and accepts the resulting match.

**Where it is called:** in the game-setup boundary right before `initialize_game` is dispatched. The WASM bridge, Tauri bridge, and `phase-server` all funnel through one validation function — no duplication across adapters.

**Failure mode:** typed `BracketViolation` bubbles to the frontend, which renders a blocking modal with the offending deck name and the violating axis/cards. Engine work does not proceed.

### 5.6 Frontend integration

**New utility — `client/src/services/cedhLock.ts`:**

```ts
export function anyAiOpponentIsCedh(config: GameSetupConfig): boolean;
export function applyCedhCascade(config: GameSetupConfig): GameSetupConfig;
export function isDeckCedhLegal(deck: Deck): boolean;
```

Single source of truth for "is any AI seat cEDH?" — all cEDH-lock decisions across the frontend go through these helpers. No scattered `if (ai === 'CEDH')` checks elsewhere.

**Modified files:**

1. **`AiDifficultyDropdown.tsx`** — Add the `cEDH` option. Display label includes a small "B5 lock" badge so users see the constraint *before* selecting.
2. **`AiOpponentConfig.tsx`** — On selecting cEDH for any seat, call `applyCedhCascade()` to set all other AI seats to cEDH. Fire a one-time toast: *"All AI opponents set to cEDH — deck pool restricted to bracket 5."* The cascade is reversed only by changing the difficulty back to a non-cEDH tier.
3. **`aiDeckCatalog.ts`** — Add `filterByBracket(decks, tier)`. When `anyAiOpponentIsCedh(config)`, the AI deck picker receives only B5/cEDH-legal decks.
4. **`GameSetupPage.tsx`** — Pre-submit: render a yellow warning chip next to the human deck selector when `anyAiOpponentIsCedh(config) && !isDeckCedhLegal(humanDeck)`. Chip text: *"Your deck is bracket {N} vs. a cEDH AI — expect to lose fast."* Does not block submit; engine validation handles the hard failure.
5. **`GamePage.tsx`** — Passive: receives any engine validation error and surfaces it as a blocking modal. No logic added here, just typed error rendering.

## 6. Data flow

```
User selects cEDH for AI seat 1
  │
  ├─► AiOpponentConfig.onChange
  │     applyCedhCascade(config)  → all AI seats now cEDH
  │     toast("All AI opponents set to cEDH")
  │
  ├─► AI deck pickers re-render
  │     aiDeckCatalog.filterByBracket(decks, Cedh)
  │     → only B5 decks visible
  │
  └─► Human deck slot
        if !isDeckCedhLegal(humanDeck): warning chip
        (no filter — human can still pick anything)

User clicks "Start Game"
  │
  ├─► adapter.initialize_game(...)
  │     ↓
  │   engine: validate_cedh_bracket(all_decks)
  │     → Ok(())                → proceed
  │     → Err(BracketViolation) → typed error → blocking modal
  │
  └─► During play
        PlannerServices uses cEDH config
          native: depth 3, 96 nodes, rollout 2 × 2 samples
          wasm:   depth 2, 64 nodes (existing WASM cap)
        Policy registry includes ComboLinePolicy (gated by difficulty)
        ComboLinePolicy queries ComboRegistry on each candidate
        Mulligan dispatched to CedhMulliganPolicy
```

## 7. Testing strategy

**Unit tests (Rust):**

- `config.rs` — cEDH preset values match the table in 5.1; `create_config_for_players(CEDH, Native, 4)` returns `max_depth == 3` (paranoid scaling skipped); WASM caps still apply to cEDH.
- `combo/detection.rs` — reachability transitions for the stub combo: all pieces present and mana available → `ReachableThisTurn`; one piece missing → `ReachableNextTurn`; nothing on board / hand → `NotReachable`.
- `policies/combo_line.rs` — `activation()` returns `None` when `features.is_cedh == false` and `Some(1.0)` when `true`; `verdict()` produces the expected boost on a state with the stub combo reachable; zero for unrelated actions.
- `policies/mulligan/cedh_keepables.rs` — `evaluate()` returns `Score { delta: 0.0 }` when `features.is_cedh == false`; triggers `ForceMulligan` on each stub heuristic when cEDH; keeps an otherwise-fine hand.
- `features/mod.rs` — `is_cedh` field defaults to `false`; populates correctly from `CommanderBracketTier::Cedh` deck metadata.
- `database/legality.rs` — `validate_cedh_bracket` accepts a B5/cEDH deck, rejects a B1-B4 deck with typed violation.

**Integration tests (Rust):**

- Full `choose_action` invocation with cEDH config + stub combo on board: planner picks the combo-progressing action over a generic alternative.
- AI-vs-AI duel using the existing `ai-duel` harness: cEDH vs cEDH on stub decks completes without panic and respects the search budget.

**Frontend tests (Vitest):**

- `cedhLock.test.ts` — cascade behavior, deck legality filter, edge cases (no AI seats, mixed difficulties before cascade fires).
- `AiOpponentConfig.test.tsx` — cascade triggers on cEDH select, toast fires exactly once per cascade.
- `GameSetupPage.test.tsx` — warning chip renders when human deck is non-B5 and any AI is cEDH; hides otherwise.
- `AiDifficultyDropdown.test.tsx` — cEDH option present in the menu; B5 badge renders.

**Not tested:**

- Real combo recognition quality (no real combos in the registry).
- Archetype tuning (none exists at this phase).
- Multiplayer cEDH (out of scope).

## 8. Open questions deferred to implementation

1. **Proof-of-life combo selection.** The skeleton needs one registered combo to verify end-to-end wiring. Candidates (in order of preference):
   - Kiki-Jiki, Mirror Breaker + Restoration Angel (infinite hasty creatures → lethal damage).
   - Heliod, Sun-Crowned + Walking Ballista (infinite damage).
   - A deliberately synthetic two-card lethal-damage line if neither of the above has clean engine coverage.

   The choice does not affect the architecture. Verified at implementation time against current coverage.

2. **`CardPredicate` shape.** May need a small extension if existing engine card-data predicates cannot express the combo pieces directly. Default expectation: existing primitives are sufficient.

3. **Tutor / draw / ramp classification for `ReachableNextTurn`.** Reuses existing card-data feature tags. Verify tag names at implementation time (`is_tutor`, `is_draw`, `is_ramp`).

## 9. Migration / rollout

- No data migration. All new code; no changes to existing serialization formats.
- `AiDifficulty` is `Serialize + Deserialize` with the existing serde tagging; adding a variant at the end is backwards-compatible for reads (older saves never reference `CEDH`). A serde-roundtrip test for the new variant is added alongside the existing one at `config.rs:723-733`.
- Frontend `AiDifficulty` TypeScript union is regenerated from Rust types via the existing tsify pipeline — no manual TS edits.
- Existing AI deck catalog data is unchanged. The bracket filter is purely a view layer over the existing catalog.

## 10. References

- [crates/phase-ai/src/config.rs](../../../crates/phase-ai/src/config.rs) — `AiDifficulty` enum, `create_config`, `create_config_for_players`, `PolicyPenalties`.
- [crates/phase-ai/src/planner/mod.rs](../../../crates/phase-ai/src/planner/mod.rs) — `PlannerServices`, beam search with alpha-beta + rollouts (the existing planner cEDH plugs into).
- [crates/phase-ai/src/policies/](../../../crates/phase-ai/src/policies/) — existing `TacticalPolicy` and `MulliganPolicy` implementations.
- [crates/engine/src/game/bracket_estimate.rs](../../../crates/engine/src/game/bracket_estimate.rs) — `CommanderBracketTier::Cedh`, bracket estimator, `BracketViolation` plumbing.
- [crates/engine/src/database/legality.rs](../../../crates/engine/src/database/legality.rs) — existing deck-legality helpers; `validate_cedh_bracket` is added here.
- [client/src/components/menu/AiOpponentConfig.tsx](../../../client/src/components/menu/AiOpponentConfig.tsx) — AI seat configuration UI; cascade applied here.
- [client/src/components/menu/AiDifficultyDropdown.tsx](../../../client/src/components/menu/AiDifficultyDropdown.tsx) — difficulty selector; `cEDH` option added here.
- [client/src/services/aiDeckCatalog.ts](../../../client/src/services/aiDeckCatalog.ts) — AI deck pool; bracket filter added here.
- CLAUDE.md design principles — "engine owns all logic", "build for the class not the card", "extend don't hack". All choices in this design respect these constraints.
