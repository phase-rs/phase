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
        .add_spell_to_hand_from_oracle(P0, "Break of Day", true, BREAK_OF_DAY)
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

fn cast_unbreakable_formation(
    phase: Phase,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let first = scenario.add_creature(P0, "First creature", 2, 2).id();
    let second = scenario.add_creature(P0, "Second creature", 3, 1).id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Unbreakable Formation", true, UNBREAKABLE_FORMATION)
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
    (runner, first, second, opponent)
}

fn cast_kytheons_tactics(
    spell_mastery_cards: usize,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "First creature", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Opponent creature", 4, 4).id();
    for index in 0..spell_mastery_cards {
        scenario.add_spell_to_graveyard(P0, &format!("Spell mastery card {index}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Kytheon's Tactics",
            false,
            "Creatures you control get +2/+1 until end of turn.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, those creatures also gain vigilance until end of turn.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&spell]
            .abilities
            .iter()
            .any(ability_contains_unimplemented),
        "Kytheon's Tactics must parse without Effect::Unimplemented residue: {:?}",
        runner.state().objects[&spell].abilities
    );

    runner.cast(spell).resolve();
    (runner, creature, opponent)
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
    let (runner, first, second, opponent) = cast_unbreakable_formation(Phase::PreCombatMain);
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

/// CR 608.2c + CR 611.2c: the addendum is conditional on casting during the
/// main phase, but the first unconditional grant still resolves outside it.
#[test]
fn unbreakable_formation_addendum_omits_main_phase_rider_outside_main_phase() {
    let (runner, first, second, opponent) = cast_unbreakable_formation(Phase::Upkeep);
    for creature in [first, second] {
        let object = &runner.state().objects[&creature];
        assert_eq!(
            object
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            0
        );
        assert!(object.has_keyword(&Keyword::Indestructible));
        assert!(!object.has_keyword(&Keyword::Vigilance));
    }
    let opponent = &runner.state().objects[&opponent];
    assert!(!opponent.has_keyword(&Keyword::Indestructible));
    assert!(!opponent.has_keyword(&Keyword::Vigilance));
}

/// CR 608.2c + CR 611.2c: spell mastery requires at least two qualifying cards
/// in the controller's graveyard and applies the rider only to that controller's
/// previously populated creature set.
#[test]
fn kytheons_tactics_requires_two_graveyard_spells_for_vigilance() {
    let (one_card, one_creature, one_opponent) = cast_kytheons_tactics(1);
    assert!(!one_card.state().objects[&one_creature].has_keyword(&Keyword::Vigilance));
    assert!(!one_card.state().objects[&one_opponent].has_keyword(&Keyword::Vigilance));

    let (two_cards, two_creature, two_opponent) = cast_kytheons_tactics(2);
    assert!(two_cards.state().objects[&two_creature].has_keyword(&Keyword::Vigilance));
    assert!(!two_cards.state().objects[&two_opponent].has_keyword(&Keyword::Vigilance));
}
