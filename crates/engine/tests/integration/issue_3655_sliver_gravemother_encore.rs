//! Regression for GitHub issue #3655 — Sliver Gravemother grants encore whose
//! cost equals each Sliver's mana value; granted encore must surface as a
//! graveyard activated ability costing generic mana equal to that mana value.
//!
//! https://github.com/phase-rs/phase/issues/3655

use engine::ai_support::legal_actions;
use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, ContinuousModification, Effect};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const GRAVEMOTHER_ORACLE: &str = "The \"legend rule\" doesn't apply to Slivers you control.\n\
Each Sliver creature card in your graveyard has encore {X}, where X is its mana value.\n\
Encore {5} ({5}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";

#[test]
fn sliver_gravemother_parses_inline_encore_grant() {
    let mut scenario = GameScenario::new();
    let gravemother_id = scenario
        .add_creature_from_oracle(P0, "Sliver Gravemother", 6, 6, GRAVEMOTHER_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&gravemother_id).unwrap();
    let grant = obj
        .static_definitions
        .iter_unchecked()
        .find(|d| {
            d.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Encore(ManaCost::SelfManaValue),
                    }
                )
            })
        })
        .expect("Sliver Gravemother must grant Encore(SelfManaValue) to graveyard Slivers");
    assert!(
        grant.affected.is_some(),
        "encore grant must carry an affected filter"
    );
    assert!(
        obj.static_definitions
            .iter_unchecked()
            .any(|d| matches!(d.mode, StaticMode::LegendRuleDoesntApply)),
        "legend-rule exemption static must parse"
    );
}

/// CR 702.141a + CR 202.3: a Sliver in graveyard with mana cost {2}{R}{G} must
/// surface an Encore activated ability costing {4} + exile when Gravemother
/// grants encore equal to mana value.
#[test]
fn sliver_gravemother_granted_encore_uses_recipient_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Sliver Gravemother", 6, 6, GRAVEMOTHER_ORACLE);

    let card_cost = ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Red, ManaCostShard::Green],
    };
    let expected_encore_cost = ManaCost::generic(4);
    let sliver_id = scenario
        .add_creature_to_graveyard(P0, "Graveyard Sliver", 2, 2)
        .with_mana_cost(card_cost.clone())
        .with_subtypes(vec!["Sliver"])
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let runner = scenario.build();
    let abilities = activated_ability_definitions(runner.state(), sliver_id);
    let (_, encore) = abilities
        .iter()
        .find(|(_, a)| matches!(&*a.effect, Effect::Encore))
        .expect("granted encore must surface on the graveyard Sliver");
    assert_eq!(encore.activation_zone, Some(Zone::Graveyard));
    let Some(AbilityCost::Composite { costs }) = &encore.cost else {
        panic!("encore cost must be composite, got {:?}", encore.cost);
    };
    assert!(
        costs
            .iter()
            .any(|c| matches!(c, AbilityCost::Mana { cost } if *cost == expected_encore_cost)),
        "encore mana sub-cost must equal the Sliver's mana value, got {costs:?}"
    );

    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility { source_id, .. } if *source_id == sliver_id
        )),
        "legal_actions must expose Encore activation for the graveyard Sliver"
    );
}
