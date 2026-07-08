//! Regression for issue #4564: Captain America, Team Leader — ETB trigger must
//! grant keywords and put +1/+1 counters on both the entering Hero and ~.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CAPTAIN_ORACLE: &str =
    "Whenever another Hero you control enters, it gains vigilance and haste until end of turn. Put a +1/+1 counter on that Hero and a +1/+1 counter on Captain America.";

fn p1p1_count(
    state: &engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
) -> u32 {
    state
        .objects
        .get(&id)
        .map(|obj| {
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

#[test]
fn captain_america_team_leader_puts_counters_on_both_heroes() {
    let parsed = parse_oracle_text(
        CAPTAIN_ORACLE,
        "Captain America, Team Leader",
        &[],
        &["Creature".to_string()],
        &[
            "Human".to_string(),
            "Soldier".to_string(),
            "Hero".to_string(),
        ],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == engine::types::triggers::TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let mut ability = trigger.execute.as_ref().expect("execute").as_ref().clone();
    let mut put_counters = Vec::new();
    if let Effect::PutCounter { .. } = ability.effect.as_ref() {
        put_counters.push(());
    }
    while let Some(sub) = ability.sub_ability.take() {
        if let Effect::PutCounter { .. } = sub.effect.as_ref() {
            put_counters.push(());
        }
        ability = *sub;
    }
    assert_eq!(
        put_counters.len(),
        2,
        "expected two PutCounter instructions in trigger chain"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let captain = scenario
        .add_creature_from_oracle(P0, "Captain America, Team Leader", 3, 3, CAPTAIN_ORACLE)
        .id();
    let ally = scenario
        .add_creature_to_hand(P0, "Other Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .id();

    let mut runner = scenario.build();
    let ally_card_id = runner.state().objects[&ally].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ally,
            card_id: ally_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast ally hero");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&ally].zone,
        Zone::Battlefield,
        "ally must enter the battlefield"
    );
    assert!(
        p1p1_count(runner.state(), ally) >= 1,
        "entering Hero must receive a +1/+1 counter"
    );
    assert!(
        p1p1_count(runner.state(), captain) >= 1,
        "Captain America must receive a +1/+1 counter"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&ally)
            .expect("ally")
            .has_keyword(&engine::types::keywords::Keyword::Vigilance),
        "entering Hero must gain vigilance"
    );
}
