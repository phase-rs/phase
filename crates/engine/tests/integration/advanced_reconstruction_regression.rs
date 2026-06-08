use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn advanced_reconstruction_randomly_exiles_and_grants_play_permission() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _advanced_reconstruction =
        scenario.add_real_card(P0, "Advanced Reconstruction", Zone::Battlefield, db);
    let draw_card = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let milled_card = scenario.add_real_card(P0, "Memnite", Zone::Library, db);
    let graveyard_card = scenario.add_real_card(P0, "Ornithopter", Zone::Graveyard, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // CR 117.1c: priority opens during Upkeep + Draw, so we use the
    // helper that drains them after `auto_advance` to land in PreCombatMain.
    let waiting = runner.auto_advance_to_main_phase();

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        matches!(waiting, WaitingFor::Priority { player } if player == P0),
        "expected P0 priority after Advanced Reconstruction trigger queued, got {waiting:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Advanced Reconstruction must place exactly one trigger on the stack"
    );
    assert!(
        matches!(
            runner.state().stack[0].kind,
            StackEntryKind::TriggeredAbility { .. }
        ),
        "Advanced Reconstruction stack entry must be a triggered ability, got {:?}",
        runner.state().stack[0].kind
    );
    assert_eq!(
        runner.state().objects[&draw_card].zone,
        Zone::Hand,
        "draw step should draw the top library card before the trigger resolves"
    );
    assert_eq!(
        runner.state().objects[&milled_card].zone,
        Zone::Library,
        "the second library card should remain available for the trigger's mill step"
    );

    let _ = runner
        .act(GameAction::PassPriority)
        .expect("P0 should be able to pass priority");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::EffectZoneChoice { .. }
        ),
        "random graveyard exile must not prompt after the first priority pass"
    );

    let _ = runner
        .act(GameAction::PassPriority)
        .expect("P1 should be able to pass priority and resolve the trigger");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::EffectZoneChoice { .. }
        ),
        "random graveyard exile must not prompt during trigger resolution"
    );

    let eligible = [milled_card, graveyard_card];
    let exiled: Vec<_> = eligible
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].zone == Zone::Exile)
        .collect();
    assert_eq!(
        exiled.len(),
        1,
        "engine must randomly exile exactly one eligible graveyard card; zones={:?}",
        eligible
            .iter()
            .map(|id| (*id, runner.state().objects[id].zone))
            .collect::<Vec<_>>()
    );
    assert!(
        matches!(
            runner.state().objects[&milled_card].zone,
            Zone::Graveyard | Zone::Exile
        ),
        "the trigger must mill the second library card before random exile; zone={:?}",
        runner.state().objects[&milled_card].zone
    );

    let exiled_card = exiled[0];
    let permissions = &runner.state().objects[&exiled_card].casting_permissions;
    assert!(
        permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                granted_to,
                ..
            } if *granted_to == P0
        )),
        "exiled card must receive PlayFromExile until end of turn for P0; permissions={permissions:?}"
    );

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "expected P0 priority after the trigger resolves, got {:?}",
        runner.state().waiting_for
    );

    let actions = engine::ai_support::legal_actions(runner.state());
    assert!(
        actions.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == exiled_card
        )),
        "legal_actions must expose casting the exiled zero-cost card; actions={actions:?}"
    );
}
