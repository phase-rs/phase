//! Issue #10195 (origin half): "is exiled from [zone]" must restrict the
//! trigger to that source zone.
//!
//! CR 603.10a: a "leaves the battlefield" trigger looks back at the event (CR
//! 603.6c defines leaves-the-battlefield abilities); when
//! the text names the source zone ("is exiled from the battlefield"), an exile
//! from any other zone is not the trigger event. The parser arm used to drop
//! the whole ` from <zone>` tail, so Soulherder and Captain Marvel, Shooting
//! Star fired on an exile from any zone; Market Gnome parsed the same
//! `origin: null`, but its self-referential trigger then functioned only on
//! the battlefield, so only its own battlefield exile reached it.
//!
//! The "while you're activating a craft ability" qualifier has no AST carrier
//! yet (it needs a serialized `TriggerConstraint`, i.e. a protocol bump) and is
//! tracked in #10630. This change's contract is the origin only: the qualifier
//! stays dropped exactly as before, so a non-craft battlefield exile still fires
//! (disclosed residual, not a regression).

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const SWORDS: &str = "Exile target creature. Its controller gains life equal to its power.";
const CLING_TO_DUST: &str =
    "Exile target card from a graveyard. If it was a creature card, you gain 3 life. Otherwise, you draw a card.";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn in_hand(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    is_instant: bool,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, is_instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// R1-negative: Soulherder does not fire when a creature is exiled from a
/// graveyard (CR 603.6c).
#[test]
fn r1_soulherder_does_not_fire_on_a_graveyard_exile() {
    let mut scenario = board();
    let soulherder = scenario.add_real_card(P0, "Soulherder", Zone::Battlefield, db());
    let grave_card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db());
    let cling = in_hand(&mut scenario, P0, "Cling to Dust", true, CLING_TO_DUST);
    let mut runner = scenario.build();

    runner.cast(cling).target_object(grave_card).resolve();

    assert_eq!(
        runner.state().objects[&grave_card].zone,
        Zone::Exile,
        "reach guard: the graveyard card was exiled"
    );
    assert_eq!(
        p1p1(&runner, soulherder),
        0,
        "CR 603.6c: a graveyard exile is not 'exiled from the battlefield'"
    );
}

/// R1-positive: the same card fires when the exile is from the battlefield.
#[test]
fn r1_soulherder_fires_on_a_battlefield_exile() {
    let mut scenario = board();
    let soulherder = scenario.add_real_card(P0, "Soulherder", Zone::Battlefield, db());
    let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    let swords = in_hand(&mut scenario, P0, "Swords to Plowshares", true, SWORDS);
    let mut runner = scenario.build();

    runner.cast(swords).target_object(victim).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "reach guard: the battlefield creature was exiled"
    );
    assert_eq!(
        p1p1(&runner, soulherder),
        1,
        "CR 603.6c: a battlefield exile is the trigger event"
    );
}

/// R1-token: a creature token exiled from the battlefield still triggers
/// Soulherder exactly once. CR 603.6c: the leaves-the-battlefield event happens
/// when the token moves to exile; CR 111.7: the token only ceases to exist
/// afterwards, as a state-based action, so the trigger is not lost.
///
/// The token body is a textless vanilla creature (no Oracle text to parse), so
/// the repo's real-cards rule does not apply to it; Soulherder and Swords to
/// Plowshares keep their real Oracle text.
#[test]
fn r1_soulherder_fires_when_a_token_is_exiled_from_the_battlefield() {
    let mut scenario = board();
    let soulherder = scenario.add_real_card(P0, "Soulherder", Zone::Battlefield, db());
    let token = scenario.add_creature(P0, "Zombie", 2, 2).id();
    let swords = in_hand(&mut scenario, P0, "Swords to Plowshares", true, SWORDS);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    let outcome = runner.cast(swords).target_object(token).resolve();
    let exiled_from_battlefield = outcome.events().iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if *object_id == token
        )
    });

    assert!(
        exiled_from_battlefield,
        "reach guard: the token moved from the battlefield to exile"
    );
    assert!(
        outcome
            .state()
            .objects
            .get(&token)
            .is_none_or(|object| object.zone != Zone::Battlefield),
        "reach guard: the token is no longer on the battlefield"
    );
    assert_eq!(
        p1p1(&runner, soulherder),
        1,
        "CR 603.6c + CR 111.7: the token's battlefield exile triggers Soulherder \
         once, before the token ceases to exist"
    );
}
