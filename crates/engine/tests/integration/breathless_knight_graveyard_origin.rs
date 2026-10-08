//! Breathless Knight (DSK) — graveyard-origin intervening-if on an enters
//! trigger that watches the source AND other creatures.
//!
//! > Flying, lifelink
//! > Whenever this creature or another creature you control enters, if that
//! > creature entered from a graveyard or you cast it from a graveyard, put a
//! > +1/+1 counter on this creature.
//!
//! CR 603.4 — the intervening-if is checked when the ability triggers and on
//! resolution. CR 400.3 + CR 404.1 — "a graveyard" is ANY graveyard (the entered
//! arm is owner-unscoped), while "you cast it" scopes the caster only.
//!
//! Field report: the clause was swallowed (`SwallowedClause` /
//! `unparsed_condition`), so the Knight grew on EVERY creature entry. The
//! existing graveyard-origin combinator (Prized Amalgam, Twilight Diviner)
//! knew neither the "that creature" anaphor nor the split "a graveyard" form.
//!
//! Rows: parse fidelity, then the real cast/trigger pipeline — hand cast (no),
//! reanimation (yes), cast from a graveyard (yes), the Knight itself cast from
//! hand (no).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, TargetFilter, TriggerCondition};
use engine::types::counter::CounterType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

// Verbatim Oracle text (Scryfall, 2026-09-19).
const BREATHLESS_KNIGHT: &str = "Flying, lifelink\n\
Whenever this creature or another creature you control enters, if that creature entered from a \
graveyard or you cast it from a graveyard, put a +1/+1 counter on this creature.";

/// A vanilla {0} reanimation sorcery: the creature is PUT onto the battlefield
/// from the graveyard, never cast.
const REANIMATE: &str = "Return target creature card from your graveyard to the battlefield.";

/// A creature that grants itself permission to be cast from the graveyard.
const GRAVE_CASTER: &str = "You may cast this card from your graveyard.";

fn knight_counters(state: &engine::types::game_state::GameState, knight: ObjectId) -> u32 {
    state.objects[&knight]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn parses_that_creature_split_a_graveyard_form() {
    let types = vec!["Creature".to_string()];
    let parsed = parse_oracle_text(BREATHLESS_KNIGHT, "Breathless Knight", &[], &types, &[]);
    let condition = parsed
        .triggers
        .first()
        .and_then(|t| t.condition.clone())
        .expect("the graveyard-origin intervening-if must be parsed, not swallowed");
    assert_eq!(
        condition,
        TriggerCondition::Or {
            conditions: vec![
                TriggerCondition::ZoneChangeObjectMatchesFilter {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    filter: TargetFilter::Any,
                },
                TriggerCondition::WasCast {
                    zone: Some(Zone::Graveyard),
                    controller: Some(ControllerRef::You),
                    owner: None,
                },
            ],
        }
    );
}

fn scenario_with_knight() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario
        .add_creature_from_oracle(P0, "Breathless Knight", 2, 1, BREATHLESS_KNIGHT)
        .id();
    (scenario, knight)
}

/// The reported bug: a creature cast from hand grew the Knight.
#[test]
fn creature_cast_from_hand_does_not_grow_the_knight() {
    let (mut scenario, knight) = scenario_with_knight();
    let mut bear = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    bear.with_mana_cost(ManaCost::generic(0));
    let bear = bear.id();
    let mut runner = scenario.build();

    let out = runner.cast(bear).resolve();
    assert_eq!(
        out.zone_of(bear),
        Zone::Battlefield,
        "reach-guard: the creature entered"
    );
    assert_eq!(
        knight_counters(out.state(), knight),
        0,
        "cast from hand is not a graveyard origin"
    );
}

/// Positive pair of the row above: the same creature REANIMATED grows the Knight.
#[test]
fn reanimated_creature_grows_the_knight() {
    let (mut scenario, knight) = scenario_with_knight();
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
        "reach-guard: the creature was reanimated"
    );
    assert_eq!(
        knight_counters(out.state(), knight),
        1,
        "entered from a graveyard → +1/+1 counter"
    );
}

/// The cast arm: a creature cast FROM a graveyard grows the Knight.
#[test]
fn creature_cast_from_graveyard_grows_the_knight() {
    let (mut scenario, knight) = scenario_with_knight();
    let mut ghoul = scenario.add_creature_to_graveyard(P0, "Grave Caster", 2, 2);
    ghoul.from_oracle_text(GRAVE_CASTER);
    ghoul.with_mana_cost(ManaCost::generic(0));
    let ghoul = ghoul.id();
    let mut runner = scenario.build();

    let out = runner.cast(ghoul).resolve();
    assert_eq!(
        out.zone_of(ghoul),
        Zone::Battlefield,
        "reach-guard: the creature was cast from the graveyard and resolved"
    );
    assert_eq!(
        knight_counters(out.state(), knight),
        1,
        "you cast it from a graveyard → +1/+1 counter"
    );
}

/// The `SelfRef` arm of the trigger: the Knight itself cast from hand.
#[test]
fn knight_cast_from_hand_does_not_grow_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut knight =
        scenario.add_creature_to_hand_from_oracle(P0, "Breathless Knight", 2, 1, BREATHLESS_KNIGHT);
    knight.with_mana_cost(ManaCost::generic(0));
    let knight = knight.id();
    let mut runner = scenario.build();

    let out = runner.cast(knight).resolve();
    assert_eq!(
        out.zone_of(knight),
        Zone::Battlefield,
        "reach-guard: the Knight entered"
    );
    assert_eq!(knight_counters(out.state(), knight), 0);
}

/// Positive self-source pair: a synthetic Knight fixture adds graveyard-cast
/// permission while preserving the printed intervening-if and trigger effect.
#[test]
fn knight_cast_from_graveyard_grows_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oracle = format!("{BREATHLESS_KNIGHT}\n{GRAVE_CASTER}");
    let mut knight = scenario.add_creature_to_graveyard(P0, "Breathless Knight", 2, 1);
    knight.from_oracle_text(&oracle);
    knight.with_mana_cost(ManaCost::generic(0));
    let knight = knight.id();
    let mut runner = scenario.build();

    let out = runner.cast(knight).resolve();
    assert_eq!(out.zone_of(knight), Zone::Battlefield);
    assert_eq!(knight_counters(out.state(), knight), 1);
}

/// CR 400.3 + CR 404.1: the trigger's "a graveyard" includes an opponent's.
/// Synthetic reanimation supplies control separately from the card's owner.
#[test]
fn opponent_owned_creature_reanimated_under_your_control_grows_knight() {
    let (mut scenario, knight) = scenario_with_knight();
    let creature = scenario
        .add_creature_to_graveyard(P1, "Opponent's Creature", 2, 2)
        .id();
    let mut reanimate = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Cross-Owner Return",
        false,
        "Return target creature card from a graveyard to the battlefield under your control.",
    );
    reanimate.with_mana_cost(ManaCost::generic(0));
    let reanimate = reanimate.id();
    let mut runner = scenario.build();
    let out = runner.cast(reanimate).target_object(creature).resolve();
    assert_eq!(out.zone_of(creature), Zone::Battlefield);
    assert_eq!(out.state().objects[&creature].owner, P1);
    assert_eq!(out.state().objects[&creature].controller, P0);
    assert_eq!(knight_counters(out.state(), knight), 1);
}
