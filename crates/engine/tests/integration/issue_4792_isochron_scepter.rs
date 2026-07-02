//! Issue #4792: Isochron Scepter must allow casting the copied imprinted instant.

use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::TrackedSetId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn fund_generic(runner: &mut GameRunner, amount: u32) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

fn link_imprinted_instant(
    runner: &mut GameRunner,
    scepter: engine::types::identifiers::ObjectId,
    imprint: engine::types::identifiers::ObjectId,
) {
    let state = runner.state_mut();
    assert_eq!(
        state.objects.get(&imprint).map(|o| o.zone),
        Some(Zone::Exile),
        "imprint candidate must start in exile"
    );
    state.exile_links.push(ExileLink {
        source_id: scepter,
        exiled_id: imprint,
        kind: ExileLinkKind::TrackedBySource,
    });
    state
        .tracked_object_sets
        .insert(TrackedSetId(0), vec![imprint]);
}

fn drive_isochron_activation(runner: &mut GameRunner, target: engine::types::ability::TargetRef) {
    for step in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional copy/cast");
            }
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("choose shock target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state at step {step}: {other:?}");
            }
            _ => {}
        }
    }
    panic!(
        "Isochron activation did not finish: stack={:?} waiting={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
}

#[test]
fn isochron_scepter_copies_and_casts_imprinted_instant() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scepter = scenario.add_real_card(P0, "Isochron Scepter", Zone::Battlefield, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Exile, db);

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    link_imprinted_instant(&mut runner, scepter, shock);
    fund_generic(&mut runner, 2);

    let life_before = runner.state().players[1].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
        })
        .expect("Isochron activation must be legal with imprint and mana");

    drive_isochron_activation(&mut runner, engine::types::ability::TargetRef::Player(P1));
    runner.advance_until_stack_empty();

    assert!(
        runner.state().players[1].life < life_before,
        "Shock copy must deal damage to the chosen creature's controller"
    );
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Exile),
        "imprinted Shock stays exiled after copying"
    );
}
