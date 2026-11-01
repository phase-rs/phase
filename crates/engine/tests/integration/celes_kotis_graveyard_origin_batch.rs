//! Celes, Rune Knight / Kotis, Sibsig Champion — the batch graveyard-origin
//! intervening-if.
//!
//! Verbatim Oracle text (Scryfall, 2026-10-01):
//!   Celes: "When Celes enters, discard any number of cards, then draw that many
//!     cards plus one. Whenever one or more other creatures you control enter,
//!     if one or more of them entered from a graveyard or was cast from a
//!     graveyard, put a +1/+1 counter on each creature you control."
//!   Kotis: "Whenever one or more creatures you control enter, if one or more
//!     of them entered from a graveyard or was cast from a graveyard, put two
//!     +1/+1 counters on Kotis."
//!
//! CR 603.2c — one event may contain multiple occurrences; the batch anaphor
//! "one or more of them" binds the entering events of ONE trigger event, not a
//! global set. CR 603.4 — the intervening-if is checked when the ability
//! triggers (per candidate) and again on resolution. CR 400.3 + CR 404.1 — "a
//! graveyard" is any graveyard; the passive split form carries no caster
//! clause.
//!
//! Rows drive the real pipeline: `move_to_zone` + `process_triggers` for the
//! simultaneous batch (mixed hand/graveyard entrants, hand entrant FIRST), and
//! `GameRunner::cast` for the reanimation / graveyard-cast legs.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CELES: &str =
    "When Celes enters, discard any number of cards, then draw that many cards plus one.\n\
Whenever one or more other creatures you control enter, if one or more of them entered from a \
graveyard or was cast from a graveyard, put a +1/+1 counter on each creature you control.";

const KOTIS: &str = "Once during each of your turns, you may cast a creature spell from your graveyard by exiling three other cards from your graveyard in addition to paying its other costs.\n\
Whenever one or more creatures you control enter, if one or more of them entered from a \
graveyard or was cast from a graveyard, put two +1/+1 counters on Kotis.";

/// A vanilla {0} reanimation sorcery: the creature is PUT onto the battlefield
/// from the graveyard, never cast.
const REANIMATE: &str = "Return target creature card from your graveyard to the battlefield.";

/// A creature that grants itself permission to be cast from the graveyard.
const GRAVE_CASTER: &str = "You may cast this card from your graveyard.";

fn counters(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn trigger_entries(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count()
}

/// CR 603.2c: one simultaneous batch with the HAND entrant FIRST and the
/// graveyard entrant second fires once, narrowed to the graveyard candidate —
/// a first-candidate-only binding would store the hand entrant instead.
#[test]
fn mixed_batch_hand_entrant_first_narrows_to_graveyard_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let hand_entrant = scenario.add_creature_to_hand(P0, "Hand Entrant", 2, 2).id();
    let gy_entrant = scenario
        .add_creature_to_graveyard(P0, "Graveyard Entrant", 2, 2)
        .id();
    let mut runner = scenario.build();

    // One simultaneous event batch: both entrants move to the battlefield.
    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        hand_entrant,
        Zone::Battlefield,
        &mut events,
    );
    move_to_zone(
        runner.state_mut(),
        gy_entrant,
        Zone::Battlefield,
        &mut events,
    );
    assert_eq!(
        runner.state().objects[&hand_entrant].zone,
        Zone::Battlefield,
        "reach guard: the hand entrant is on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&gy_entrant].zone,
        Zone::Battlefield,
        "reach guard: the graveyard entrant is on the battlefield"
    );

    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());

    assert_eq!(
        trigger_entries(&runner, celes),
        1,
        "positive reach guard: exactly one batched Celes trigger is on the stack"
    );
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == celes)
        .expect("the Celes trigger entry exists");
    let StackEntryKind::TriggeredAbility { trigger_event, .. } = &entry.kind else {
        panic!("expected a triggered ability, got {:?}", entry.kind);
    };
    let Some(GameEvent::ZoneChanged { object_id, .. }) = trigger_event else {
        panic!("expected the narrowed ZoneChanged event, got {trigger_event:?}");
    };
    assert_eq!(
        *object_id, gy_entrant,
        "the stored candidate must be the graveyard entrant, not the first (hand) entrant"
    );
    assert!(
        runner.state().stack_trigger_event_batches.is_empty(),
        "a single surviving candidate must not produce a multi-event batch entry"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        counters(runner.state(), hand_entrant),
        1,
        "reach guard: the resolved Celes effect pumps the team"
    );
    assert_eq!(counters(runner.state(), gy_entrant), 1);
}

/// Positive reach row: a reanimated creature (entered from a graveyard) pumps
/// the team.
#[test]
fn graveyard_origin_entry_pumps_the_team() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut reanimate = scenario.add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE);
    reanimate.with_mana_cost(ManaCost::generic(0));
    let reanimate = reanimate.id();
    let mut runner = scenario.build();

    let out = runner.cast(reanimate).target_object(bear).resolve();
    assert_eq!(
        out.zone_of(bear),
        Zone::Battlefield,
        "reach guard: the creature was reanimated"
    );
    assert_eq!(
        counters(out.state(), celes),
        1,
        "entered from a graveyard → the team gets a +1/+1 counter"
    );
    assert_eq!(counters(out.state(), bear), 1);
}

/// A batch with no qualifying member does not fire: a hand entrant alone
/// (put onto the battlefield, never cast) leaves the ability untriggered, while
/// the entrant itself proves the entry happened.
#[test]
fn hand_entry_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let hand_entrant = scenario.add_creature_to_hand(P0, "Hand Entrant", 2, 2).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        hand_entrant,
        Zone::Battlefield,
        &mut events,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    assert_eq!(
        runner.state().objects[&hand_entrant].zone,
        Zone::Battlefield,
        "reach guard: the entrant is on the battlefield"
    );
    assert_eq!(
        trigger_entries(&runner, celes),
        0,
        "a hand entry is neither a graveyard origin nor a graveyard cast"
    );
    assert_eq!(counters(runner.state(), celes), 0);
}

/// The cast-from-hand sibling of the row above, through the real cast
/// pipeline.
#[test]
fn none_qualify_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let mut bear = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    bear.with_mana_cost(ManaCost::generic(0));
    let bear = bear.id();
    let mut runner = scenario.build();

    let out = runner.cast(bear).resolve();
    assert_eq!(
        out.zone_of(bear),
        Zone::Battlefield,
        "reach guard: the creature resolved onto the battlefield"
    );
    assert_eq!(
        counters(out.state(), celes),
        0,
        "cast from hand is not a graveyard origin"
    );
}

/// The `WasCast` leg: a creature cast FROM a graveyard fires the ability.
#[test]
fn cast_from_graveyard_entry_fires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let mut ghoul = scenario.add_creature_to_graveyard(P0, "Grave Caster", 2, 2);
    ghoul.from_oracle_text(GRAVE_CASTER);
    ghoul.with_mana_cost(ManaCost::generic(0));
    let ghoul = ghoul.id();
    let mut runner = scenario.build();

    let out = runner.cast(ghoul).resolve();
    assert_eq!(
        out.zone_of(ghoul),
        Zone::Battlefield,
        "reach guard: the creature was cast from the graveyard and resolved"
    );
    assert_eq!(
        counters(out.state(), celes),
        1,
        "you cast it from a graveyard → the team gets a +1/+1 counter"
    );
}

/// No leakage across batches: an earlier graveyard batch must not make a later
/// hand entry fire; the counter count is flat across the second entry.
#[test]
fn earlier_graveyard_batch_does_not_leak_into_later_hand_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celes = scenario
        .add_creature_from_oracle(P0, "Celes, Rune Knight", 3, 4, CELES)
        .id();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let hand_entrant = scenario.add_creature_to_hand(P0, "Hand Entrant", 2, 2).id();
    let mut reanimate = scenario.add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE);
    reanimate.with_mana_cost(ManaCost::generic(0));
    let reanimate = reanimate.id();
    let mut runner = scenario.build();

    let out = runner.cast(reanimate).target_object(bear).resolve();
    assert_eq!(out.zone_of(bear), Zone::Battlefield);
    let after_graveyard_batch = counters(out.state(), celes);
    assert_eq!(
        after_graveyard_batch, 1,
        "reach guard: the first (graveyard) batch's counters are asserted first"
    );

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        hand_entrant,
        Zone::Battlefield,
        &mut events,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&hand_entrant].zone,
        Zone::Battlefield,
        "reach guard: the hand entrant entered"
    );
    assert_eq!(
        counters(runner.state(), celes),
        after_graveyard_batch,
        "a prior graveyard batch must not leak into a later hand entry"
    );
}

/// Kotis's exact effect (two counters on Kotis) runs off the same binding.
#[test]
fn kotis_batch_places_two_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kotis = scenario
        .add_creature_from_oracle(P0, "Kotis, Sibsig Champion", 3, 3, KOTIS)
        .id();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut reanimate = scenario.add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE);
    reanimate.with_mana_cost(ManaCost::generic(0));
    let reanimate = reanimate.id();
    let mut runner = scenario.build();

    let out = runner.cast(reanimate).target_object(bear).resolve();
    assert_eq!(
        out.zone_of(bear),
        Zone::Battlefield,
        "reach guard: the creature was reanimated"
    );
    assert_eq!(
        counters(out.state(), kotis),
        2,
        "Kotis's graveyard-origin batch places two +1/+1 counters"
    );
}
