//! Runtime cast-pipeline tests for Alania, Divergent Storm's disjunctive
//! first-of-type intervening-if (CR 601.2a + CR 603.4).
//!
//! Oracle: "Whenever you cast a spell, if it's the first instant spell, the
//! first sorcery spell, or the first Otter spell other than Alania you've cast
//! this turn, you may have target opponent draw a card. If you do, copy that
//! spell. You may choose new targets for the copy."
//!
//! The condition lowers to `Or` of composed
//! `And(TriggeringSpellMatchesFilter(T), SpellsCastThisTurn{You,T} == 1)`
//! disjuncts. Each test drives the real pipeline (`apply()` via the scenario
//! runner) and discriminates one behavioral claim; the revert-to-red note names
//! the assertion that flips when the specific arm/recognizer/fix is reverted.
//!
//! Signal design: the cast spell is a self-life-gain ("You gain 3 life") so the
//! trigger's "target opponent draw a card" is the ONLY thing that touches the
//! opponent's hand — the opponent's hand delta is a clean fire/no-fire signal.
//! The "if you do, copy that spell" copy re-runs the life gain (+3 more), so the
//! TOTAL life delta cleanly signals whether the copy was made (6 = copied,
//! 3 = not copied), independent of which player the copy resolves for.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ALANIA: &str = "Whenever you cast a spell, if it's the first instant spell, the first sorcery spell, or the first Otter spell other than Alania you've cast this turn, you may have target opponent draw a card. If you do, copy that spell. You may choose new targets for the copy.";

/// A self-contained life-gain spell body — its resolution never touches a hand,
/// so opponent-hand deltas isolate the trigger's opponent-draw.
const GAIN: &str = "You gain 3 life.";

/// Build a scenario with Alania on P0's battlefield and deep libraries for both
/// players (so every "draw a card" resolves).
fn scenario_with_alania() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Alania, Divergent Storm", 2, 3, ALANIA);
    for i in 0..16 {
        s.add_card_to_library_top(P0, &format!("P0 Lib {i}"));
        s.add_card_to_library_top(P1, &format!("P1 Lib {i}"));
    }
    s
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn total_life(runner: &GameRunner) -> i32 {
    runner.state().players.iter().map(|p| p.life).sum()
}

fn cast_spell(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&spell)
        .expect("spell object exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
}

/// Answer Alania's trigger target (the `opponent`), the optional "you may draw"
/// (`accept`), drain ordering, pass priority, and stop at stack-empty (or any
/// prompt not modeled here).
fn drive(runner: &mut GameRunner, opponent: PlayerId, accept: bool) {
    for _ in 0..128 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(opponent))
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose target must be accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Pump only the trigger's target selection + ordering and stop at the first
/// Priority window — leaving Alania's triggered ability ON the stack, unresolved,
/// so a spell can be cast in response (used by the CR 603.4 re-check test).
fn pump_until_trigger_on_stack(runner: &mut GameRunner, opponent: PlayerId) {
    for _ in 0..64 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(opponent))
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose trigger target must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => break,
            _ => break,
        }
    }
}

/// (1) First instant with Alania out → trigger fires; accepting the optional
/// draw makes the OPPONENT draw and copies the spell (the life gain runs twice).
///
/// Revert-to-red: comment the runtime arm (`TriggeringSpellMatchesFilter`) or the
/// parser recognizer → the anchor is never satisfied / the condition never lowers
/// → the trigger does not fire → the opponent draws 0 (the `== 1` hand assertion
/// fires) and no copy is made (total life is +3, so the `== 6` assertion fires).
#[test]
fn first_instant_fires_and_copies() {
    let mut scenario = scenario_with_alania();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Bolt", true, GAIN)
        .id();
    let mut runner = scenario.build();

    let p1_before = hand_size(&runner, P1);
    let life_before = total_life(&runner);

    cast_spell(&mut runner, bolt);
    drive(&mut runner, P1, true);

    // Opponent drew from the trigger's "target opponent draw a card" (fire signal).
    assert_eq!(
        hand_size(&runner, P1) as i64 - p1_before as i64,
        1,
        "first instant must fire Alania and make the opponent draw"
    );
    // Total life +6: the spell's life gain ran twice (original + copy) — proving
    // "if you do, copy that spell" ran.
    assert_eq!(
        total_life(&runner) - life_before,
        6,
        "the spell must be copied (its life gain runs twice: original + copy)"
    );
}

/// (2) A SECOND instant the same turn does NOT fire — `SpellsCastThisTurn{Instant}`
/// is 2, so the ordinal check (`== 1`) fails.
///
/// Revert-to-red: a dropped-count / `>= 1` implementation fires on the second cast
/// too → the "no additional opponent draw" assertion fires.
#[test]
fn second_instant_same_turn_does_not_fire() {
    let mut scenario = scenario_with_alania();
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Bolt", true, GAIN)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Second Bolt", true, GAIN)
        .id();
    let mut runner = scenario.build();

    cast_spell(&mut runner, first);
    drive(&mut runner, P1, true);
    let p1_after_first = hand_size(&runner, P1);

    cast_spell(&mut runner, second);
    drive(&mut runner, P1, true);

    assert_eq!(
        hand_size(&runner, P1),
        p1_after_first,
        "the second instant this turn must NOT fire (it is not the first instant)"
    );
}

/// (3) First SORCERY fires — proves the Sorcery disjunct, not just Instant.
///
/// Revert-to-red: same as (1) — reverting the arm/recognizer drops the opponent
/// draw to 0.
#[test]
fn first_sorcery_fires() {
    let mut scenario = scenario_with_alania();
    let rite = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Rite", false, GAIN)
        .id();
    let mut runner = scenario.build();

    let p1_before = hand_size(&runner, P1);
    cast_spell(&mut runner, rite);
    drive(&mut runner, P1, true);

    assert_eq!(
        hand_size(&runner, P1) as i64 - p1_before as i64,
        1,
        "first sorcery must fire Alania and make the opponent draw"
    );
}

/// (4) Otter self-exclusion — the F3 `spell_record_matches_filter` `Named` fix.
/// An Otter spell named "Alania, Divergent Storm" is cast first (it does NOT
/// fire: the Otter disjunct's `Not(Named{~})` excludes it, and a creature is
/// neither instant nor sorcery). A subsequent DIFFERENT Otter spell IS the first
/// "Otter other than Alania" → fires. Creature spells never touch a hand, so the
/// opponent-hand delta isolates the trigger's opponent-draw.
///
/// Revert-to-red: drop the `TargetFilter::Named` arm in `spell_record_matches_filter`
/// → `Not(Named{Alania})` no-ops over the spell record → the Alania-named cast is
/// counted → the real first non-Alania Otter reads as the SECOND Otter → does not
/// fire → the "opponent drew 1" assertion fires. (Non-legendary fixtures avoid the
/// legend-rule prompt; the count keys on the recorded card name, CR 201.2.)
#[test]
fn otter_self_exclusion_named_count() {
    let mut scenario = scenario_with_alania();
    // An Otter spell that shares Alania's name — excluded from the Otter count.
    let alania_otter = scenario
        .add_creature_to_hand(P0, "Alania, Divergent Storm", 1, 1)
        .with_subtypes(vec!["Otter"])
        .id();
    // A distinct Otter spell — the first "Otter other than Alania" this turn.
    let other_otter = scenario
        .add_creature_to_hand(P0, "Riverchurn Otter", 1, 1)
        .with_subtypes(vec!["Otter"])
        .id();
    let mut runner = scenario.build();

    let p1_before = hand_size(&runner, P1);

    // Casting the Alania-named Otter does not fire (excluded / non-instant-sorcery).
    cast_spell(&mut runner, alania_otter);
    drive(&mut runner, P1, true);
    let p1_after_alania = hand_size(&runner, P1);
    assert_eq!(
        p1_after_alania, p1_before,
        "the Alania-named Otter must not fire the trigger"
    );

    cast_spell(&mut runner, other_otter);
    drive(&mut runner, P1, true);

    assert_eq!(
        hand_size(&runner, P1) - p1_after_alania,
        1,
        "the first non-Alania Otter must fire (Alania-named cast excluded from the count)"
    );
}

/// (5) CR 603.4 resolution re-check (team-lead mandate). First instant fires and
/// Alania's trigger goes on the stack; a SECOND instant is cast IN RESPONSE.
/// When the trigger resolves, the live `SpellsCastThisTurn{Instant}` is 2, so the
/// intervening-if is FALSE at resolution → the ability is removed and does
/// nothing: the opponent does NOT draw and no copy is made.
///
/// Revert-to-red: a fire-time-pinned model (one that snapshots the ordinal at fire
/// time instead of re-reading the live count) would still fire → the opponent
/// would draw 1 → the `== 0` assertion fires.
#[test]
fn cr603_4_resolution_recheck_fizzles() {
    let mut scenario = scenario_with_alania();
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Bolt", true, GAIN)
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Response Bolt", true, GAIN)
        .id();
    let mut runner = scenario.build();

    let p1_before = hand_size(&runner, P1);

    cast_spell(&mut runner, first);
    // Leave Alania's (targeted) trigger on the stack, unresolved.
    pump_until_trigger_on_stack(&mut runner, P1);
    assert!(
        runner.state().stack.len() >= 2,
        "Alania's trigger must be on the stack above the first instant before we respond"
    );

    // Cast the second instant in response — advances the live count to 2.
    cast_spell(&mut runner, response);
    drive(&mut runner, P1, true);

    assert_eq!(
        hand_size(&runner, P1) as i64 - p1_before as i64,
        0,
        "CR 603.4: the trigger must fizzle at resolution (live count == 2), no opponent draw"
    );
}

/// (6) Declining the optional "you may draw" leaves "if you do" false, so the
/// CopySpell sub does NOT resolve — no copy, and the opponent does not draw.
///
/// Revert-to-red: if the preserved `EffectOutcome{OptionalEffectPerformed}` gate
/// were dropped, the copy would resolve even on decline → total life would be +6
/// → the `== 3` assertion fires.
#[test]
fn optional_decline_skips_copy() {
    let mut scenario = scenario_with_alania();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Bolt", true, GAIN)
        .id();
    let mut runner = scenario.build();

    let p1_before = hand_size(&runner, P1);
    let life_before = total_life(&runner);

    cast_spell(&mut runner, bolt);
    drive(&mut runner, P1, false); // decline the optional draw

    assert_eq!(
        hand_size(&runner, P1),
        p1_before,
        "declining the optional draw must not make the opponent draw"
    );
    // Total life +3: only the original spell's life gain ran; NO copy.
    assert_eq!(
        total_life(&runner) - life_before,
        3,
        "declining 'you may' must skip the copy (only the original spell resolves)"
    );
}
