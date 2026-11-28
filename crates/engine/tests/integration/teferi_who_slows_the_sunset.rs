//! Teferi, Who Slows the Sunset — the [+1] has three independent optional
//! target slots and applies the two tap-state instructions to their union.
//!
//! CR 115.1c + CR 115.6: each printed "up to one target" is an independent
//! target occurrence that may be declined. CR 608.2c: "the chosen permanents"
//! names the complete set announced by that instruction. CR 701.26a-b: tap and
//! untap are status changes; CR 119.3: the ability still gains 2 life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn teferi_plus_one_index(runner: &engine::game::scenario::GameRunner, teferi: ObjectId) -> usize {
    runner.state().objects[&teferi]
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: 1 })))
        .expect("Teferi's [+1] loyalty ability must be present")
}

fn activate_teferi_plus_one_with_bulk_targets(
    runner: &mut engine::game::scenario::GameRunner,
    teferi: ObjectId,
    targets: Vec<TargetRef>,
) {
    let ability_index = teferi_plus_one_index(runner, teferi);
    runner
        .act(GameAction::ActivateAbility {
            source_id: teferi,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Teferi's [+1] activation must be accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "Teferi's [+1] must expose its target-selection window"
    );
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("bulk target submission must be accepted");
    runner.advance_until_stack_empty();
}

#[test]
fn plus_one_targets_artifact_creature_and_land_and_applies_to_all_three() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let teferi = scenario.add_real_card(P0, "Teferi, Who Slows the Sunset", Zone::Battlefield, db);
    let own_artifact = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let own_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let opponent_land = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&own_artifact)
        .unwrap()
        .tapped = true;
    runner
        .state_mut()
        .objects
        .get_mut(&own_creature)
        .unwrap()
        .tapped = true;

    let ability_index = teferi_plus_one_index(&runner, teferi);

    let outcome = runner
        .activate(teferi, ability_index)
        .target_objects(&[own_artifact, own_creature, opponent_land])
        .resolve();

    assert!(!outcome.state().objects[&own_artifact].tapped);
    assert!(!outcome.state().objects[&own_creature].tapped);
    assert!(outcome.state().objects[&opponent_land].tapped);
    assert_eq!(outcome.life_delta(P0), 2);
    assert!(!matches!(
        outcome.final_waiting_for(),
        WaitingFor::TargetSelection { .. }
    ));
}

#[test]
fn plus_one_bulk_select_targets_accepts_all_three_slots() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let teferi = scenario.add_real_card(P0, "Teferi, Who Slows the Sunset", Zone::Battlefield, db);
    let artifact = scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    let creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let land = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&artifact)
        .unwrap()
        .tapped = true;
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .tapped = true;

    activate_teferi_plus_one_with_bulk_targets(
        &mut runner,
        teferi,
        vec![
            TargetRef::Object(artifact),
            TargetRef::Object(creature),
            TargetRef::Object(land),
        ],
    );

    assert!(!runner.state().objects[&artifact].tapped);
    assert!(!runner.state().objects[&creature].tapped);
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(runner.life(P0), 22);
}

#[test]
fn plus_one_bulk_select_targets_accepts_zero_targets() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let teferi = scenario.add_real_card(P0, "Teferi, Who Slows the Sunset", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Sol Ring", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();

    activate_teferi_plus_one_with_bulk_targets(&mut runner, teferi, vec![]);

    assert_eq!(runner.life(P0), 22);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn plus_one_bulk_duplicate_target_untaps_once_after_removing_one_stun_counter() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    let teferi = scenario.add_real_card(P0, "Teferi, Who Slows the Sunset", Zone::Battlefield, db);
    let artifact_creature = scenario.add_real_card(P0, "Ornithopter", Zone::Battlefield, db);
    scenario.with_counter(artifact_creature, CounterType::Stun, 1);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&artifact_creature)
        .unwrap()
        .tapped = true;

    activate_teferi_plus_one_with_bulk_targets(
        &mut runner,
        teferi,
        vec![
            TargetRef::Object(artifact_creature),
            TargetRef::Object(artifact_creature),
        ],
    );

    let object = &runner.state().objects[&artifact_creature];
    assert_eq!(
        object
            .counters
            .get(&CounterType::Stun)
            .copied()
            .unwrap_or(0),
        0
    );
    assert!(
        object.tapped,
        "one untap attempt removes the stun counter only"
    );
    assert_eq!(runner.life(P0), 22);
}

#[test]
fn duel_for_dominance_plural_reader_retains_both_targets() {
    const DUEL_FOR_DOMINANCE: &str = "Choose target creature you control and target creature you don't control. If you control three or more creatures with different powers, put a +1/+1 counter on the chosen creature you control. Then the chosen creatures fight each other.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opponent = scenario.add_creature(P1, "Opponent", 4, 10).id();
    scenario.add_creature(P0, "Coven A", 5, 5);
    scenario.add_creature(P0, "Coven B", 6, 6);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Duel for Dominance", true, DUEL_FOR_DOMINANCE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[mine, opponent])
        .resolve();

    assert_eq!(outcome.damage_marked(opponent), 3);
    assert_eq!(outcome.damage_marked(mine), 4);
}
