//! Overflow Submatrix (TRK) — once-per-turn artifact creature graveyard cast
//! with a linked finality-counter rider.
//!
//! Oracle text (verified verbatim against `client/public/card-data.json`):
//! "Once during each of your turns, you may cast an artifact creature spell
//! from your graveyard. If you cast a spell this way, that artifact creature
//! enters with a finality counter on it. (If a creature with a finality counter
//! on it would die, exile it instead.)"
//!
//! CR 601.3: the static is the permission to cast from the graveyard.
//! CR 102.1: "during each of your turns" limits it to its controller's turns.
//! CR 205.2b: "artifact creature" is a conjunctive type stack — the spell must
//! be both.
//! CR 122.1h + CR 614.1c: the finality counter is placed only on the permanent
//! a spell cast through this permission becomes.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{CardPlayMode, Effect, StaticCondition, TargetFilter, TypeFilter};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

const OVERFLOW_SUBMATRIX: &str = "Once during each of your turns, you may cast an artifact creature spell from your graveyard. If you cast a spell this way, that artifact creature enters with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";

fn submatrix_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Overflow Submatrix", 3, 3)
        .as_artifact_creature()
        .from_oracle_text(OVERFLOW_SUBMATRIX);
    scenario
}

fn free_artifact_creature_in_graveyard(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, name, 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn offered(runner: &engine::game::scenario::GameRunner, card: ObjectId) -> bool {
    let on_offer = spell_objects_available_to_cast(runner.state(), P0).contains(&card);
    assert_eq!(
        on_offer,
        can_cast_object_now(runner.state(), P0, card),
        "offer list and can_cast_object_now must agree for {card:?}"
    );
    on_offer
}

/// Parse: one `GraveyardCastPermission { OncePerTurn, Cast, finality }` whose
/// `affected` requires BOTH artifact and creature, gated to your turn, with no
/// unimplemented residue.
#[test]
fn overflow_submatrix_parses_to_once_per_turn_finality_permission() {
    let parsed = parse_oracle_text(
        OVERFLOW_SUBMATRIX,
        "Overflow Submatrix",
        &[],
        &["Artifact".to_string(), "Creature".to_string()],
        &["Borg".to_string()],
    );
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "no unimplemented residue: {:?}",
        parsed.abilities
    );
    let [def] = parsed.statics.as_slice() else {
        panic!("expected exactly one static, got {:?}", parsed.statics);
    };
    let StaticMode::GraveyardCastPermission {
        frequency,
        play_mode,
        enters_with_counter,
        graveyard_destination_replacement,
        extra_cost,
        ..
    } = &def.mode
    else {
        panic!("expected GraveyardCastPermission, got {:?}", def.mode);
    };
    assert_eq!(*frequency, CastFrequency::OncePerTurn);
    assert_eq!(*play_mode, CardPlayMode::Cast);
    assert_eq!(*enters_with_counter, Some(CounterType::Finality));
    assert_eq!(*graveyard_destination_replacement, None);
    assert_eq!(*extra_cost, None);
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    let Some(TargetFilter::Typed(tf)) = &def.affected else {
        panic!("expected a typed affected filter, got {:?}", def.affected);
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Artifact)
            && tf.type_filters.contains(&TypeFilter::Creature),
        "\"artifact creature\" must require both types, got {:?}",
        tf.type_filters
    );
}

/// Runtime: on your turn an artifact creature card in the graveyard is offered,
/// a plain creature and a noncreature artifact are not; the cast resolves onto
/// the battlefield with a finality counter; the slot is then spent for the
/// turn, so a second artifact creature card is no longer offered.
#[test]
fn overflow_submatrix_casts_one_artifact_creature_per_turn_with_finality() {
    let mut scenario = submatrix_scenario();
    let first = free_artifact_creature_in_graveyard(&mut scenario, "Graveyard Golem");
    let second = free_artifact_creature_in_graveyard(&mut scenario, "Second Golem");
    let plain_creature = scenario
        .add_creature_to_graveyard(P0, "Plain Bear", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let noncreature_artifact = scenario
        .add_creature_to_graveyard(P0, "Plain Relic", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    assert!(offered(&runner, first), "artifact creature card is offered");
    assert!(offered(&runner, second), "reach: both candidates offered");
    assert!(
        !offered(&runner, plain_creature),
        "a non-artifact creature card is not offered"
    );
    assert!(
        !offered(&runner, noncreature_artifact),
        "a noncreature artifact card is not offered"
    );

    let outcome = runner.cast(first).resolve();
    assert_eq!(outcome.zone_of(first), Zone::Battlefield);
    assert_eq!(
        outcome.counters(first, CounterType::Finality),
        1,
        "the artifact creature cast this way enters with a finality counter"
    );

    assert!(
        !offered(&runner, second),
        "\"once during each of your turns\": the slot is spent for this turn"
    );
}

/// Runtime: the permission is not offered on the opponent's turn.
#[test]
fn overflow_submatrix_is_limited_to_your_turns() {
    let mut scenario = submatrix_scenario();
    let golem = free_artifact_creature_in_graveyard(&mut scenario, "Graveyard Golem");
    let mut runner = scenario.build();
    assert!(offered(&runner, golem), "reach: offered on my own turn");
    runner.state_mut().active_player = PlayerId(1);
    assert!(
        !can_cast_object_now(runner.state(), P0, golem),
        "not castable on the opponent's turn"
    );
}

/// Runtime: the same artifact creature cast from hand enters with no finality
/// counter — the rider belongs to the permission, not to the card.
#[test]
fn overflow_submatrix_hand_cast_gets_no_finality_counter() {
    let mut scenario = submatrix_scenario();
    let golem = scenario
        .add_creature_to_hand(P0, "Hand Golem", 2, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(golem).resolve();
    assert_eq!(outcome.zone_of(golem), Zone::Battlefield);
    assert_eq!(outcome.counters(golem, CounterType::Finality), 0);
}
