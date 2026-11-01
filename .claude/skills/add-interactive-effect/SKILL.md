---
name: add-interactive-effect
description: Use when adding any effect or ability that requires player input mid-resolution — choices, selections, modal decisions, or any WaitingFor/GameAction round-trip. Covers the continuation pattern, engine handler wiring, AI legal actions, multiplayer routing, and frontend UI.
---

# Adding an Interactive Effect

Interactive effects pause game resolution to wait for player input, then resume. Examples: Scry (choose top/bottom), Dig (choose cards to keep), Surveil (choose graveyard/library), Search (choose from library), Reveal+Choose (pick opponent's card).

The core mechanism is the **continuation pattern**: `resolve_ability_chain()` parks remaining instructions in typed frames on `GameState::resolution_stack`. A public action completes the choice, then the shared resume path drains the owning frame until completion or another prompt.

**Before you start:** Trace Scry end-to-end as the simplest example:
- Resolver: `effects/scry.rs` — sets `WaitingFor::ScryChoice`
- Engine handler: `engine_resolution_choices.rs::handle_resolution_choice` — `(WaitingFor::ScryChoice, GameAction::SelectCards)` arm
- Continuation: `finish_with_continuation` → `engine::resume_pending_continuation_if_priority`
- AI: `ai_support/candidates.rs` — generates legal card selections
- Frontend: `CardChoiceModal` → `ScryModal`

> **CR Verification Rule:** Every CR number in annotations MUST be verified by grepping `docs/MagicCompRules.txt` before writing. Do NOT rely on memory — 701.x and 702.x numbers are arbitrary sequential assignments that LLMs consistently hallucinate. Run `grep -n "^701.22" docs/MagicCompRules.txt` (etc.) for every number. If you cannot find it, do not write the annotation.

---

## The Continuation Pattern

This is the most important architectural concept for interactive effects.

### Problem

A card says "Scry 2, then draw a card." This is parsed as `Scry { count: 2 }` with `sub_ability: Draw { count: 1 }`. But Scry requires player input — the engine can't just barrel through to Draw.

### Owners and APIs

Read the live implementations below rather than copying a hand-written continuation:

| Responsibility | Existing authority |
|---|---|
| Recognize a resolution pause and retain the remaining instructions | `game/effects/mod.rs::waits_for_resolution_choice` and `resolve_ability_chain` |
| Capture continuation context | `types/game_state.rs::PendingContinuation::new` |
| Park or inspect the active continuation | `GameState::park_ability_continuation`, `active_ability_continuation`, `active_ability_continuation_frame_mut` |
| Preserve an existing parent/child nesting relationship | The `append_to_pending_continuation` and `prepend_to_pending_continuation` paths in `game/effects/mod.rs`; trace the analogous caller before choosing a frame operation |
| Validate and deliver a resolution choice | `game/engine_resolution_choices.rs::handle_resolution_choice` |
| Resume after that choice settles | `finish_with_continuation` → `engine::resume_pending_continuation_if_priority` → `effects::drain_pending_continuation` / `resume_resolution_frames` |

These are paths under `crates/engine/src/`. An active continuation accessor only sees the owning top frame; a buried parent is not a fallback lookup. Preserve target bindings, producer identity, trigger context and parent event bookkeeping through the existing constructor and chain helpers. Do not recreate a `state.pending_continuation` slot or manually copy parent targets around these authorities.

### Completion paths

- **Immediate:** an effect may finish without a prompt. Trace its normal delivery and event publication; do not manufacture a waiting state merely to share the prompted path.
- **Paused:** the choice handler validates and publishes its result before invoking the shared resume path. Return the resulting waiting state; the continuation may have opened another prompt.
- **Empty or declined:** complete the same owning choice and publish its empty/declined result when a downstream consumer exists. Pending and completed-empty are different states; an old producer's result must not survive as the new result.
- **Nested or paused again:** preserve the parent frame while the child finishes. Do not take an outer continuation or reset priority to bypass the child. The Scry handler also demonstrates preserving observer events produced by a continuation that pauses again.

For executable examples, use the named production-entry recipes in [card-test](../card-test/SKILL.md#choose-the-production-entry-point). These examples guide the existing continuation tests below; they add no separate gate.

---

## Checklist — Adding a New Interactive Effect

### Phase 1 — WaitingFor + GameAction

- [ ] **`crates/engine/src/types/game_state.rs` — `WaitingFor` enum**
  Reuse or parameterize an existing choice when its semantics fit. If a distinct choice is needed, add a variant carrying enough data for the frontend to render it:
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
  Compute legal choices and either deliver immediately or set the appropriate `WaitingFor`. Trace `effects/scry.rs::resolve` for replacement-aware delivery and pause handling. Keep completion events at the analogous delivery/completion authority; opening a prompt is not proof that its action completed.

- [ ] **`crates/engine/src/game/effects/mod.rs` — wire resolver + continuation match**
  Two changes:
  1. Add `Effect::YourEffect { .. } => your_module::resolve(state, ability, events)` to `resolve_effect()`
  2. Classify the new pause in `waits_for_resolution_choice` and trace the applicable `resolve_ability_chain` parking branch. Reuse its frame/context handling rather than adding a parallel continuation slot.

### Phase 3 — Engine Handler

- [ ] **`crates/engine/src/game/engine_resolution_choices.rs` — resolution-choice dispatch and handler**
  Add the new waiting state to `handles(waiting_for)`, which `engine.rs` consults before dispatching to this module. Follow the Scry arm in `handle_resolution_choice`: validate the public response, deliver the choice, then finish through the shared completion path. Keep resolution-choice logic in this handler module. Preserve any subsequent prompt and observer events, including when delivery itself pauses on a replacement.

### Phase 4 — AI Legal Actions

- [ ] **`crates/engine/src/ai_support/candidates.rs` — candidate action generation**
  Legal action generation now lives in the engine crate (`engine::ai_support`), not in `phase-ai`. The entry point is `engine::ai_support::legal_actions(state)` which calls `candidate_actions()`. Add a match arm generating all legal responses for your `WaitingFor` variant:

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

- [ ] **`crates/engine/src/types/game_state.rs` — `WaitingFor::acting_player()` / `acting_players()`**
  Include the new choice's semantic actor in the existing engine query. `game/turn_control.rs::authorized_submitter` / `authorized_submitters` maps that actor to the authorized decision maker; `server-core/src/session.rs` delegates to those authorities. Do not add a second routing match in the server.

### Phase 6 — Frontend

- [ ] **`client/src/adapter/types.ts` — `WaitingFor` + `GameAction` types**
  Add TypeScript discriminated union variants. `tsify` may auto-generate these from Rust — check `client/src/wasm/` for generated types and whether manual overrides exist in `types.ts`.

- [ ] **`client/src/pages/GamePage.tsx` or `client/src/components/` — UI component**
  Render the choice when `waitingFor.type === "YourChoice"`. Follow existing patterns:
  - `CardChoiceModal` → `ScryModal` — card reordering (top/bottom)
  - `CardChoiceModal` → `DigModal` — card selection (keep N)
  - `CardChoiceModal` → `SurveilModal` — binary per-card choice (library/graveyard)
  - `ChoiceModal` — simple option selection (buttons)
  - `CardChoiceModal` → `SearchModal` — filtered card selection from list
  - `NamedChoiceModal` — named choices including `CardName`, `NumberRange`, `Labeled`, and `LandType`

- [ ] **Internationalize all frontend-authored text** (titles, prompts, buttons, labels) via `t()` — `const { t } = useTranslation("game")`, with keys added to `client/src/i18n/locales/en/game.json` first. Card names and Oracle text stay raw (content pipeline). Boundary rule and conventions: `client/src/i18n/README.md`. See `$add-frontend-component` Phase 2.5 for the full checklist.

### Phase 7 — Multiplayer State Filtering (if hidden info)

- [ ] **`crates/engine/src/game/visibility.rs` — `filter_state_for_viewer()`**
  Trace the analogous choice's visibility and cleanup through this engine authority. Distinguish private "look at" information from public reveals; do not put every choice's cards in `state.revealed_cards`. `server-core/src/filter.rs::filter_state_for_player` is a thin delegate. Verify the intended viewer and an unauthorized opponent during and after the choice.

### Phase 8 — Tests

- [ ] Resolver test: effect sets correct `WaitingFor` with expected choices
- [ ] Production round-trip test: enter `WaitingFor` through the real effect / cast / ability / replacement path, assert the required prompt, submit the action, and verify the effect and completion boundary using [card-test](../card-test/SKILL.md#observe-the-intended-completion-boundary). A manually constructed waiting state can supplement this as a handler unit test.
- [ ] Continuation test: effect with sub_ability → interactive pause → resume → sub_ability executes
- [ ] Empty/decline test: for optional choices or "up to" selections, submit the empty choice through the real `GameAction` path and verify tracked sets, continuations, and final `waiting_for` state are correct.
- [ ] Tracked-set test: if the choice feeds "those", "them", "this way", or a downstream tracked-set consumer, publish or clear a fresh tracked set on first choice resolution, including all-decline/empty paths, or prove no tracked-set consumer exists.
- [ ] Resume-state test: verify downstream continuations execute and priority / next waiting state is restored; do not only assert that the pending queue was cleared.
- [ ] AI test: `get_legal_actions()` returns valid options for the waiting state
- [ ] Verify through the caller's existing workflow. Under `engine-implementer`, use its executor's preparatory checks and Step 5 candidate checks; standalone, use [project-reference](../project-reference/SKILL.md) for the applicable verification commands and Tilt ownership. Do not run an additional full build from this checklist.

---

## Extending ChoiceType (Named Choice System)

The `NamedChoice` system is a well-contained interactive pattern with low blast radius for adding new choice types. Current `ChoiceType` variants: `CreatureType`, `Color`, `OddOrEven`, `BasicLandType`, `CardType`, `CardName`, `NumberRange { min, max }`, `Labeled { options }`, `LandType`, `Opponent`, `Player`, `TwoColors`.

### Architecture (all touchpoints)

| # | File | What to change |
|---|------|---------------|
| 1 | `crates/engine/src/types/ability.rs` — `ChoiceType` enum | Add variant with typed fields |
| 2 | `crates/engine/src/game/effects/choose.rs` — `compute_options()` | Add match arm to generate options for the new variant |
| 3 | `crates/engine/src/game/engine_resolution_choices.rs` — `ChooseOption` handler | May need custom validation (e.g., NumberRange validates parsed u8 in range) |
| 4 | `client/src/adapter/types.ts` | Already generic (`choice_type: string`, `options: string[]`) — **no change needed** |
| 5 | `client/src/components/modal/NamedChoiceModal.tsx` | Add rendering branch only if the existing button grid / card-name search is insufficient |
| 6 | `client/src/components/modal/NamedChoiceModal.tsx` — `CHOICE_TYPE_TITLE_KEYS` | Map the new ChoiceType key → an i18n leaf (e.g. `Keyword: "keyword"`). The title renders via `t(\`namedChoice.title.${leaf}\`)`, so also add `namedChoice.title.<leaf>` to `client/src/i18n/locales/en/game.json`. Never hardcode the label. |
| 7 | `crates/engine/src/ai_support/candidates.rs` — `NamedChoice` arm | Already generates one action per option — works for any choice type with populated options |
| 8 | `crates/engine/src/parser/oracle_effect/` | Add parser patterns for the new choice text |
| 9 | `crates/engine/src/game/effects/choose.rs` — tests | Add test for `compute_options()` with new variant |

### Key design decisions

- **`last_named_choice: Option<String>`** stores the result for continuations. For `NumberRange`, the stored string is the number as text (e.g., `"3"`). Continuations parse it as needed. This avoids changing the continuation protocol.
- **Frontend `options: Vec<String>`** — for `NumberRange`, `compute_options()` generates `["0", "1", "2", ..., "7"]`. The current `ButtonGrid` renderer already works; a dedicated number input is optional UX polish, not required.
- **`Labeled { options }` carries its options in the enum variant** — unlike `Color` or `CreatureType` where options are hardcoded in `compute_options()`, `Labeled` options come from the parser (card-specific text like "fame" / "fortune").
- **Multiplayer filtering**: No changes needed — `NamedChoice` is public information, not filtered by `filter_state_for_player()`.
- **`source_id` / `persist`**: Existing mechanism for storing choice on the source object via `ChosenAttribute`. New choice types may not need persistence — use `persist: false` unless the choice must be remembered across turns.

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

### Card Selection Effects (reuse `GameAction::SelectCards`)

| Effect | WaitingFor | Complexity |
|--------|-----------|------------|
| **Scry** | `ScryChoice { player, cards }` | Simple — binary per-card (top/bottom) |
| **Dig** | `DigChoice { player, cards, keep_count }` | Medium — select exactly N cards |
| **Surveil** | `SurveilChoice { player, cards }` | Simple — binary per-card (library/graveyard) |
| **RevealHand** | `RevealChoice { player, cards, filter }` | Medium — select from filtered set, clears revealed state after |
| **SearchLibrary** | `SearchChoice { player, cards, count }` | Complex — filter library, "fail to find" rule, multi-card select |
| **Discover** | `DiscoverChoice { player, cards, discover_value }` | Medium — exile from top until CMC ≤ discover value, cast or put in hand |
| **Connive** | `ConniveDiscard { player, cards, count }` | Medium — discard after draw |
| **ChooseFromZone** | `ChooseFromZoneChoice { player, cards, count, zone }` | Medium — generic zone card selection |
| **ManifestDread** | `ManifestDreadChoice { player, cards }` | Simple — select card from top to manifest |
| **DiscardToHandSize** | `DiscardChoice { player, cards, count }` | Simple — select cards to discard down |
| **Proliferate** | `ProliferateChoice { player, permanents }` | Medium — select permanents to add counters |

### Targeting & Combat

| Effect | WaitingFor | Complexity |
|--------|-----------|------------|
| **TargetSelection** | `TargetSelection { player, pending_cast, legal_targets }` | Complex — spell/ability targeting |
| **MultiTargetSelection** | `MultiTargetSelection { player, slots, ... }` | Complex — multi-slot targeting |
| **TriggerTargetSelection** | `TriggerTargetSelection { player, trigger_index, ... }` | Complex — trigger target selection |
| **DeclareAttackers** | `DeclareAttackers { player, legal_attackers }` | Complex — combat phase |
| **DeclareBlockers** | `DeclareBlockers { player, legal_blockers }` | Complex — combat phase |
| **AssignCombatDamage** | `AssignCombatDamage { player, assignments }` | Complex — damage assignment order |
| **EquipTarget** | `EquipTarget { player, equipment_id, legal_targets }` | Simple — target for equip |
| **CopyRetarget** | `CopyRetarget { player, copy_id, ... }` | Medium — retarget copied spell |

### Named & Modal Choices

| Effect | WaitingFor | GameAction | Complexity |
|--------|-----------|------------|------------|
| **NamedChoice** | `NamedChoice { player, choice_type, options }` | `ChooseOption` | Medium — creature type, color, etc. |
| **ModeChoice** | `ModeChoice { player, modal, pending_cast }` | `SelectModes` | Medium — spell modal selection |
| **AbilityModeChoice** | `AbilityModeChoice { player, modal, source_id, ... }` | `SelectModes` | Medium — activated/triggered modal |
| **Replacement** | `ReplacementChoice { player, count, descriptions }` | `ChooseReplacement` | Simple — index-based selection |
| **TopOrBottom** | `TopOrBottomChoice { player, card }` | `ChooseTopOrBottom` | Simple — binary choice |
| **ChooseRingBearer** | `ChooseRingBearer { player, legal_targets }` | `SelectTargets` | Simple — ring bearer selection |
| **ChooseLegend** | `ChooseLegend { player, legends }` | `SelectCards` | Simple — legend rule |

### Cost & Payment

| Effect | WaitingFor | GameAction | Complexity |
|--------|-----------|------------|------------|
| **ManaPayment** | `ManaPayment { player }` | Mana declaration | Medium — X costs |
| **OptionalCost** | `OptionalCostChoice { player, costs, ... }` | `ChooseOptionalCosts` | Medium — kicker, additional costs |
| **DefilerPayment** | `DefilerPayment { player, life_per_pip, ... }` | `DefilerDecision` | Medium — pay life instead of mana |
| **WarpCost** | `WarpCostChoice { player, options, ... }` | `SelectWarpCost` | Medium — alternative cost choice |
| **UnlessPayment** | `UnlessPayment { player, cost, ... }` | `PayUnlessCost` | Medium — "unless you pay" |
| **DiscardForCost** | `DiscardForCost { player, cards, ... }` | `SelectCards` | Medium — discard as cost |
| **SacrificeForCost** | `SacrificeForCost { player, candidates, ... }` | `SelectCards` | Medium — sacrifice as cost |
| **ExileFromGraveyardForCost** | `ExileFromGraveyardForCost { player, cards, ... }` | `SelectCards` | Medium — exile from yard as cost |
| **HarmonizeTapChoice** | `HarmonizeTapChoice { player, candidates, ... }` | `SelectCards` | Medium — tap creature for cost |
| **WardDiscard** | `WardDiscardChoice { player, ... }` | `SelectCards` | Simple — ward discard cost |
| **WardSacrifice** | `WardSacrificeChoice { player, ... }` | `SelectCards` | Simple — ward sacrifice cost |

### Optional & Opponent Choices

| Effect | WaitingFor | GameAction | Complexity |
|--------|-----------|------------|------------|
| **OptionalEffect** | `OptionalEffectChoice { player, description }` | `ChooseOptionalEffect` | Simple — "you may" effects |
| **OpponentMay** | `OpponentMayChoice { player, description, ... }` | `ChooseOptionalEffect` | Simple — opponent "may" |
| **AdventureCast** | `AdventureCastChoice { player, card_id }` | `ChooseAdventureCast` | Simple — cast adventure side |
| **CopyTarget** | `CopyTargetChoice { player, copy_id, legal_targets }` | `SelectTargets` | Medium — what to copy |
| **CompanionReveal** | `CompanionReveal { player, companion_id }` | `RevealCompanion` | Simple — companion revelation |

### Game Structure

| Effect | WaitingFor | Complexity |
|--------|-----------|------------|
| **Mulligan** | `MulliganDecision { player }` | Simple — keep/mulligan (bottoming is a `MulliganDecisionPhase::BottomCards` sub-phase of the same variant) |
| **Sideboard** | `BetweenGamesSideboard { player, ... }` | Medium — sideboard swaps |
| **PlayDraw** | `BetweenGamesChoosePlayDraw { player }` | Simple — play/draw choice |
| **GameOver** | `GameOver { winners }` | Terminal state |

Note how many effects reuse `GameAction::SelectCards` — only create a new `GameAction` variant if the response shape is genuinely different (e.g., `ChooseOption` for string responses, `SelectModes` for index-based selection).

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| **Missing pause or handler classification** | The chain bypasses the choice, or its response never reaches the handler | Update `waits_for_resolution_choice` for parking and `engine_resolution_choices::handles` for action dispatch |
| Missing engine actor-query coverage | Multiplayer cannot authorize the choice correctly | Update `WaitingFor::acting_player()` / `acting_players()`; keep server routing delegated |
| Missing AI legal actions | AI hangs forever waiting for a response it can't generate | Add match arm in `engine/src/ai_support/candidates.rs` |
| Wrong visibility or missing reveal cleanup | Private choices leak, or revealed cards remain visible too long | Follow the analogous reveal lifetime and engine visibility authority |
| Testing only a manually constructed `WaitingFor` | Bypasses bugs in the real effect/cast/ability path that creates the choice | Add at least one production setup test that reaches the waiting state naturally |
| Overwriting the resumed waiting state | A continuation's next prompt disappears | Return the state produced by the shared completion/resume path |
| Clearing pending state without testing downstream continuation | The prompt disappears but the rest of the ability never resolves | Assert the continuation's effect and final waiting state |
| Empty choice leaves an old tracked set bound | "Those/them/this way" can refer to a prior producer instead of the empty choice | Publish/clear a fresh tracked set even for all-decline paths when a consumer exists |
| Reconstructing continuation bindings manually | A child reads the wrong target, producer or trigger context | Reuse the constructor and chain helpers at the analogous owning site |
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
rg -q "pub fn park_ability_continuation" crates/engine/src/types/game_state.rs && \
rg -q "fn finish_with_continuation" crates/engine/src/game/engine_resolution_choices.rs && \
rg -q "fn resume_pending_continuation_if_priority" crates/engine/src/game/engine.rs && \
rg -q "fn resolve_effect" crates/engine/src/game/effects/mod.rs && \
rg -q "fn legal_actions" crates/engine/src/ai_support/mod.rs && \
rg -q "fn acting_player" crates/server-core/src/session.rs && \
rg -q "enum WaitingFor" crates/engine/src/types/game_state.rs && \
rg -q "enum GameAction" crates/engine/src/types/actions.rs && \
rg -q "ScryChoice" crates/engine/src/game/effects/mod.rs && \
echo "✓ referenced symbols exist; read their callers to verify the recipe" || \
echo "✗ STALE — update skill references"
```
