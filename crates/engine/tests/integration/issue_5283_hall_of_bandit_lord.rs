//! Regression for issue #5283: Hall of the Bandit Lord mana must grant haste to
//! creature spells cast with its colorless mana.
//!
//! https://github.com/phase-rs/phase/issues/5283

use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaRestriction, ManaSpellGrant};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HALL_ORACLE: &str = "Hall of the Bandit Lord enters tapped.\n\
{T}, Pay 3 life: Add {C}. If that mana is spent on a creature spell, it gains haste.";

#[test]
fn hall_of_bandit_lord_mana_ability_parses_creature_haste_grant() {
    let parsed = parse_oracle_text(
        HALL_ORACLE,
        "Hall of the Bandit Lord",
        &[],
        &["Land".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .iter()
        .find(|def| matches!(*def.effect, Effect::Mana { .. }))
        .expect("Hall of the Bandit Lord must parse an activated mana ability");
    let Effect::Mana { grants, .. } = &*ability.effect else {
        panic!("expected Mana effect");
    };
    assert_eq!(
        grants,
        &[ManaSpellGrant::AddKeywordUntilEndOfTurn {
            keyword: Keyword::Haste,
            restriction: Some(ManaRestriction::OnlyForSpellType("Creature".to_string())),
            duration: Box::new(engine::types::ability::Duration::Permanent),
        }]
    );
    assert!(
        ability.sub_ability.is_none(),
        "mana rider must fold into grants, not a Spell sub-ability"
    );
}

#[test]
fn hall_of_bandit_lord_mana_grants_haste_to_creature_cast_with_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let hall = scenario
        .add_land_from_oracle(P0, "Hall of the Bandit Lord", HALL_ORACLE)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();

    // Untap Hall so we can activate it this turn.
    runner.state_mut().objects.get_mut(&hall).unwrap().tapped = false;

    let activate_outcome = runner.activate(hall, 0).resolve();
    assert_eq!(
        activate_outcome.mana_pool_color(P0, engine::types::mana::ManaType::Colorless),
        1,
        "Hall of the Bandit Lord must produce one colorless mana"
    );

    let cast_outcome = runner.cast(bear).resolve();
    cast_outcome.assert_zone(&[bear], Zone::Battlefield);

    assert!(
        has_keyword(runner.state().objects.get(&bear).unwrap(), &Keyword::Haste),
        "creature cast with Hall mana must have haste on the battlefield"
    );
}
