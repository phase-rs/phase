//! Kotis, Sibsig Champion (and Celes, Rune Knight) — batched graveyard-origin
//! intervening-if: "one or more of them entered from a graveyard or was cast
//! from a graveyard".
//!
//! > Whenever one or more creatures you control enter, if one or more of them
//! > entered from a graveyard or was cast from a graveyard, put two +1/+1
//! > counters on Kotis.
//!
//! CR 603.4 — intervening-if; CR 603.2c — one trigger per batch of
//! simultaneous entries; CR 603.6a — enters-the-battlefield triggers check
//! permanents against the entry event.
//!
//! The clause was swallowed (`unparsed_condition`), so Kotis grew on EVERY
//! creature entry. Rows: parse fidelity, hand cast (no), reanimation (yes),
//! cast from graveyard (yes), batch (once), opponent's entrant (no), token (no).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TriggerCondition};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

// Verbatim Oracle text (Scryfall / MTGJSON card data).
const KOTIS: &str = "Once during each of your turns, you may cast a creature spell from your graveyard by exiling three other cards from your graveyard in addition to paying its other costs.\n\
Whenever one or more creatures you control enter, if one or more of them entered from a graveyard or was cast from a graveyard, put two +1/+1 counters on Kotis.";

const REANIMATE: &str = "Return target creature card from your graveyard to the battlefield.";
const REANIMATE_ALL: &str = "Return all creature cards from your graveyard to the battlefield.";
const GRAVE_CASTER: &str = "You may cast this card from your graveyard.";

fn counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn scenario_with_kotis() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kotis = scenario
        .add_creature_from_oracle(P0, "Kotis, Sibsig Champion", 3, 3, KOTIS)
        .id();
    (scenario, kotis)
}

#[test]
fn parses_one_or_more_of_them_graveyard_origin() {
    let types = vec!["Creature".to_string()];
    let parsed = parse_oracle_text(KOTIS, "Kotis, Sibsig Champion", &[], &types, &[]);
    let trigger = parsed.triggers.first().expect("Kotis trigger");
    assert!(trigger.batched);
    let Some(TriggerCondition::Or { conditions }) = &trigger.condition else {
        panic!(
            "intervening-if must be parsed, not swallowed: {:?}",
            trigger.condition
        );
    };
    assert_eq!(conditions.len(), 2);
    assert!(matches!(
        conditions[0],
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            filter: TargetFilter::Any,
        }
    ));
    assert!(matches!(
        conditions[1],
        TriggerCondition::WasCast {
            zone: Some(Zone::Graveyard),
            controller: None,
            owner: None,
        }
    ));
    let execute = trigger.execute.as_deref().expect("Kotis body");
    assert!(matches!(&*execute.effect, Effect::PutCounter { .. }));
}

/// The reported bug: a creature cast from hand grew Kotis.
#[test]
fn creature_cast_from_hand_does_not_grow_kotis() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let mut bear = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    bear.with_mana_cost(ManaCost::generic(0));
    let bear = bear.id();
    let mut runner = scenario.build();

    let out = runner.cast(bear).resolve();
    assert_eq!(out.zone_of(bear), Zone::Battlefield, "reach-guard");
    assert_eq!(counters(out.state(), kotis), 0);
}

/// Positive pair: the same creature reanimated gives exactly two counters.
#[test]
fn reanimated_creature_grows_kotis_by_two() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut spell = scenario.add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE);
    spell.with_mana_cost(ManaCost::generic(0));
    let spell = spell.id();
    let mut runner = scenario.build();

    let out = runner.cast(spell).target_object(bear).resolve();
    assert_eq!(out.zone_of(bear), Zone::Battlefield, "reach-guard");
    assert_eq!(counters(out.state(), kotis), 2);
}

/// The cast leg: entry is from the stack, so only `WasCast` can satisfy it.
#[test]
fn creature_cast_from_graveyard_grows_kotis_by_two() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let mut ghoul = scenario.add_creature_to_graveyard(P0, "Grave Caster", 2, 2);
    ghoul.from_oracle_text(GRAVE_CASTER);
    ghoul.with_mana_cost(ManaCost::generic(0));
    let ghoul = ghoul.id();
    let mut runner = scenario.build();

    let out = runner.cast(ghoul).resolve();
    assert_eq!(out.zone_of(ghoul), Zone::Battlefield, "reach-guard");
    assert_eq!(counters(out.state(), kotis), 2);
}

/// CR 603.2c: two creatures entering from the graveyard at once are one
/// batch, so one trigger: two counters, not four.
#[test]
fn simultaneous_graveyard_entries_trigger_once() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let a = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let b = scenario
        .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
        .id();
    let mut spell =
        scenario.add_spell_to_hand_from_oracle(P0, "Reanimate All", false, REANIMATE_ALL);
    spell.with_mana_cost(ManaCost::generic(0));
    let spell = spell.id();
    let mut runner = scenario.build();

    let out = runner.cast(spell).resolve();
    assert_eq!(out.zone_of(a), Zone::Battlefield, "reach-guard");
    assert_eq!(out.zone_of(b), Zone::Battlefield, "reach-guard");
    assert_eq!(counters(out.state(), kotis), 2);
}

/// "creatures you control": an opponent-controlled entrant from a graveyard
/// does not trigger Kotis. Positive control: `reanimated_creature_grows_kotis_by_two`.
#[test]
fn opponents_creature_entering_from_graveyard_does_not_grow_kotis() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let theirs = scenario
        .add_creature_to_graveyard(P1, "Opponent's Creature", 2, 2)
        .id();
    let mut spell = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Owner Return",
        false,
        "Return target creature card from a graveyard to the battlefield under its owner's control.",
    );
    spell.with_mana_cost(ManaCost::generic(0));
    let spell = spell.id();
    let mut runner = scenario.build();

    let out = runner.cast(spell).target_object(theirs).resolve();
    assert_eq!(out.zone_of(theirs), Zone::Battlefield, "reach-guard");
    assert_eq!(out.state().objects[&theirs].controller, P1, "reach-guard");
    assert_eq!(counters(out.state(), kotis), 0);
}

/// A token has no origin zone and was not cast: no trigger.
#[test]
fn token_entry_does_not_grow_kotis() {
    let (mut scenario, kotis) = scenario_with_kotis();
    let mut spell = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Make Soldier",
        false,
        "Create a 1/1 white Soldier creature token.",
    );
    spell.with_mana_cost(ManaCost::generic(0));
    let spell = spell.id();
    let mut runner = scenario.build();

    let out = runner.cast(spell).resolve();
    let tokens = out
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .count();
    assert_eq!(tokens, 1, "reach-guard: the token entered");
    assert_eq!(counters(out.state(), kotis), 0);
}
