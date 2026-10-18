//! "If any of that mana is spent on a creature spell, it gains haste until end
//! of turn." (Arena of Glory, Generator Servant).
//!
//! Player report: activating Arena of Glory's exert ability gave haste to the
//! LAND instead of to the creature spell the mana paid for. The rider's
//! "any of" quantifier missed the shared "If that mana is spent on" mana-rider
//! opening, so the clause fell through to a self-targeted keyword grant.
//!
//! Rules exercised:
//! - CR 106.6: mana can carry an additional effect on the spell it is spent on.
//! - CR 106.6a: that effect is created separately for each mana produced, so
//!   spending any one unit on a creature spell grants it haste, and splitting
//!   the mana across two creature spells hastes both (Scryfall rulings).
//! - CR 601.2h: the mana is spent while paying the spell's total cost.
//! - CR 702.10b: a creature with haste can attack the turn it arrives.
//! - CR 400.7a: an effect on a permanent spell continues to apply to the
//!   permanent that spell becomes.
//! - CR 903.8 + CR 903.3d: a commander cast from the command zone is a spell
//!   that is a commander, so it receives the grant like any other creature spell.

use engine::game::combat::get_valid_attacker_ids;
use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, ContinuousModification, Duration, Effect};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaRestriction, ManaSpellGrant, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall), reminder text included.
const ARENA_OF_GLORY_ORACLE: &str = "This land enters tapped unless you control a Mountain.\n\
{T}: Add {R}.\n\
{R}, {T}, Exert this land: Add {R}{R}. If any of that mana is spent on a creature spell, \
it gains haste until end of turn. (An exerted permanent won't untap during your next untap step.)";

/// Verbatim Oracle text (Scryfall), reminder text included.
const GENERATOR_SERVANT_ORACLE: &str = "{T}, Sacrifice this creature: Add {C}{C}. If any of that \
mana is spent on a creature spell, it gains haste until end of turn. (That creature can attack \
and {T} as soon as it comes under your control.)";

/// The grant every unit produced by the rider-bearing ability must carry.
fn creature_spell_haste_grant() -> ManaSpellGrant {
    ManaSpellGrant::AddKeywordUntilEndOfTurn {
        keyword: Keyword::Haste,
        restriction: Some(ManaRestriction::OnlyForSpellType("Creature".to_string())),
        duration: Box::new(Duration::UntilEndOfTurn),
    }
}

fn mana_pool_units(runner: &GameRunner) -> Vec<ManaUnit> {
    let player = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 must exist");
    player.mana_pool.units().collect()
}

/// Exert route observed: after the activation and before the cast, the pool
/// holds exactly two units and every unit's `grants` equals
/// `[AddKeywordUntilEndOfTurn { Haste, Some(OnlyForSpellType("Creature")), UntilEndOfTurn }]`.
/// Units from `{T}: Add {R}` and from the Mountain carry no grants, so this
/// proves the exert ability ran and that the fixed parse is live.
fn assert_pool_holds_two_granted_units(runner: &GameRunner, expected_color: ManaType) {
    let units = mana_pool_units(runner);
    assert_eq!(
        units.len(),
        2,
        "the rider ability must leave exactly two units, got {units:?}"
    );
    for unit in units {
        assert_eq!(
            unit.color, expected_color,
            "unexpected unit color: {unit:?}"
        );
        assert_eq!(
            unit.grants,
            vec![creature_spell_haste_grant()],
            "every produced unit must carry the creature-spell haste grant"
        );
    }
}

/// Index of Arena's exert ability, located by its Composite `{R}, {T}, Exert`
/// cost so the test can never activate `{T}: Add {R}` by accident.
fn exert_mana_ability_index(runner: &GameRunner, arena: ObjectId) -> usize {
    let abilities = &runner.state().objects[&arena].abilities;
    let composite_indices: Vec<usize> = abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| matches!(ability.cost, Some(AbilityCost::Composite { .. })))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        composite_indices.len(),
        1,
        "Arena of Glory must have exactly one Composite-cost ability, abilities: {abilities:?}"
    );
    composite_indices[0]
}

/// Untaps Arena and its Mountain, taps the Mountain for the `{R}` activation
/// cost, then activates the exert ability and checks the exert route ran.
fn activate_arena_exert_ability(runner: &mut GameRunner, arena: ObjectId, mountain: ObjectId) {
    // Arena may enter tapped; untap both lands so the test controls exactly
    // which mana exists.
    for land in [arena, mountain] {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = false;
    }
    runner.activate(mountain, 0).resolve();
    let exert_index = exert_mana_ability_index(runner, arena);
    runner.activate(arena, exert_index).resolve();
    assert_pool_holds_two_granted_units(runner, ManaType::Red);
}

fn transient_haste_effect_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    }
                )
            })
        })
        .count()
}

/// Parses the verbatim card and checks that exactly one of its mana abilities
/// carries the rider as a creature-spell haste grant (no self-targeted
/// sub-ability), that no ability is `Unimplemented`, and that no mana ability
/// gained a spend restriction.
fn assert_rider_folds_into_grants(
    oracle: &str,
    name: &str,
    card_type: &str,
    mana_ability_count: usize,
) {
    let parsed = parse_oracle_text(oracle, name, &[], &[card_type.to_string()], &[]);
    for ability in &parsed.abilities {
        assert!(
            !matches!(*ability.effect, Effect::Unimplemented { .. }),
            "{name} must not leave an Unimplemented ability: {ability:?}"
        );
    }

    let mana_abilities: Vec<_> = parsed
        .abilities
        .iter()
        .filter(|ability| matches!(*ability.effect, Effect::Mana { .. }))
        .collect();
    assert_eq!(
        mana_abilities.len(),
        mana_ability_count,
        "{name} must parse {mana_ability_count} mana abilities (reminder text adds none)"
    );

    let mut rider_count = 0;
    for ability in mana_abilities {
        let Effect::Mana {
            grants,
            restrictions,
            ..
        } = &*ability.effect
        else {
            unreachable!("filtered to Mana effects");
        };
        // Ruling: the mana can be spent on anything, so the creature clause is a
        // grant condition and never a CR 106.6 spend restriction.
        assert!(
            restrictions.is_empty(),
            "{name} mana must be spendable on anything"
        );
        if grants.is_empty() {
            continue;
        }

        rider_count += 1;
        assert_eq!(grants, &vec![creature_spell_haste_grant()]);
        assert!(
            ability.sub_ability.is_none(),
            "{name}'s rider must fold into grants, not a self-targeted sub-ability: {:?}",
            ability.sub_ability
        );
    }
    assert_eq!(
        rider_count, 1,
        "{name} must carry exactly one rider-bearing mana ability"
    );
}

/// CR 106.6 + CR 106.6a: Arena's exert rider folds into `grants`; its plain
/// `{T}: Add {R}` ability still parses without a grant (reach-guard).
#[test]
fn arena_of_glory_exert_rider_parses_as_creature_haste_grant() {
    assert_rider_folds_into_grants(ARENA_OF_GLORY_ORACLE, "Arena of Glory", "Land", 2);
}

/// CR 106.6 + CR 106.6a: Generator Servant's rider folds into `grants`.
#[test]
fn generator_servant_rider_parses_as_creature_haste_grant() {
    assert_rider_folds_into_grants(GENERATOR_SERVANT_ORACLE, "Generator Servant", "Creature", 1);
}

/// CR 106.6a + CR 702.10b: spending ONE unit of Arena's {R}{R} on a creature
/// spell gives it haste, so it can attack this turn; the land gains nothing.
#[test]
fn arena_of_glory_mana_gives_creature_spell_haste() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mountain = scenario
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();

    activate_arena_exert_ability(&mut runner, arena, mountain);
    runner
        .cast(bear)
        .resolve()
        .assert_zone(&[bear], Zone::Battlefield);

    // Exactly one unit was spent; the other still carries its grant.
    let remaining = mana_pool_units(&runner);
    assert_eq!(remaining.len(), 1, "a {{1}} spell spends exactly one unit");
    assert_eq!(remaining[0].grants, vec![creature_spell_haste_grant()]);

    assert!(
        has_keyword(&runner.state().objects[&bear], &Keyword::Haste),
        "the creature spell must gain haste"
    );
    assert!(
        get_valid_attacker_ids(runner.state()).contains(&bear),
        "a hasted creature can attack the turn it arrives"
    );
    assert!(
        !has_keyword(&runner.state().objects[&arena], &Keyword::Haste),
        "the land itself must not gain haste"
    );
}

/// CR 903.8 + CR 903.3d: a commander cast from the command zone with Arena's
/// mana gains haste exactly like a creature cast from hand.
#[test]
fn arena_of_glory_mana_gives_commander_cast_from_command_zone_haste() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mountain = scenario
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    // Base cost {1} with no prior command-zone casts: commander tax is 0
    // (CR 903.8), so the two-unit pool pays for it.
    let commander = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_commander(commander);
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    activate_arena_exert_ability(&mut runner, arena, mountain);
    runner
        .cast(commander)
        .resolve()
        .assert_zone(&[commander], Zone::Battlefield);

    assert_eq!(
        runner.state().objects[&commander].cast_from_zone,
        Some(Zone::Command),
        "the commander must have been cast from the command zone"
    );
    assert!(
        has_keyword(&runner.state().objects[&commander], &Keyword::Haste),
        "the commander must gain haste"
    );
    assert!(get_valid_attacker_ids(runner.state()).contains(&commander));
    assert!(
        !has_keyword(&runner.state().objects[&arena], &Keyword::Haste),
        "the land itself must not gain haste"
    );
}

/// CR 106.6: the rider only grants when the spell is a creature spell; mana
/// spent on a noncreature spell creates no haste effect.
#[test]
fn arena_of_glory_mana_on_noncreature_spell_grants_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mountain = scenario
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mind Stone Replica", "{T}: Add {C}.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();

    // Reach-guard: the granted units exist before the spend, so a zero count
    // below is the restriction gate rejecting the spell, not a missing grant.
    activate_arena_exert_ability(&mut runner, arena, mountain);
    runner
        .cast(artifact)
        .resolve()
        .assert_zone(&[artifact], Zone::Battlefield);
    assert_eq!(
        mana_pool_units(&runner).len(),
        1,
        "one granted unit must have been spent on the artifact"
    );

    assert_eq!(
        transient_haste_effect_count(&runner),
        0,
        "a noncreature spell must not receive the haste grant"
    );
    assert!(!has_keyword(
        &runner.state().objects[&artifact],
        &Keyword::Haste
    ));
    assert!(
        !has_keyword(&runner.state().objects[&arena], &Keyword::Haste),
        "the land itself must not gain haste"
    );
}

/// CR 106.6a (Scryfall ruling): mana split across two creature spells gives
/// each of them haste — the grant is per unit, not once per activation.
#[test]
fn arena_of_glory_mana_split_across_two_creature_spells_hastes_both() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arena = scenario
        .add_land_from_oracle(P0, "Arena of Glory", ARENA_OF_GLORY_ORACLE)
        .id();
    let mountain = scenario
        .add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.")
        .id();
    let first_bear = scenario
        .add_creature_to_hand(P0, "First Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let second_bear = scenario
        .add_creature_to_hand(P0, "Second Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();

    activate_arena_exert_ability(&mut runner, arena, mountain);
    runner
        .cast(first_bear)
        .resolve()
        .assert_zone(&[first_bear], Zone::Battlefield);

    // Reach-guard: the second spell is paid by the remaining granted unit.
    let remaining = mana_pool_units(&runner);
    assert_eq!(
        remaining.len(),
        1,
        "one granted unit must remain for the second spell"
    );
    assert_eq!(remaining[0].grants, vec![creature_spell_haste_grant()]);

    runner
        .cast(second_bear)
        .resolve()
        .assert_zone(&[second_bear], Zone::Battlefield);

    assert!(
        has_keyword(&runner.state().objects[&first_bear], &Keyword::Haste),
        "the first creature must gain haste"
    );
    assert!(
        has_keyword(&runner.state().objects[&second_bear], &Keyword::Haste),
        "the second creature must gain haste"
    );
}

/// CR 106.6a: the grant lives on each produced unit, so it still applies after
/// Generator Servant was sacrificed to make the mana.
#[test]
fn generator_servant_mana_gives_creature_spell_haste() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let servant = scenario
        .add_creature_from_oracle(P0, "Generator Servant", 1, 1, GENERATOR_SERVANT_ORACLE)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();

    runner.activate(servant, 0).resolve();
    assert_eq!(
        runner.state().objects[&servant].zone,
        Zone::Graveyard,
        "the sacrifice cost must have been paid before the mana is spent"
    );
    assert_pool_holds_two_granted_units(&runner, ManaType::Colorless);

    runner
        .cast(bear)
        .resolve()
        .assert_zone(&[bear], Zone::Battlefield);

    assert!(
        has_keyword(&runner.state().objects[&bear], &Keyword::Haste),
        "the creature spell must gain haste"
    );
    assert!(get_valid_attacker_ids(runner.state()).contains(&bear));
}
