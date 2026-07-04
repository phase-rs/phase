//! Issue #3264 — Jace, the Mind Sculptor's −12 must exile the targeted player's
//! library and shuffle their hand into that library. Battlefield permanents must
//! stay in play.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const JACE_ORACLE: &str = "\
+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n\
0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\
−1: Return target creature to its owner's hand.\n\
−12: Exile all cards from target player's library, then that player shuffles their hand into their library.";

#[test]
fn jace_minus_twelve_exiles_library_and_shuffles_hand_without_touching_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let jace = scenario
        .add_creature(P0, "Jace, the Mind Sculptor", 0, 0)
        .from_oracle_text(JACE_ORACLE)
        .id();

    let opp_creature = scenario.add_vanilla(P1, 2, 2);
    let caster_creature = scenario.add_vanilla(P0, 3, 3);
    let opp_lib_top = scenario.add_card_to_library_top(P1, "Island");
    let opp_lib_bottom = scenario.add_card_to_library_top(P1, "Forest");
    let opp_hand = scenario.add_card_to_hand(P1, "Shock");

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&jace).expect("jace");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.loyalty = Some(20);
        obj.counters.insert(CounterType::Loyalty, 20);
    }

    let outcome = runner.activate(jace, 3).target_player(P1).resolve();

    let state = outcome.state();

    assert_eq!(
        state.objects[&opp_creature].zone,
        Zone::Battlefield,
        "opponent battlefield permanent must remain"
    );
    assert_eq!(
        state.objects[&caster_creature].zone,
        Zone::Battlefield,
        "caster battlefield permanent must remain"
    );
    assert_eq!(
        state.objects[&jace].zone,
        Zone::Battlefield,
        "Jace must remain on the battlefield"
    );

    assert_eq!(
        state.objects[&opp_lib_top].zone,
        Zone::Exile,
        "opponent library card must be exiled"
    );
    assert_eq!(
        state.objects[&opp_lib_bottom].zone,
        Zone::Exile,
        "opponent library card must be exiled"
    );
    assert_eq!(
        state.objects[&opp_hand].zone,
        Zone::Library,
        "opponent hand card must be shuffled into library"
    );
    assert!(
        state.players[1].library.contains(&opp_hand),
        "shuffled hand card must be in opponent library zone list"
    );
}
