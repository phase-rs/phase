//! Issue #5282 — Nissa, Who Shakes the World's [−8] ultimate must both create
//! an emblem AND let you search your library for Forest cards.
//!
//! The ultimate reads:
//!   "You get an emblem with \"Lands you control have indestructible.\" Search
//!    your library for any number of Forest cards, put them onto the battlefield
//!    tapped, then shuffle."
//!
//! The emblem's granted static ends in a sentence-final close quote
//! (`indestructible."`). The clause splitter did not treat that close quote as a
//! sentence boundary, so the following "Search your library …" sentence was
//! glued onto the emblem clause and swallowed into the emblem's static text —
//! the ability never produced an `Effect::SearchLibrary`, so activating the
//! ultimate resolved without searching the library (the reported bug).
//!
//! This pins the fix: an emblem's sentence-ending close quote closes the
//! sentence, so the sibling search clause parses on its own and surfaces a
//! `SearchChoice` at resolution (CR 701.23).

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const NISSA_ORACLE: &str = "\
Whenever you tap a Forest for mana, add an additional {G}.\n\
[+1]: Put three +1/+1 counters on up to one target noncreature land you control. Untap it. It becomes a 0/0 Elemental creature with vigilance and haste that's still a land.\n\
[−8]: You get an emblem with \"Lands you control have indestructible.\" Search your library for any number of Forest cards, put them onto the battlefield tapped, then shuffle.";

#[test]
fn nissa_who_shakes_the_world_ultimate_searches_library_for_forests() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let nissa = scenario
        .add_creature(P0, "Nissa, Who Shakes the World", 0, 0)
        .from_oracle_text(NISSA_ORACLE)
        .id();

    // A Forest in the library gives the "any number of Forest cards" search a
    // legal card to find.
    let forest = scenario.add_card_to_library_top(P0, "Forest");

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // CR 306.5b: a planeswalker's loyalty is its loyalty-counter count; seed
        // 8 so the [−8] (CR 606.6: can't drop loyalty below 0) is legal.
        let obj = state.objects.get_mut(&nissa).expect("nissa");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.loyalty = Some(8);
        obj.counters.insert(CounterType::Loyalty, 8);

        // Give the library card real Forest characteristics so the
        // Typed[Land, Subtype(Forest)] search filter matches it.
        let land = state.objects.get_mut(&forest).expect("forest");
        land.card_types.core_types = vec![CoreType::Land];
        land.card_types.subtypes = vec!["Forest".to_string()];
        land.base_card_types = land.card_types.clone();
    }

    // Activate [−8]: ability index 1 ([+1] is index 0; the "Whenever you tap a
    // Forest for mana" line is a triggered ability, not an activated one).
    let outcome = runner.activate(nissa, 1).resolve();

    // CR 701.23: The ultimate's second sentence is a library search. Before the
    // fix it was swallowed into the emblem's static text and no search ran, so
    // the chain resolved straight to a priority window. With the fix the
    // `Effect::SearchLibrary` resolves and the driver pauses on the interactive
    // `SearchChoice` for the controller.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::SearchChoice { player, .. } if *player == P0
        ),
        "Nissa's [−8] must search the library (surface SearchChoice); got {:?}",
        outcome.final_waiting_for()
    );
}
