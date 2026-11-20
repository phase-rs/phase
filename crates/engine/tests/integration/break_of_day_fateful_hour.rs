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
use engine::types::ability_visit::ability_tree_any;
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const BREAK_OF_DAY: &str = "Creatures you control get +1/+1 until end of turn.\nFateful hour — If you have 5 or less life, those creatures gain indestructible until end of turn.";
const UNBREAKABLE_FORMATION: &str = "Creatures you control gain indestructible until end of turn.\nAddendum — If you cast this spell during your main phase, put a +1/+1 counter on each of those creatures and they gain vigilance until end of turn.";

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
    ability_tree_any(def, &|candidate| {
        matches!(candidate.effect.as_ref(), Effect::Unimplemented { .. })
    })
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

/// CR 608.2c + CR 611.2c: a later addendum rider keeps the population locked
/// by the first continuous grant, including both the counter and keyword
/// modifications, while leaving the opponent's creature untouched.
#[test]
fn unbreakable_formation_addendum_uses_first_grant_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First creature", 2, 2).id();
    let second = scenario.add_creature(P0, "Second creature", 3, 1).id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Unbreakable Formation", false, UNBREAKABLE_FORMATION)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&spell]
            .abilities
            .iter()
            .any(ability_contains_unimplemented),
        "Unbreakable Formation must parse without Effect::Unimplemented residue: {:?}",
        runner.state().objects[&spell].abilities
    );

    runner.cast(spell).resolve();
    for creature in [first, second] {
        let object = &runner.state().objects[&creature];
        assert_eq!(object.counters[&CounterType::Plus1Plus1], 1);
        assert!(object.has_keyword(&Keyword::Indestructible));
        assert!(object.has_keyword(&Keyword::Vigilance));
    }
    let opponent = &runner.state().objects[&opponent];
    assert_eq!(
        opponent
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        0
    );
    assert!(!opponent.has_keyword(&Keyword::Indestructible));
    assert!(!opponent.has_keyword(&Keyword::Vigilance));
}
