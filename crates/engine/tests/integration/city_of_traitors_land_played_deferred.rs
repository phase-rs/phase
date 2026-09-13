//! Issue #8738 — a "when you play another land" trigger must fire when the
//! played land's battlefield entry pauses on an "As it enters, choose …"
//! replacement (City of Traitors watching Cavern of Souls).
//!
//! Root cause: the deferred-entry replay that restores ETB observers across an
//! as-enters choice (issue #830 / PR #3167) carried only the entering
//! permanent's `ZoneChanged` event, not the sibling `GameEvent::LandPlayed`
//! emitted by the land-play finalizer. Because the entry paused on
//! `WaitingFor::NamedChoice` instead of `Priority`, the canonical priority-time
//! trigger collection was skipped and the `LandPlayed` occurrence never reached
//! `process_triggers` — so City of Traitors never saw the Cavern of Souls play.
//!
//! These tests drive the REAL apply() pipeline (play land → resolve as-enters
//! choice → resolve triggers off the stack), not a hand-built state.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

// Oracle text (from card-data.json) — CR 305.1 + CR 603.2: `LandPlayed` trigger
// with a self-sacrifice payoff.
const CITY_OF_TRAITORS: &str = "When you play another land, sacrifice this land.\n{T}: Add {C}{C}.";

// Oracle text (from card-data.json) — the as-enters creature-type choice pauses
// the entry on `WaitingFor::NamedChoice`.
const CAVERN_OF_SOULS: &str = "As this land enters, choose a creature type.\n{T}: Add {C}.\n\
     {T}: Add one mana of any color. Spend this mana only to cast a creature spell of the chosen \
     type, and that spell can't be countered.";

/// Discriminating bug repro (#8738): with City of Traitors on the battlefield,
/// playing Cavern of Souls (as-enters creature-type choice) and answering the
/// choice MUST fire City's `LandPlayed` trigger and sacrifice City. Fails
/// before the fix — the deferred entry replay drops the `LandPlayed` event, so
/// City survives the Cavern play.
#[test]
fn city_of_traitors_sacrifices_after_as_enters_choice_land() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // City of Traitors on P0's battlefield.
    let city = scenario
        .add_land_from_oracle(P0, "City of Traitors", CITY_OF_TRAITORS)
        .id();

    // Cavern of Souls in P0's hand — its entry pauses on the creature-type
    // choice.
    let cavern = {
        let mut b = scenario.add_land_to_hand(P0, "Cavern of Souls");
        b.from_oracle_text(CAVERN_OF_SOULS);
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&cavern).unwrap().card_id;

    // Play the land — its entry pauses on the as-enters creature-type choice.
    runner
        .act(GameAction::PlayLand {
            object_id: cavern,
            card_id,
        })
        .expect("play Cavern of Souls");

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "as-enters land must pause on the creature-type choice, got {}",
            runner.waiting_for_kind()
        );
    };
    let creature_type = options.first().expect("creature-type options").clone();

    // Answer the creature type — this is where the deferred `LandPlayed` event
    // must replay and fire City of Traitors' trigger.
    runner
        .act(GameAction::ChooseOption {
            choice: creature_type,
        })
        .expect("choose the creature type");

    // Resolve the now-stacked City trigger.
    runner.advance_until_stack_empty();

    let city_zone = runner
        .state()
        .objects
        .get(&city)
        .map(|o| o.zone)
        .expect("City of Traitors still tracked");
    assert_eq!(
        city_zone,
        Zone::Graveyard,
        "City of Traitors must sacrifice itself when Cavern of Souls (an as-enters-\
         choice land) is played (#8738); got zone {city_zone:?}"
    );
}

/// Control case: a plain basic-land play (no as-enters choice) still fires
/// City's `LandPlayed` trigger through the ordinary priority-time trigger
/// collection. Proves the fix does not regress the already-working path and
/// that City fires via both routes.
#[test]
fn city_of_traitors_sacrifices_after_plain_land() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let city = scenario
        .add_land_from_oracle(P0, "City of Traitors", CITY_OF_TRAITORS)
        .id();

    // A plain land in P0's hand (no replacement — resolves to `Priority`).
    let island = scenario.add_land_to_hand(P0, "Island").id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&island).unwrap().card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: island,
            card_id,
        })
        .expect("play the basic land");

    // No choice pauses a basic land, so the trigger stacks and resolves.
    runner.advance_until_stack_empty();

    let city_zone = runner
        .state()
        .objects
        .get(&city)
        .map(|o| o.zone)
        .expect("City of Traitors still tracked");
    assert_eq!(
        city_zone,
        Zone::Graveyard,
        "City of Traitors must sacrifice itself after a plain land play (#8738 control); \
         got zone {city_zone:?}"
    );
}
