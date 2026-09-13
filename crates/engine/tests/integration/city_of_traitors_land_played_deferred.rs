//! Issue #8738 — a "when you play another land" trigger must fire when the
//! played land's battlefield entry pauses on ANY of the three land-entry
//! continuation shapes (City of Traitors watching the played land):
//!
//! * the as-enters "As it enters, choose …" replacement (Cavern of Souls) — the
//!   post-replacement drain route, deferred into `deferred_entry_events` by
//!   `capture_deferred_entry_events_if_mid_entry_choice` and replayed via
//!   `replay_deferred_entry_events`;
//! * the pre-entry shock/payment replacement (Stomping Ground "you may pay 2
//!   life") — `ReplacementResult::NeedsChoice`;
//! * the delivery-tail counter-order choice (two non-commuting counter modifiers
//!   on an "enters with an additional counter" static) —
//!   `ZoneDeliveryResult::NeedsChoice`.
//!
//! Root cause: the land-play finalizer (`finalize_committed_land_play`) emits the
//! `GameEvent::LandPlayed` occurrence into the action's `events`, but any pause
//! that hands back a non-`Priority` waiting state drops that vec before the
//! priority-time trigger collection (`run_post_action_pipeline`) can scan it. The
//! original as-enters fix (issue #830 / PR #3167) carried only the entering
//! permanent's `ZoneChanged`, not the sibling `LandPlayed`, so City of Traitors
//! never saw the Cavern of Souls play — and the two sibling continuations were
//! left unpreserved. The fix parks the `LandPlayed` occurrence in
//! `deferred_entry_events` at each pause and flushes it into the resume's
//! `events` once the entry completes.
//!
//! These tests drive the REAL apply() pipeline (play land → resolve the entry
//! choice → resolve triggers off the stack), not a hand-built state.

use engine::game::scenario::GameScenario;
use engine::types::ability::{
    ControllerRef, QuantityModification, ReplacementDefinition, StaticDefinition, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
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

/// Oracle text (from card-data.json) — the shock-land "As this land enters, you
/// may pay 2 life. If you don't, it enters tapped" replacement pauses the entry
/// on `WaitingFor::ReplacementChoice` BEFORE the land enters (the pre-entry
/// `ReplacementResult::NeedsChoice` continuation).
const STOMPING_GROUND: &str =
    "({T}: Add {R} or {G}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";

/// Discriminating bug repro (#8738): a shock/payment land play pauses on a
/// pre-entry `ReplacementChoice`, which drops the `LandPlayed` occurrence unless
/// it is parked and flushed after the entry. With City of Traitors on the
/// battlefield, playing Stomping Ground and accepting the life payment MUST fire
/// City's `LandPlayed` trigger and sacrifice City.
#[test]
fn city_of_traitors_sacrifices_after_shock_land() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let city = scenario
        .add_land_from_oracle(P0, "City of Traitors", CITY_OF_TRAITORS)
        .id();

    let shock = {
        let mut b = scenario.add_land_to_hand(P0, "Stomping Ground");
        b.from_oracle_text(STOMPING_GROUND);
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&shock).unwrap().card_id;

    // Play the shock land — its pre-entry payment choice parks the entry.
    runner
        .act(GameAction::PlayLand {
            object_id: shock,
            card_id,
        })
        .expect("play the shock land");

    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "shock land must pause on the pay-life replacement choice, got {}",
            runner.waiting_for_kind()
        );
    };

    // Accept the payment (index 0 = accept, index 1 = decline). The accept/decline
    // split is an optional-branch shape, so accepting is the non-decline index.
    let pick = candidates
        .iter()
        .position(|c| c.description != "Decline")
        .unwrap_or(0);
    runner
        .act(GameAction::ChooseReplacement { index: pick })
        .expect("accept the shock payment");

    // The land has now entered; resolve the stacked City trigger.
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
        "City of Traitors must sacrifice itself when a shock land is played (#8738); \
         got zone {city_zone:?}"
    );
}

/// Discriminating bug repro (#8738): a delivery-tail counter-order pause (two
/// non-commuting counter modifiers forcing a CR 616.1 ordering choice on an
/// "enters with an additional counter" static) drops the `LandPlayed` occurrence
/// unless it is parked and flushed after the entry. With City of Traitors on the
/// battlefield, playing a land whose entry pauses on the counter-ordering choice
/// MUST fire City's `LandPlayed` trigger and sacrifice City.
#[test]
fn city_of_traitors_sacrifices_after_counter_order_land() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let city = scenario
        .add_land_from_oracle(P0, "City of Traitors", CITY_OF_TRAITORS)
        .id();

    // Two non-commuting counter modifiers force a CR 616.1 ordering choice on the
    // entering land's counter placement.
    scenario
        .add_creature(P0, "Doubling Season", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::Times { factor: 2 }),
        );
    scenario
        .add_creature(P0, "Hardened Scales", 0, 0)
        .as_enchantment()
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(QuantityModification::Plus { value: 1 }),
        );

    // A static granting "lands you control enter with an additional +1/+1 counter"
    // so the played land's entry carries a counter the two modifiers contest.
    scenario
        .add_creature(P0, "Land Counter Lord", 0, 0)
        .as_enchantment()
        .with_static_definition(
            StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::You),
            )),
        );

    let island = scenario.add_land_to_hand(P0, "Island").id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&island).unwrap().card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: island,
            card_id,
        })
        .expect("play the land");

    // The counter placement pauses on the ordering choice between the two
    // non-commuting counter modifiers.
    let WaitingFor::ReplacementChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "counter-order land must pause on the counter-ordering choice, got {}",
            runner.waiting_for_kind()
        );
    };
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("choose the counter ordering");

    // The land has now entered (with its counter); resolve the stacked City trigger.
    runner.advance_until_stack_empty();

    // Reach-guard: the entry genuinely paused on the CR 616.1 counter-ordering
    // choice and resumed through the delivery tail — the +1/+1 counter (base one,
    // then the two non-commuting modifiers) must land on the played land.
    let island_counters = runner
        .state()
        .objects
        .get(&island)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0);
    assert!(
        island_counters > 0,
        "the counter-order entry must place a +1/+1 counter on the land (reach-guard)"
    );

    let city_zone = runner
        .state()
        .objects
        .get(&city)
        .map(|o| o.zone)
        .expect("City of Traitors still tracked");
    assert_eq!(
        city_zone,
        Zone::Graveyard,
        "City of Traitors must sacrifice itself when a counter-order land is played (#8738); \
         got zone {city_zone:?}"
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
