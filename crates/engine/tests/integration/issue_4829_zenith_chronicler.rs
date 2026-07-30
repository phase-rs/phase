//! Issue #4829: Zenith Chronicler must draw for each other player when a player
//! casts their first multicolored spell each turn.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, Effect, FilterProp, PlayerFilter, TargetFilter, TriggerConstraint, TypeFilter,
    TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const ZENITH_CHRONICLER_ORACLE: &str =
    "Whenever a player casts their first multicolored spell each turn, each other player draws a card.";

const MULTICOLOR_SPELL: &str = "Deal 3 damage to any target.";

fn hand_size(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

#[test]
fn zenith_chronicler_parses_multicolored_first_spell_and_other_player_draw() {
    let parsed = parse_oracle_text(
        ZENITH_CHRONICLER_ORACLE,
        "Zenith Chronicler",
        &[],
        &["Creature".to_string(), "Artifact".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Zenith Chronicler must parse a trigger");
    assert!(matches!(
        trigger.constraint,
        Some(TriggerConstraint::NthSpellThisTurn { n: 1, .. })
    ));
    if let Some(TriggerConstraint::NthSpellThisTurn { filter, .. }) = &trigger.constraint {
        assert_eq!(
            filter.as_ref(),
            Some(&TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).properties(vec![FilterProp::ColorCount {
                    comparator: Comparator::GE,
                    count: 2,
                }],)
            )),
        );
    }
    let execute = trigger.execute.as_ref().expect("trigger must have execute");
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::AllExcept {
            exclude: Box::new(PlayerFilter::TriggeringPlayer),
        })
    );
    assert!(matches!(execute.effect.as_ref(), Effect::Draw { .. }));
}

#[test]
fn zenith_chronicler_draws_for_controller_when_opponent_casts_multicolored_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zenith Chronicler", 3, 1, ZENITH_CHRONICLER_ORACLE);
    scenario.with_library_top(P0, &["Draw Card A", "Draw Card B"]);
    scenario.with_mana_pool(
        P1,
        vec![
            engine::types::mana::ManaUnit::new(
                ManaType::Red,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
            engine::types::mana::ManaUnit::new(
                ManaType::White,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            ),
        ],
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Boros Charm", true, MULTICOLOR_SPELL)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red, ManaCostShard::White],
        })
        .id();
    let target = scenario.add_creature(P0, "Target Dummy", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Instant];
        obj.base_card_types = obj.card_types.clone();
    }

    let p0_hand_before = hand_size(&runner, P0);

    runner.cast(spell).target_object(target).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P0),
        p0_hand_before + 1,
        "Zenith controller must draw when opponent casts first multicolored spell"
    );
}

#[test]
fn zenith_chronicler_does_not_draw_for_monocolored_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zenith Chronicler", 3, 1, ZENITH_CHRONICLER_ORACLE);
    scenario.with_library_top(P0, &["Draw Card A", "Draw Card B"]);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let target = scenario.add_creature(P0, "Target Dummy", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Instant];
        obj.base_card_types = obj.card_types.clone();
    }

    let p0_hand_before = hand_size(&runner, P0);

    runner.cast(spell).target_object(target).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P0),
        p0_hand_before,
        "Zenith Chronicler must not draw when opponent casts a monocolored spell"
    );
}
