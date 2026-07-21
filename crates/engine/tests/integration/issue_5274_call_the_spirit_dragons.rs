//! Regression for GitHub issue #5274 — Call the Spirit Dragons upkeep counter
//! placement and win rider.
//!
//! Oracle: "At the beginning of your upkeep, for each color, put a +1/+1
//! counter on a Dragon you control of that color. If you put +1/+1 counters on
//! five Dragons this way, you win the game."
//!
//! CR 105.1: iterates each color; CR 122.1: places +1/+1 counters; CR 608.2c:
//! tracked-set "this way" gate for the five-Dragon win (CR 104.2b).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, ForEachCategoryAction, IterationCategory};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const UPKEEP_ORACLE: &str =
    "for each color, put a +1/+1 counter on a Dragon you control of that color. \
If you put +1/+1 counters on five Dragons this way, you win the game.";

fn one_color_cost(color: ManaColor) -> ManaCost {
    let shard = match color {
        ManaColor::White => ManaCostShard::White,
        ManaColor::Blue => ManaCostShard::Blue,
        ManaColor::Black => ManaCostShard::Black,
        ManaColor::Red => ManaCostShard::Red,
        ManaColor::Green => ManaCostShard::Green,
    };
    ManaCost::Cost {
        generic: 0,
        shards: vec![shard],
    }
}

fn add_mono_color_dragon(
    scenario: &mut GameScenario,
    name: &str,
    color: ManaColor,
) -> engine::types::identifiers::ObjectId {
    let mut b = scenario.add_creature(P0, name, 4, 4);
    b.with_subtypes(vec!["Dragon"]);
    b.with_mana_cost(one_color_cost(color));
    b.id()
}

#[test]
fn call_the_spirit_dragons_upkeep_puts_counters_on_five_dragons_and_wins() {
    let mut scenario = GameScenario::new_n_player(2, 5274);
    scenario.at_phase(Phase::Upkeep);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    let source = scenario
        .add_creature(P0, "Call the Spirit Dragons", 0, 0)
        .id();

    for (i, color) in [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
    .iter()
    .enumerate()
    {
        add_mono_color_dragon(&mut scenario, &format!("Dragon {i}"), *color);
    }

    let def = parse_effect_chain(UPKEEP_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            &*def.effect,
            Effect::ForEachCategory {
                category: IterationCategory::Color,
                action: ForEachCategoryAction::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    ..
                },
                ..
            }
        ),
        "expected ForEachCategory(PutCounter), got {:?}",
        def.effect
    );

    let mut runner = scenario.build();
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    let state = runner.state();
    for obj in state.objects.values() {
        if obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Dragon"))
        {
            assert_eq!(
                obj.counters.get(&CounterType::Plus1Plus1).copied(),
                Some(1),
                "{} should have one +1/+1 counter",
                obj.name
            );
        }
    }

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(w) } if w == P0
        ),
        "five Dragons countered this way must win for P0, got {:?}",
        state.waiting_for
    );
    assert!(
        state
            .players
            .iter()
            .find(|p| p.id == P1)
            .map(|p| p.is_eliminated)
            .unwrap(),
        "opponent must be eliminated after win"
    );
}
