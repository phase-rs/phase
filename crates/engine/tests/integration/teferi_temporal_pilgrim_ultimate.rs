//! Teferi, Temporal Pilgrim −12 (issue #9847): "Target opponent chooses a
//! permanent they control and returns it to its owner's hand. Then they
//! shuffle each nonland permanent they control into its owner's library."
//!
//! The opponent's resolution-time choice is not lowered yet, so the clauses are
//! explicit gaps (see `teferi_minus_twelve_resolution_choice_is_explicitly_unsupported`
//! in the parser). The defect #9847 reported was the opposite: the activator's
//! own permanents were bounced and shuffled away. CR 608.2c: "they" is the
//! targeted opponent, never the activator, so the −12 must not move the
//! activator's permanents.

use engine::game::scenario::{P0, P1};
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::mana::ManaColor;
use engine::types::zones::Zone;

use super::controller_anaphor_binds_through_context::{
    main_phase_scenario, settle_recording_offers,
};

const TEFERI: &str = "Whenever you draw a card, put a loyalty counter on Teferi.\n0: Draw a card.\n−2: Create a 2/2 blue Spirit creature token with vigilance and \"Whenever you draw a card, put a +1/+1 counter on this token.\"\n−12: Target opponent chooses a permanent they control and returns it to its owner's hand. Then they shuffle each nonland permanent they control into its owner's library.";

/// Activating the −12 pays its loyalty cost and leaves the activator's
/// permanents on the battlefield, whatever the opponent's own permanents do.
#[test]
fn ultimate_never_moves_the_activators_permanents() {
    let mut scenario = main_phase_scenario(2);
    let walker = scenario
        .add_planeswalker_from_oracle(P0, "Teferi, Temporal Pilgrim", "Teferi", 13, TEFERI)
        .id();
    let my_creature = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let my_land = scenario.add_basic_land(P0, ManaColor::Blue);
    let their_creature = scenario.add_creature(P1, "P1 Ox", 2, 2).id();
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&walker]
        .abilities
        .iter()
        .position(|a| matches!(a.cost, Some(AbilityCost::Loyalty { amount: -12 })))
        .expect("the -12 loyalty ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: walker,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate -12");
    settle_recording_offers(
        &mut runner,
        &[TargetRef::Player(P1), TargetRef::Object(their_creature)],
        &[],
    );

    let state = runner.state();
    assert_eq!(
        state.objects[&walker]
            .counters
            .get(&engine::types::counter::CounterType::Loyalty),
        Some(&1),
        "the -12 cost was paid, so the ability did activate and resolve"
    );
    assert_eq!(state.objects[&my_creature].zone, Zone::Battlefield);
    assert_eq!(state.objects[&my_land].zone, Zone::Battlefield);
}
