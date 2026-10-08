//! CR 604.1 + CR 601.2f regression (runtime cast pipeline): a "During turns other
//! than yours, spells you cast cost {N} less" reducer (Geyser Drake, Naiad of
//! Hidden Coves) must reduce the controller's spells ONLY on turns that are not
//! the controller's — the leading negated-turn clause maps to
//! `StaticCondition::Not(DuringYourTurn)`.
//!
//! Before the fix, the leading "During turns other than yours," clause was not
//! recognized by the cost-modifier scope stripper, so the static parsed with
//! `condition: None` and the reducer applied on EVERY turn (including the
//! controller's own). CR 102.1: the active player is the player whose turn it is;
//! the gate is evaluated against the source permanent's controller.
//!
//! Drives the real cost pipeline: `GameAction::CastSpell` computes the
//! battlefield-modified cost at `WaitingFor::TargetSelection` (before payment),
//! so `pending_cast.cost.mana_value()` reads the actual reduction the resolver
//! applied. On revert the reduction applies on the controller's own turn too and
//! the "not reduced on your turn" assertion (mv == 2) fails.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::StaticCondition;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const GEYSER: &str = "During turns other than yours, spells you cast cost {1} less to cast.";
const HURKYLS_FINAL_MEDITATION: &str =
    "During turns other than yours, this spell costs {3} more to cast.";

/// Begin casting P0's {2}-generic instant and return the total mana value of the
/// battlefield-modified cost the engine resolved (surfaced at `TargetSelection`
/// before payment). No reduction → 2; the {1} reduction → 1.
fn resolved_cost_mana_value(runner: &mut GameRunner, spell_id: ObjectId) -> u32 {
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the instant should begin (cost is checked at payment, not here)");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.cost.mana_value(),
        other => panic!("expected TargetSelection after casting the instant, got {other:?}"),
    }
}

fn scenario_with_geyser() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0
    scenario
        .add_creature(P0, "Geyser Source", 2, 2)
        .with_static_definition(parse_static_line(GEYSER).expect("Geyser static should parse"));
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Blast",
            true,
            "Test Blast deals 3 damage to any target.",
        )
        .with_mana_cost(ManaCost::generic(2))
        .id();
    (scenario, spell_id)
}

/// Cast the real Hurkyl's Final Meditation self-cost static from hand and read
/// the locked pending-cast cost at the mana-payment boundary. This is the
/// production path that requires the static's self-spell active zones; a
/// battlefield-only generic reducer cannot exercise it.
fn hurkyls_pending_cost_mana_value(active_player: PlayerId) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Hurkyl's Final Meditation",
            true,
            HURKYLS_FINAL_MEDITATION,
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
        })
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = active_player;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&spell_id].card_id;
    // Manual payment mode surfaces `WaitingFor::ManaPayment` and pauses at the
    // locked-cost boundary WITHOUT auto-tapping; the pending cast retains the
    // battlefield-modified cost so we can read its mana value before payment.
    // Auto mode would instead try to pay immediately and fail here because this
    // scenario seeds no mana sources ("Cannot pay mana cost").
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("casting Hurkyl's Final Meditation should begin");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .state()
        .pending_cast
        .as_ref()
        .expect("ManaPayment must retain the pending Hurkyl's cast")
        .cost
        .mana_value()
}

#[test]
fn geyser_static_parses_with_not_during_your_turn_condition() {
    // CR 604.1: the leading "During turns other than yours," clause must lower to
    // `Not(DuringYourTurn)` — not be silently dropped (which left condition: None).
    let def = parse_static_line(GEYSER).expect("Geyser static should parse");
    assert_eq!(
        def.condition,
        Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        }),
        "\"During turns other than yours,\" must gate the reducer, got {:?}",
        def.condition,
    );
}

#[test]
fn geyser_does_not_reduce_on_its_controllers_own_turn() {
    // On P0's OWN turn, Not(DuringYourTurn) is false → no reduction → full {2}.
    // Before the fix the dropped gate reduced it on every turn (mv would be 1).
    let (scenario, spell_id) = scenario_with_geyser();
    let mut runner = scenario.build();
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        2,
        "on the controller's own turn the reducer is OFF; the {{2}} spell stays {{2}}",
    );
}

#[test]
fn geyser_reduces_during_turns_other_than_the_controllers() {
    // On P1's turn, Not(DuringYourTurn) is true → the {2} spell is reduced to {1}.
    let (scenario, spell_id) = scenario_with_geyser();
    let mut runner = scenario.build();
    // Move to P1's turn and hand P0 priority to cast its instant.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        1,
        "during a turn that is not the controller's, the {{2}} spell is reduced to {{1}}",
    );
}

/// CR 601.2f: Hurkyl's Final Meditation is a real self-spell cost modifier,
/// not a battlefield reducer. Its `SelfRef` static must be active from hand
/// while the engine locks the pending cast cost.
#[test]
fn hurkyls_final_meditation_self_cost_tracks_controller_turn() {
    assert_eq!(
        hurkyls_pending_cost_mana_value(P0),
        7,
        "on its controller's turn, Hurkyl's keeps its printed {{4}}{{U}}{{U}}{{U}} cost",
    );
    assert_eq!(
        hurkyls_pending_cost_mana_value(P1),
        10,
        "on another player's turn, Hurkyl's self modifier adds {{3}} to the pending cast cost",
    );
}
