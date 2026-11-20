//! Production-path regression for Break of Day's cross-line Fateful hour rider.
//!
//! Oracle text (verified against Scryfall and MTGJSON):
//!   "Creatures you control get +1/+1 until end of turn.
//!    Fateful hour — If you have 5 or less life, those creatures gain
//!    indestructible until end of turn."
//!
//! CR 207.2c: "Fateful hour" is an ability word with no rules meaning of its
//! own; the conditional rider is still a second instruction in the spell's
//! document. CR 608.2c requires the instructions to be followed in order, and
//! CR 611.2a makes both continuous effects last until end of turn.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const BREAK_OF_DAY: &str = "Creatures you control get +1/+1 until end of turn.\nFateful hour — If you have 5 or less life, those creatures gain indestructible until end of turn.";

fn cast_break_of_day(
    life: i32,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, life);
    let first = scenario.add_creature(P0, "First creature", 2, 2).id();
    let _second = scenario.add_creature(P0, "Second creature", 3, 1).id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Break of Day", false, BREAK_OF_DAY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&spell]
            .abilities
            .iter()
            .any(ability_contains_unimplemented),
        "Break of Day must parse without Effect::Unimplemented residue: {:?}",
        runner.state().objects[&spell].abilities
    );

    runner.cast(spell).resolve();
    (runner, first, opponent)
}

fn ability_contains_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
}

/// CR 608.2c + CR 611.2a: at exactly five life, the ability-word rider
/// remains attached to the first instruction and grants indestructible to the
/// same controller-scoped creature population.
#[test]
fn break_of_day_grants_both_effects_at_fateful_hour_threshold() {
    let (runner, creature, opponent) = cast_break_of_day(5);

    assert_eq!(runner.state().objects[&creature].power, Some(3));
    assert_eq!(runner.state().objects[&creature].toughness, Some(3));
    assert!(runner.state().objects[&creature].has_keyword(&Keyword::Indestructible));
    assert!(!runner.state().objects[&opponent].has_keyword(&Keyword::Indestructible));
}

/// Six life does not satisfy the card's printed "5 or less" condition; the
/// unconditional pump still resolves, but the conditional keyword grant does
/// not.
#[test]
fn break_of_day_omits_rider_above_fateful_hour_threshold() {
    let (runner, creature, opponent) = cast_break_of_day(6);

    assert_eq!(runner.state().objects[&creature].power, Some(3));
    assert_eq!(runner.state().objects[&creature].toughness, Some(3));
    assert!(!runner.state().objects[&creature].has_keyword(&Keyword::Indestructible));
    assert!(!runner.state().objects[&opponent].has_keyword(&Keyword::Indestructible));
}
