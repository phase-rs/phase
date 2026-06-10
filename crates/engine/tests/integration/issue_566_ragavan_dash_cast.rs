//! Regression for issue #566: Ragavan, Nimble Pilferer must offer and execute
//! its Dash alternative cost from hand.
//!
//! Ragavan's printed cost is `{R}` and its dash cost is `{1}{R}`. When both are
//! affordable the engine must surface `AlternativeCastChoice(Dash)` and honor an
//! opt-in dash cast.
//!
//! https://github.com/phase-rs/phase/issues/566

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::StaticDefinition;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::game_state::{AlternativeCastKeyword, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, red: u32, colorless: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..red {
        pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
    }
    for _ in 0..colorless {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

fn setup_ragavan_in_hand() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::CardId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ragavan = scenario
        .add_creature_to_hand(P0, "Ragavan, Nimble Pilferer", 2, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .with_keyword(Keyword::Dash(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        }))
        .id();
    let runner = scenario.build();
    let card_id = runner.state().objects[&ragavan].card_id;
    (runner, ragavan, card_id)
}

#[test]
fn ragavan_cast_offers_dash_when_both_costs_affordable() {
    let (mut runner, ragavan, card_id) = setup_ragavan_in_hand();
    add_mana(&mut runner, 1, 1);

    let result = runner
        .act(GameAction::CastSpell {
            object_id: ragavan,
            card_id,
            targets: vec![],
        })
        .expect("cast Ragavan");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Dash,
                ..
            }
        ),
        "expected AlternativeCastChoice(Dash), got {:?}",
        result.waiting_for
    );
}

#[test]
fn ragavan_dash_choice_casts_creature_onto_battlefield() {
    let (mut runner, ragavan, card_id) = setup_ragavan_in_hand();
    add_mana(&mut runner, 1, 1);

    runner
        .act(GameAction::CastSpell {
            object_id: ragavan,
            card_id,
            targets: vec![],
        })
        .expect("cast Ragavan");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("opt into dash");

    runner.advance_until_stack_empty();

    assert!(
        runner.state().battlefield.contains(&ragavan),
        "Ragavan must resolve onto the battlefield via dash"
    );
    let obj = runner.state().objects.get(&ragavan).unwrap();
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "Ragavan must be a creature on the battlefield"
    );
    assert!(
        obj.keywords.iter().any(|k| matches!(k, Keyword::Haste)),
        "dash resolution must grant haste"
    );
}

/// Discriminating coverage for the actual #566 fix: Dash granted by a
/// `StaticMode::CastWithKeyword` static (CR 604.1), with NO printed Dash in
/// `obj.keywords`, must still surface `AlternativeCastChoice(Dash)`. The
/// pre-fix code read `obj.keywords` directly, so this test fails on
/// origin/main and passes only with the `effective_spell_keywords` routing.
#[test]
fn granted_dash_from_static_offers_dash_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Grantor on P0's battlefield; its CastWithKeyword{Dash} static is attached
    // below (no real card grants Dash, so the static is set synthetically —
    // mirroring the WebSlinging fixture in derived_views.rs).
    let grantor = scenario.add_creature(P0, "Dash Grantor", 2, 2).id();

    // A creature in hand WITHOUT printed Dash.
    let bear = scenario
        .add_creature_to_hand(P0, "Vanilla Bear", 2, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bear].card_id;

    // CR 604.1: grant Dash {1}{R} to spells via a battlefield static. No
    // `affected` filter = applies to every spell; the granted-keyword merge in
    // `effective_spell_keywords` is the seam this test pins.
    let def = StaticDefinition::new(StaticMode::CastWithKeyword {
        keyword: Keyword::Dash(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        }),
    });
    runner
        .state_mut()
        .objects
        .get_mut(&grantor)
        .unwrap()
        .static_definitions = vec![def].into();

    add_mana(&mut runner, 1, 1);

    let result = runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id,
            targets: vec![],
        })
        .expect("cast creature with granted dash");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Dash,
                ..
            }
        ),
        "statics-granted Dash must surface AlternativeCastChoice(Dash), got {:?}",
        result.waiting_for
    );
}
