//! Issue #832 — Combat Celebrant exert-as-attack.
//!
//! Oracle: "If this creature hasn't been exerted this turn, you may exert it as
//! it attacks. When you do, untap all other creatures you control and after
//! this phase, there is an additional combat phase."
//!
//! Before the fix the parser produced a `TriggerMode::Exerted` trigger, but the
//! engine never offered the optional exert cost when attackers were declared
//! (CR 508.1g) and `TriggerMode::Exerted` was routed to `match_unimplemented`,
//! so the ability did nothing. These tests drive the real combat pipeline
//! through `apply`: declare attackers → exert prompt → choice → linked trigger →
//! stack resolution.
//!
//! CR 508.1g (optional attack costs paid "as" a creature attacks), CR 701.43a
//! (exert = won't untap during your next untap step), CR 701.43d (the linked
//! "when you do" trigger).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const COMBAT_CELEBRANT: &str = "If this creature hasn't been exerted this turn, \
you may exert it as it attacks. When you do, untap all other creatures you \
control and after this phase, there is an additional combat phase.";

/// CR 508.1g + CR 701.43d: declaring an exert-capable attacker offers the exert
/// choice; paying it fires the linked trigger, which schedules the additional
/// combat phase.
#[test]
fn exert_offered_and_fires_extra_combat_when_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celebrant = scenario
        .add_creature_from_oracle(P0, "Combat Celebrant", 4, 1, COMBAT_CELEBRANT)
        .id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(celebrant, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    assert_eq!(
        runner.waiting_for_kind(),
        "ExertChoice",
        "declaring an exert-capable attacker must offer the optional exert cost"
    );

    runner
        .act(GameAction::ChooseExert { exert: true })
        .expect("ChooseExert should succeed");
    runner.advance_until_stack_empty();

    // CR 701.43a: the creature is recorded as exerted this turn.
    assert!(
        runner.state().exerted_this_turn.contains(&celebrant),
        "exerting must record the creature in exerted_this_turn"
    );
    // CR 701.43d: the linked "when you do" trigger resolved and scheduled the
    // additional combat phase.
    assert!(
        !runner.state().extra_phases.is_empty(),
        "exerting Combat Celebrant must schedule an additional combat phase"
    );
}

/// CR 508.1g: the exert cost is optional — declining leaves the creature
/// un-exerted and fires no linked trigger.
#[test]
fn declining_exert_does_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celebrant = scenario
        .add_creature_from_oracle(P0, "Combat Celebrant", 4, 1, COMBAT_CELEBRANT)
        .id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(celebrant, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    assert_eq!(runner.waiting_for_kind(), "ExertChoice");

    runner
        .act(GameAction::ChooseExert { exert: false })
        .expect("ChooseExert should succeed");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().exerted_this_turn.contains(&celebrant),
        "declining must not record the creature as exerted"
    );
    assert!(
        runner.state().extra_phases.is_empty(),
        "declining the exert cost must not schedule an additional combat phase"
    );
}

/// A vanilla attacker has no exert-as-attack ability, so declaration proceeds
/// without an exert prompt.
#[test]
fn non_exert_attacker_is_not_prompted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(bear, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    assert_ne!(
        runner.waiting_for_kind(),
        "ExertChoice",
        "a vanilla attacker must not trigger the exert prompt"
    );
}
