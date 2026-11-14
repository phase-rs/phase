//! U2 — the superlative player predicate, wired to both grammatical
//! positions: the effect SUBJECT ("the player with the most `<property>`
//! gains control of ~") and the TARGET position ("target player with the
//! most `<property>` draws a card"), CR 102.1 + CR 608.2c.
//!
//! Verbatim Oracle text throughout (never paraphrased). Wild Dogs (Cycling)
//! and Sokenzan Renegade (Bushido 1) carry inline keywords, built via
//! `from_oracle_text_with_keywords` per the `/card-test` recipe (foot-gun 4:
//! feeding reminder text as plain Oracle text parses to
//! `Effect::Unimplemented`).
//!
//! U2 depends on U1 (HARD ORDERING CONSTRAINT, PLAN-v3 §Sizing): on these
//! three cards the intervening-if condition U1 adds is what makes a tie at
//! resolution unreachable, so `unique_recipient_from_filter`'s fail-closed
//! "ambiguous GiveControl recipient" path is never hit here. That coupling
//! is pinned by `u2_r3_multi_authority_tie_blocks_trigger` below.
//!
//! Several rows below also assert `state.unimplemented_oracle_ids` is EMPTY,
//! alongside the controller check. **Corrected by the F1 fix (see
//! `f1_unimplemented_oracle_ids_not_recorded_for_bare_top_level_unimplemented`
//! below): this does NOT discriminate a revert of U2.** With U2 reverted the
//! clause lowers to `Effect::Unimplemented{name: "unbound_subject"}` as a
//! BARE top-level ability effect (no `sub_ability` chain), and
//! `game/stack.rs::execute_effect` skips such an ability before
//! `effects::resolve_effect`'s `Effect::Unimplemented` recording arm
//! (`game/effects/mod.rs`) is ever reached — so
//! `unimplemented_oracle_ids` stays empty whether U2 is
//! present or reverted. The rows on which the final-controller observable
//! ALSO coincides (`u2_r1`, `u2_r5`, `u2_r6` — leader already controls the
//! permanent) have no revert-discriminating assertion of their own; the
//! genuine hand-size-axis coverage for that is `u2_r5b`
//! (`u2_r2` already covers the life axis).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::PlayerId;

/// A synthetic "the player to your right gains control of ~" upkeep trigger
/// — the shipped, PRE-EXISTING `parse_subject_application` seating-neighbor
/// arm (not part of this diff), used only to prove the harness reaches
/// `Effect::GiveControl` at all. Bucknard's-Everfull-Purse-shaped, not
/// verbatim (that card gates the same clause behind an activated ability,
/// which is orthogonal to what this row measures).
const NEIGHBOR_REACH_GUARD: &str =
    "At the beginning of your upkeep, the player to your right gains control of this artifact.";

/// Ghazbán Ogre / Wild Dogs' verbatim upkeep-trigger line (CR 119.3 axis).
const LIFE_AXIS_LINE: &str = "At the beginning of your upkeep, if a player has more life than \
                               each other player, the player with the most life gains control \
                               of this creature.";

/// Wild Dogs' full verbatim Oracle text, including the inline Cycling line.
const WILD_DOGS: &str = "At the beginning of your upkeep, if a player has more life than each \
                          other player, the player with the most life gains control of this \
                          creature.\nCycling {2} ({2}, Discard this card: Draw a card.)";

/// Sokenzan Renegade's full verbatim Oracle text, including the inline
/// Bushido line.
const SOKENZAN_RENEGADE: &str = "Bushido 1 (Whenever this creature blocks or becomes blocked, \
                                  it gets +1/+1 until end of turn.)\nAt the beginning of your \
                                  upkeep, if a player has more cards in hand than each other \
                                  player, the player who has the most cards in hand gains \
                                  control of this creature.";

/// The subfamily-B decline shape (object count, not a player property): the
/// property `alt` has no `creatures` arm, so this must stay `unbound_subject`
/// — honestly red, per PLAN-v3's scope decision to leave subfamily B
/// unmodelled.
const SUBFAMILY_B_DECLINE_LINE: &str = "At the beginning of your upkeep, the player with the \
                                         most creatures gains control of this creature.";

/// "target player with the most life draws a card" — the P11 target-position
/// regression this diff closes as a side effect of U2.2.
const TARGET_LEADER_DRAW: &str = "Target player with the most life draws a card.";

/// "target opponent with the most life draws a card" — the G2 fix's own
/// regression coverage: the "opponent" head noun must scope the superlative
/// population to the caster's opponents (`PlayerRelation::Opponent`), not to
/// every player (`PlayerRelation::All`). Synthetic, matching the convention
/// `TARGET_LEADER_DRAW` above already establishes for this test file.
const TARGET_OPPONENT_LEADER_DRAW: &str = "Target opponent with the most life draws a card.";

fn upkeep_scenario(player_count: u8, seed: u64) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(player_count, seed);
    scenario.at_phase(Phase::Untap);
    scenario
}

/// U2-R0 — POSITIVE REACH-GUARD: the harness reaches `Effect::GiveControl`
/// at all, on a shape shipped BEFORE this diff. Unaffected by revert.
#[test]
fn u2_r0_positive_reach_guard_neighbor_control_moves() {
    let mut scenario = upkeep_scenario(3, 100);
    let card = scenario
        .add_creature(P0, "Neighbor Reach-Guard Test Card", 1, 1)
        .from_oracle_text(NEIGHBOR_REACH_GUARD)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    // CR 102.1 + CR 103.1: P0's "right" neighbor in seat order [P0,P1,P2] is
    // P2 (right = previous player; see `players::neighbor`).
    assert_eq!(
        runner.state().objects[&card].controller,
        PlayerId(2),
        "the harness must reach GiveControl and move control to the right neighbor"
    );
}

/// U2-R1 — Ghazbán Ogre under P0, unique life leader IS P0 (controller).
/// The final-controller observable (`P0`) is IDENTICAL whether U2 is present
/// or reverted (P0 already controls it), so that alone does not discriminate
/// — passes either way, like U1-R1/R3. The `unimplemented_oracle_ids` check
/// does NOT discriminate either (see the F1 fix,
/// `f1_unimplemented_oracle_ids_not_recorded_for_bare_top_level_unimplemented`):
/// a reverted clause lowers to a bare top-level
/// `Effect::Unimplemented{"unbound_subject"}`, which `game/stack.rs`'s
/// `execute_effect` skips before the recording arm is ever reached, so the
/// set stays empty either way. Kept here as a same-shape sanity check
/// (nothing unimplemented executes in the shipped, non-reverted path); the
/// genuine revert-discriminating coverage for this axis is `u2_r2`.
#[test]
fn u2_r1_leader_is_controller_resolves_cleanly() {
    let mut scenario = upkeep_scenario(3, 101);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(PlayerId(2), 10);
    let card = scenario
        .add_creature(P0, "Ghazbán Ogre", 2, 3)
        .from_oracle_text(LIFE_AXIS_LINE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&card].controller, P0);
    assert!(
        runner.state().unimplemented_oracle_ids.is_empty(),
        "in the shipped path nothing unimplemented executes here, so the set \
         should stay empty (NOT revert-discriminating — see the F1 fix); got {:?}",
        runner.state().unimplemented_oracle_ids
    );
}

/// U2-R2 — LOAD-BEARING: the leader is NOT the controller. REVERT-FAIL:
/// with U2 reverted the clause is `Effect::Unimplemented{unbound_subject}`
/// and NO control move occurs (control stays with P0); fixed, control moves
/// to the leader P1. Also pins that the recipient filter selects the
/// LEADER, not the controller.
#[test]
fn u2_r2_leader_not_controller_control_moves_to_leader() {
    let mut scenario = upkeep_scenario(3, 102);
    scenario
        .with_life(P0, 10)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 10);
    let card = scenario
        .add_creature(P0, "Ghazbán Ogre", 2, 3)
        .from_oracle_text(LIFE_AXIS_LINE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&card].controller,
        P1,
        "control must move to the unique life leader P1, not stay with the controller P0"
    );
}

/// Hostile fixture — MULTI-AUTHORITY: a 4-player board where two
/// NON-CONTROLLER players tie at the top and the controller is third. With
/// U1 present, the intervening-if removes the ability at resolution (CR
/// 603.4) before U2's recipient filter is ever evaluated, so
/// `unique_recipient_from_filter`'s fail-closed "ambiguous GiveControl
/// recipient" path is never reached on this card. NOT independently
/// revert-discriminating (§5.0) — pins the U1<->U2 coupling instead.
#[test]
fn u2_r3_multi_authority_tie_blocks_trigger() {
    let mut scenario = upkeep_scenario(4, 103);
    scenario
        .with_life(P0, 10)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 20)
        .with_life(PlayerId(3), 5);
    let card = scenario
        .add_creature(P0, "Ghazbán Ogre", 2, 3)
        .from_oracle_text(LIFE_AXIS_LINE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    // The final-controller assertion below cannot tell a BLOCKED condition
    // apart from a trigger that reached the stack and then failed closed in
    // `unique_recipient_from_filter` on the P1/P2 ambiguity: both outcomes
    // leave control with P0. Assert the trigger never reached the stack, so
    // this fixture pins the condition rather than the fail-closed path.
    //
    // Measured non-vacuous: break the tie (P2 20 -> 15, making P1 the unique
    // leader) and this assertion fires here with the message below. So the
    // stack IS populated at this point when the trigger fires, and a green
    // result means the trigger genuinely never reached it.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| entry.source_id != card),
        "U1's intervening-if must keep this trigger off the stack entirely; if it \
         reaches the stack, the controller assertion below is satisfied by \
         unique_recipient_from_filter failing closed instead"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&card].controller,
        P0,
        "a tie among non-controller leader candidates must block the trigger via U1's \
         condition before U2's recipient filter is ever evaluated"
    );
}

/// U2-R4a — P11 TARGET-POSITION REGRESSION, legality half. Board is 20/20/10
/// (P0 and P1 TIED for the max, P2 strictly lower) rather than a unique
/// leader: with a single legal candidate the cast pipeline's
/// `auto_select_targets` (CR 601.2c-style single-legal-option resolution,
/// `game/ability_utils.rs`) would silently pick it and skip straight to
/// `Priority`, never surfacing `WaitingFor::TargetSelection` for this test to
/// inspect. Two tied candidates force a real prompt. REVERT-FAIL: today the
/// qualifier is swallowed and the filter is bare `TargetFilter::Player`, so
/// P2 (strictly behind) is ALSO a legal target. Manually drives to the
/// `TargetSelection` window (rather than the `SpellCast` driver) so the
/// legal set can be inspected directly.
#[test]
fn u2_r4a_target_position_leader_is_only_legal_target() {
    let mut scenario = GameScenario::new_n_player(3, 104);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 10);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw Instant", true, TARGET_LEADER_DRAW)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the spell must be accepted");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "expected WaitingFor::TargetSelection (two tied candidates must force a real \
             prompt, not auto-select), got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "exactly one target slot expected");
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Player(P0)),
        "the tied life leader P0 must be a legal target, got {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Player(P1)),
        "the tied life leader P1 must be a legal target, got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Player(PlayerId(2))),
        "REVERT-FAIL: the strictly-lower P2 must NOT be a legal target, got {legal:?}"
    );
}

/// U2-R4b — P11 TARGET-POSITION REGRESSION, resolution half: casting the
/// spell targeting the unique leader P0 resolves the draw to P0.
#[test]
fn u2_r4b_target_position_leader_draws_card() {
    let mut scenario = GameScenario::new_n_player(3, 105);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(PlayerId(2), 10);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw Instant", true, TARGET_LEADER_DRAW)
        .id();
    // Give P0 a card to draw so the draw is observable.
    scenario.with_library_top(P0, &["Library Card"]);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P0).resolve();
    outcome.assert_hand_drawn(P0, 1);
}

/// G2 — "target opponent with the most life" must scope the superlative
/// population to P0's OPPONENTS, not to every player. P0 (the caster) has the
/// highest life in the game (30); P0's two opponents P1 and P2 are TIED at
/// 20, both below P0. CR 102.1 + CR 102.3: `Opponent` is a topology relation,
/// not raw life comparison, so the "most life" superlative must be measured
/// only against the population the head noun names — here, P0's opponents,
/// among whom P1 and P2 are tied leaders.
///
/// REVERT-FAIL: before the G2 fix, the superlative arm hardcoded
/// `PlayerRelation::All`, so the population was `PlayerScope::AllPlayers`
/// (life >= 30, the caster's own life) rather than `PlayerScope::Opponent`
/// (life >= 20, the max among P0's opponents). Composed with the head noun's
/// own `Typed{controller: Opponent}` leg, that required a legal target to be
/// both an opponent AND at least as high on life as the caster — on this
/// arrangement NO opponent satisfies it, so the target slot would have ZERO
/// legal targets and the cast could not even be declared. The tie between P1
/// and P2 also forces a real `TargetSelection` prompt here (mirroring
/// `u2_r4a`'s tie-breaking technique) rather than letting
/// `auto_select_targets` silently resolve a single-candidate slot.
#[test]
fn g2_target_opponent_leader_scopes_to_opponents_not_all_players() {
    let mut scenario = GameScenario::new_n_player(3, 107);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .with_life(P0, 30)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 20);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Opponent Draw Instant",
            true,
            TARGET_OPPONENT_LEADER_DRAW,
        )
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect(
            "casting the spell must be accepted with a non-empty legal target set — \
             REVERT-FAIL: under the pre-fix `PlayerRelation::All` population, no opponent \
             has life >= the caster's 30, so this cast would be rejected for lack of a \
             legal target",
        );
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "expected WaitingFor::TargetSelection (the P1/P2 tie among opponents must force \
             a real prompt), got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "exactly one target slot expected");
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Player(P1)),
        "P1 (tied life leader AMONG P0's opponents) must be a legal target, got {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Player(PlayerId(2))),
        "P2 (tied life leader AMONG P0's opponents) must be a legal target, got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Player(P0)),
        "P0 is the caster, never a legal target for an opponent-headed target filter, \
         got {legal:?}"
    );
}

/// U2-R5 — the `HandSize` axis end-to-end (Sokenzan Renegade), inline
/// Bushido keyword. Same leader-is-controller shape as R1: the
/// `unimplemented_oracle_ids` check does NOT discriminate a revert of U2
/// (see the F1 fix,
/// `f1_unimplemented_oracle_ids_not_recorded_for_bare_top_level_unimplemented`
/// — a bare top-level `Effect::Unimplemented` never populates the set, so
/// it stays empty either way). Kept as a same-shape sanity check; `u2_r5b`
/// below is the genuine revert-discriminating coverage for this axis.
#[test]
fn u2_r5_sokenzan_renegade_hand_axis_resolves_cleanly() {
    let mut scenario = upkeep_scenario(3, 106);
    let card = scenario
        .add_creature(P0, "Sokenzan Renegade", 3, 2)
        .from_oracle_text_with_keywords(&["Bushido"], SOKENZAN_RENEGADE)
        .id();
    scenario
        .with_cards_in_hand(P0, &["Card A1", "Card A2", "Card A3"])
        .with_cards_in_hand(P1, &["Card B1"])
        .with_cards_in_hand(PlayerId(2), &["Card C1"]);
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&card].controller, P0);
    assert!(
        runner.state().unimplemented_oracle_ids.is_empty(),
        "in the shipped path Sokenzan Renegade's subject does not lower to \
         Effect::Unimplemented, so the set should stay empty (NOT \
         revert-discriminating — see the F1 fix); got {:?}",
        runner.state().unimplemented_oracle_ids
    );
}

/// U2-R6 — the LIFE axis end-to-end via Wild Dogs, inline Cycling keyword.
/// Pins that the inline Cycling line does not derail the upkeep line's
/// parse. Same leader-is-controller shape as R1: the
/// `unimplemented_oracle_ids` check does NOT discriminate a revert of U2
/// (see the F1 fix,
/// `f1_unimplemented_oracle_ids_not_recorded_for_bare_top_level_unimplemented`).
/// Kept as a same-shape sanity check; `u2_r2` is the genuine
/// revert-discriminating coverage for the life axis.
#[test]
fn u2_r6_wild_dogs_life_axis_resolves_cleanly() {
    let mut scenario = upkeep_scenario(3, 107);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(PlayerId(2), 10);
    let card = scenario
        .add_creature(P0, "Wild Dogs", 3, 3)
        .from_oracle_text_with_keywords(&["Cycling"], WILD_DOGS)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&card].controller, P0);
    assert!(
        runner.state().unimplemented_oracle_ids.is_empty(),
        "in the shipped path Wild Dogs' subject does not lower to \
         Effect::Unimplemented (and the inline Cycling line must not derail the \
         upkeep line's parse), so the set should stay empty (NOT \
         revert-discriminating — see the F1 fix); got {:?}",
        runner.state().unimplemented_oracle_ids
    );
}

/// SHAPE — subfamily-B decline path stays honestly red: the property `alt`
/// has no `creatures` arm, so this clause must remain
/// `Effect::Unimplemented{"unbound_subject"}`, never a silently-wrong
/// binding. Parser-shape-only is acceptable here per the `/card-test`
/// recipe: the semantics stay genuinely unsupported (red), not a runtime
/// behavior claim.
#[test]
fn subfamily_b_object_count_noun_declines_honestly() {
    let parsed = parse_oracle_text(
        SUBFAMILY_B_DECLINE_LINE,
        "Subfamily-B Decline Test Card",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .into_iter()
        .next()
        .expect("the phase trigger itself must still parse");
    let execute = trigger
        .execute
        .expect("the trigger must carry an execute ability, even an Unimplemented one");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::Unimplemented { name, .. } if name == "unbound_subject"
        ),
        "the object-count noun 'creatures' must decline honestly as unbound_subject, \
         got {:?}",
        execute.effect
    );
}

/// F1 — RUNTIME CONTROL for the `unimplemented_oracle_ids` discriminator
/// used (as a secondary check) by `u2_r1`/`u2_r5`/`u2_r6` above. Those rows
/// assert `unimplemented_oracle_ids.is_empty()`, which would ALSO pass
/// vacuously if the upkeep trigger never fired, never resolved, or an
/// inline keyword derailed the parse so no trigger existed at all.
/// `subfamily_b_object_count_noun_declines_honestly` above proves
/// `SUBFAMILY_B_DECLINE_LINE` parses to
/// `Effect::Unimplemented{"unbound_subject"}`, but only asserts parse SHAPE
/// — it never executes the trigger. This row drives the same shape through
/// the SAME runtime harness the `is_empty()` rows use (`upkeep_scenario` +
/// `advance_until_stack_empty`) to check what the instrument actually does.
///
/// MEASURED RESULT (not the outcome originally expected): `state.stack`
/// carries the triggered ability with `execute.effect ==
/// Effect::Unimplemented{"unbound_subject"}` immediately after
/// `advance_to_upkeep` (confirmed via direct inspection), and the stack
/// drains to empty via `advance_until_stack_empty` — but
/// `unimplemented_oracle_ids` stays EMPTY. The reason is
/// `game/stack.rs::execute_effect`: for a stack entry whose OWN
/// `ability.effect` is `Effect::Unimplemented` (no `sub_ability` chain), it
/// returns immediately ("Skip unimplemented effects (logged elsewhere as
/// warnings)") without ever calling `resolve_ability_chain` /
/// `effects::resolve_effect` — so the latter's `Effect::Unimplemented`
/// recording arm (`game/effects/mod.rs`) is never reached. That skip is
/// PRE-EXISTING baseline behavior (untouched by this branch), not
/// introduced by U1/U2, and out of scope to change here — it is a broad,
/// unmeasured-blast-radius engine change, not a parser/test fix.
///
/// CONSEQUENCE: `unimplemented_oracle_ids.is_empty()` does **not**
/// discriminate a revert of U2 on `u2_r1`/`u2_r5`/`u2_r6` — a reverted U2
/// would lower those clauses to exactly this bare top-level
/// `Effect::Unimplemented` shape, which (per this measurement) never
/// populates the set either. Those three rows' `unimplemented_oracle_ids`
/// checks are corrected below to say so; their REAL revert-discriminating
/// coverage is the final-controller assertions in the sibling
/// leader-not-controller rows (`u2_r2` for the life axis;
/// `u2_r5b`, added below, for the hand-size axis, which had no such row
/// before this fix).
#[test]
fn f1_unimplemented_oracle_ids_not_recorded_for_bare_top_level_unimplemented() {
    let mut scenario = upkeep_scenario(3, 109);
    let card = scenario
        .add_creature(P0, "Subfamily-B Decline Test Card", 2, 2)
        .from_oracle_text(SUBFAMILY_B_DECLINE_LINE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_upkeep();

    // The post-drain assertion below is an ABSENCE check, and absence is also
    // what you get when the trigger never fires at all. Pin the antecedent
    // first: the ability really does reach the stack carrying the bare
    // top-level `Effect::Unimplemented`, so the empty set measured afterwards
    // is the skip in `game/stack.rs::execute_effect` and not a no-show.
    //
    // Measured non-vacuous: drop the `advance_to_upkeep()` above so the
    // trigger never reaches the stack, and this pair fails with `got []`
    // (left 0, right 1) while the post-drain absence check still passes.
    // That difference is exactly the gap these two assertions close.
    let staged: Vec<Effect> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if *source_id == card => Some(ability.effect.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        staged.len(),
        1,
        "expected exactly one triggered ability from this source on the stack before draining; got {staged:?}"
    );
    assert!(
        matches!(&staged[0], Effect::Unimplemented { name, .. } if name == "unbound_subject"),
        "the staged ability must carry the bare top-level Effect::Unimplemented with name \
         \"unbound_subject\" that this test is about; got {:?}",
        staged[0]
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().unimplemented_oracle_ids.is_empty(),
        "MEASURED (not originally expected): a bare top-level \
         Effect::Unimplemented ability is skipped by \
         game/stack.rs::execute_effect before effects::resolve_effect's \
         recording arm is ever reached, so unimplemented_oracle_ids stays \
         empty even though the trigger genuinely fired and resolved; got {:?}",
        runner.state().unimplemented_oracle_ids
    );
}

/// U2-R5b — HAND-SIZE AXIS, LEADER NOT CONTROLLER (Sokenzan Renegade). The
/// mirror of `u2_r2` (life axis) for the hand-size axis, added by the F1 fix:
/// `u2_r5` alone (leader IS controller, P0) cannot discriminate a revert of
/// U2, because both the final-controller observable (P0 either way) AND the
/// `unimplemented_oracle_ids` check (proven non-discriminating above) hold
/// regardless of whether U2 is present. This row's final-controller
/// observable is genuinely revert-discriminating: reverted, U2's clause
/// lowers to `Effect::Unimplemented`, no control move happens, and control
/// stays with the controller P0; fixed, control moves to the unique
/// hand-size leader P1.
#[test]
fn u2_r5b_sokenzan_renegade_leader_not_controller_control_moves_to_leader() {
    let mut scenario = upkeep_scenario(3, 110);
    let card = scenario
        .add_creature(P0, "Sokenzan Renegade", 3, 2)
        .from_oracle_text_with_keywords(&["Bushido"], SOKENZAN_RENEGADE)
        .id();
    scenario
        .with_cards_in_hand(P0, &["Card A1"])
        .with_cards_in_hand(P1, &["Card B1", "Card B2", "Card B3"])
        .with_cards_in_hand(PlayerId(2), &["Card C1"]);
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&card].controller,
        P1,
        "control must move to the unique hand-size leader P1, not stay with the \
         controller P0"
    );
}

/// F7 (HOSTILE) — ELIMINATED PLAYER, HAND-SIZE AXIS: pins that
/// `resolve_per_player_scalar`'s `AllPlayers` arm (`game/quantity.rs`)
/// excludes eliminated players from the population via `!p.is_eliminated`,
/// not just the `exclude` anchor. Without that filter, an eliminated
/// player's larger hand would inflate the population `Max` above every LIVE
/// candidate's hand size, so `player_property_leader_filter`'s
/// `PlayerAttribute` predicate (`candidate's hand size >= population Max`)
/// would match NO live player. The failure surfaces at the CONDITION, not at
/// the recipient: U1's intervening-if reads that same inflated
/// `HandSize{AllPlayers{Max}}` as its threshold, so its `PlayerCount` folds to
/// 0 and the condition is FALSE at the CR 603.4 FIRE-TIME check, so the
/// ability never triggers and never reaches the stack;
/// `unique_recipient_from_filter` (`game/effects/gain_control.rs`) is never
/// reached. Measured with the guard reverted, by `eprintln` probes on
/// `triggers::check_trigger_condition_with_source` (printing its result plus a
/// captured backtrace) and on `unique_recipient_from_filter`: the condition is
/// evaluated exactly ONCE, returning `false`, from
/// `collect_matching_triggers_inner` ← `collect_matching_triggers` ←
/// `collect_pending_triggers_with_overlay`, and the
/// `unique_recipient_from_filter` probe prints nothing. With the guard
/// restored, the same probes print `true` three times and reach
/// `unique_recipient_from_filter` exactly once.
///
/// Board: P0 (controller) has 1 card; P1 is ELIMINATED holding 5 cards (would
/// "lead" if counted); P2 (live) has 3 cards — the unique LIVE leader.
/// Expected: control moves to P2. The two axes are guarded SEPARATELY, not by
/// one shared filter: `QuantityRef::HandSize` resolves through
/// `resolve_per_player_scalar`, whose guard this fixture pins, while
/// `QuantityRef::LifeTotal` never reaches that function — it dispatches to
/// `resolve_per_team_life` / `team_life_total` (CR 810.9a team folding). The
/// LIFE-axis sibling
/// `hostile_eliminated_player_life_axis_excluded_from_population`
/// (`unique_player_property_leader_condition.rs`) covers the
/// `resolve_per_team_life` path.
///
/// The two guards are NOT symmetric in what they pin, and this row's sibling
/// is not the life guard's regression pin. Measured with
/// `cargo test -p phase-engine --no-fail-fast` under a revert of
/// `resolve_per_team_life`'s filters: NO integration fixture fails (7076
/// passed, 0 failed) and every other target is green — that guard's only
/// failing test anywhere is the lib unit test
/// `game::quantity::tests::life_total_min_excludes_eliminated_player_from_population`.
/// See the life-axis fixture's own doc block for the four reverts behind that.
#[test]
fn f7_hostile_eliminated_player_hand_axis_leader_still_wins() {
    let mut scenario = upkeep_scenario(3, 111);
    let card = scenario
        .add_creature(P0, "Sokenzan Renegade", 3, 2)
        .from_oracle_text_with_keywords(&["Bushido"], SOKENZAN_RENEGADE)
        .id();
    scenario
        .with_cards_in_hand(P0, &["Card A1"])
        .with_cards_in_hand(P1, &["Card B1", "Card B2", "Card B3", "Card B4", "Card B5"])
        .with_cards_in_hand(PlayerId(2), &["Card C1", "Card C2", "Card C3"]);
    let mut runner = scenario.build();
    runner.state_mut().players[1].is_eliminated = true;
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&card].controller,
        PlayerId(2),
        "REVERT-FAIL (F7): with P1 eliminated, control must still move to the live \
         hand-size leader P2 (3 cards); eliminated P1 (5 cards) must not count \
         toward the population max"
    );
}
