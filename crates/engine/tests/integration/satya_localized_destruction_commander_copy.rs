//! Regression: The Sixth Doctor's Demonstrate spell copy must not retain the
//! source card's commander designation and block state-based actions after a
//! Localized Destruction board wipe.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 903.3 + CR 707.10 + CR 704.5d: The Sixth Doctor grants Demonstrate to
/// commander Sarah Jane Smith. Its spell copies are tokens, so Localized
/// Destruction's "Destroy all creatures" instruction must offer the real
/// card's command-zone return and then remove the copied token without a
/// second unreachable choice.
#[test]
fn localized_destruction_does_not_deadlock_on_a_copied_commander_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "The Sixth Doctor",
        2,
        4,
        "The first nonartifact spell you cast each turn has demonstrate.",
    );
    let sarah = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarah Jane Smith", 3, 4, "")
        .commander()
        .id();
    let localized_destruction = scenario
        .add_spell_to_hand_from_oracle(P0, "Localized Destruction", false, "Destroy all creatures.")
        .id();

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    // CR 702.144a: accept Demonstrate's self-copy. The opponent copy is also
    // produced by the keyword, but all copies must shed the source's commander
    // designation before resolving.
    runner.cast(sarah).accept_optional().resolve();

    let copied_sarah_ids: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.name == "Sarah Jane Smith" && obj.is_token)
        .map(|obj| obj.id)
        .collect();
    assert!(
        !copied_sarah_ids.is_empty(),
        "CR 702.144a: Demonstrate must create at least its self-copy"
    );
    assert!(
        copied_sarah_ids.iter().all(|id| {
            let copy = &runner.state().objects[id];
            !copy.is_commander && !copy.is_signature_spell()
        }),
        "CR 903.3: copied spells are tokens, not command-zone cards"
    );
    assert!(
        runner.state().objects[&sarah].is_commander,
        "the original Sarah Jane Smith card remains the commander"
    );

    runner.cast(localized_destruction).resolve();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Graveyard,
            ..
        } if commander_id == sarah
    ));

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the genuine commander return choice must be actionable");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "after the genuine choice, priority must progress instead of surfacing a token choice"
    );
    assert_eq!(runner.state().objects[&sarah].zone, Zone::Command);
    assert!(
        copied_sarah_ids
            .iter()
            .all(|id| !runner.state().objects.contains_key(id)),
        "CR 704.5d: copied Sarah Jane tokens must cease to exist after the wipe"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ),
        "no second CommanderZoneChoice may strand the player"
    );
}

/// CR 903.3 + CR 111.6 + CR 704.3 + CR 704.5d: a saved copy of a commander
/// spell must not preserve an unreachable command-zone prompt after it leaves
/// the battlefield. This models the reported capture's `CommanderZoneChoice`
/// shape, then restores it through the production persistence chokepoint.
#[test]
fn restored_token_commander_choice_resumes_sba_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let copied_commander = scenario
        .add_creature(P0, "Copied Commander", 3, 4)
        .commander()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        copied_commander,
        Zone::Graveyard,
        &mut events,
    );
    let player = runner.state().objects[&copied_commander].owner;
    runner
        .state_mut()
        .objects
        .get_mut(&copied_commander)
        .unwrap()
        .is_token = true;
    {
        let state = runner.state_mut();
        state.priority_player = P1;
        state.priority_pass_count = 1;
        state.priority_passes.insert(P1);
        state.waiting_for = WaitingFor::CommanderZoneChoice {
            player,
            commander_id: copied_commander,
            current_zone: Zone::Graveyard,
        };
    }

    let serialized = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the captured command-zone choice serializes");
    let mut restored = serde_json::from_str::<PersistedGameState>(&serialized)
        .expect("the captured command-zone choice restores")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    assert!(matches!(
        &restored.waiting_for,
        WaitingFor::Priority { player } if *player == P0
    ));
    assert_eq!(restored.priority_player, P0);
    assert_eq!(restored.priority_pass_count, 0);
    assert!(restored.priority_passes.is_empty());
    assert!(
        !restored.objects.contains_key(&copied_commander),
        "CR 704.5d: the malformed token must cease to exist during restore cleanup"
    );
    engine::game::engine::apply(&mut restored, P0, GameAction::PassPriority)
        .expect("the active player must be able to act after stale choice recovery");
}

/// CR 723.3 + CR 723.5: restoring an impossible command-zone prompt during a
/// controlled turn preserves the controlled seat as the semantic priority
/// holder while assigning action authority to the turn controller.
#[test]
fn restored_token_commander_choice_respects_turn_control_priority_authority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let copied_commander = scenario
        .add_creature(P0, "Controlled Copied Commander", 3, 4)
        .commander()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        copied_commander,
        Zone::Graveyard,
        &mut events,
    );
    let player = runner.state().objects[&copied_commander].owner;
    {
        let state = runner.state_mut();
        state.objects.get_mut(&copied_commander).unwrap().is_token = true;
        state.active_player = P0;
        state.turn_decision_controller = Some(P1);
        state.priority_player = P0;
        state.priority_pass_count = 1;
        state.priority_passes.insert(P0);
        state.waiting_for = WaitingFor::CommanderZoneChoice {
            player,
            commander_id: copied_commander,
            current_zone: Zone::Graveyard,
        };
    }

    let serialized = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the controlled stale command-zone choice serializes");
    let mut restored = serde_json::from_str::<PersistedGameState>(&serialized)
        .expect("the controlled stale command-zone choice restores")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    assert!(matches!(
        &restored.waiting_for,
        WaitingFor::Priority { player } if *player == P0
    ));
    assert_eq!(restored.priority_player, P1);
    assert_eq!(restored.priority_pass_count, 0);
    assert!(restored.priority_passes.is_empty());
    assert!(
        !restored.objects.contains_key(&copied_commander),
        "CR 704.5d: the malformed token must cease to exist during restore cleanup"
    );
    assert!(
        engine::game::engine::apply(&mut restored, P0, GameAction::PassPriority).is_err(),
        "CR 723.5: the controlled seat cannot submit its own priority action"
    );
    engine::game::engine::apply(&mut restored, P1, GameAction::PassPriority)
        .expect("CR 723.5: the turn controller must be able to submit priority actions");
}

/// CR 903.3 + CR 903.9a: restore must preserve a legitimate card-backed
/// command-zone choice; only the impossible token-backed shape is normalized.
#[test]
fn restored_genuine_commander_choice_remains_actionable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let commander = scenario
        .add_creature(P0, "Genuine Commander", 3, 4)
        .commander()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), commander, Zone::Graveyard, &mut events);
    let player = runner.state().objects[&commander].owner;
    runner.state_mut().waiting_for = WaitingFor::CommanderZoneChoice {
        player,
        commander_id: commander,
        current_zone: Zone::Graveyard,
    };

    let serialized = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the genuine command-zone choice serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&serialized)
        .expect("the genuine command-zone choice restores")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    assert!(matches!(
        restored.waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Graveyard,
            ..
        } if commander_id == commander
    ));
    assert_eq!(restored.objects[&commander].zone, Zone::Graveyard);
}
