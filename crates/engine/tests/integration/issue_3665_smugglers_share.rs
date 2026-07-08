//! Regression for GitHub issue #3665 — Smuggler's Share must only draw when an
//! opponent drew two or more cards this turn (not every end step).
//!
//! https://github.com/phase-rs/phase/issues/3665

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, PlayerFilter, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{BattlefieldEntryRecord, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const SMUGGLERS_SHARE: &str = "Smuggler's Share";
const SMUGGLERS_SHARE_ORACLE: &str = "At the beginning of each end step, draw a card for each opponent who drew two or more cards this turn, then create a Treasure token for each opponent who had two or more lands enter the battlefield under their control this turn.";

fn reach_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

fn hand_len(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn treasure_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.controller == P0)
        .filter(|obj| obj.name.eq_ignore_ascii_case("Treasure"))
        .count()
}

fn record_opponent_land_entry(runner: &mut GameRunner, object_id: u64) {
    runner
        .state_mut()
        .battlefield_entries_this_turn
        .push(BattlefieldEntryRecord {
            object_id: ObjectId(object_id),
            name: format!("Land {object_id}"),
            core_types: vec![CoreType::Land],
            subtypes: vec![],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
            controller: P1,
        });
}

#[test]
fn smugglers_share_from_oracle_text_parses_dynamic_draw_count() {
    let mut scenario = GameScenario::new();
    let share_id = scenario
        .add_creature(P0, SMUGGLERS_SHARE, 0, 0)
        .as_enchantment()
        .from_oracle_text(SMUGGLERS_SHARE_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&share_id).expect("share on bf");
    let trigger = obj
        .trigger_definitions
        .as_slice()
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .expect("parsed end-step trigger");
    let execute = trigger
        .execute
        .as_ref()
        .expect("end-step trigger must have execute ability");
    match execute.effect.as_ref() {
        Effect::Draw { count, .. } => assert!(matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::PlayerAttribute { .. },
                },
            }
        )),
        other => panic!("expected dynamic draw count, got {other:?}"),
    }
}

fn add_smugglers_share(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature(P0, SMUGGLERS_SHARE, 0, 0)
        .as_enchantment()
        .from_oracle_text(SMUGGLERS_SHARE_ORACLE)
        .id()
}

#[test]
fn smugglers_share_does_not_draw_when_opponent_drew_one_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let share = add_smugglers_share(&mut scenario);
    scenario.add_card_to_library_top(P0, "Draw Fodder");
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].cards_drawn_this_turn = 1;

    let hand_before = hand_len(&runner, P0);
    reach_end_step(&mut runner);

    // Resolve Smuggler's Share trigger if it fired (it should not).
    if runner.state().stack.iter().any(|e| {
        matches!(
            &e.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == share
        )
    }) {
        runner.advance_until_stack_empty();
    }

    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "Smuggler's Share must not draw when no opponent drew two or more cards"
    );
}

#[test]
fn smugglers_share_draws_when_opponent_drew_two_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _share = add_smugglers_share(&mut scenario);
    scenario.add_card_to_library_top(P0, "Draw Fodder");
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].cards_drawn_this_turn = 2;

    let hand_before = hand_len(&runner, P0);
    reach_end_step(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "Smuggler's Share must draw once per qualifying opponent"
    );
}

#[test]
fn smugglers_share_creates_treasure_when_opponent_had_two_lands_enter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _share = add_smugglers_share(&mut scenario);
    let mut runner = scenario.build();
    record_opponent_land_entry(&mut runner, 9001);
    record_opponent_land_entry(&mut runner, 9002);

    let treasures_before = treasure_count(&runner);
    reach_end_step(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        treasures_before + 1,
        "Smuggler's Share must create one Treasure per opponent with two or more lands entered"
    );
}
