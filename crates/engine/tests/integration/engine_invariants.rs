use assert_matches::assert_matches;
use engine::ai_support::legal_actions;
use engine::game::apply_as_current;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::zones::Zone;
use proptest::prelude::*;

fn target_selection_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let bear_id = scenario.add_creature(P1, "Bear", 2, 2).id();
    let bolt_id = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should succeed");
    assert_matches!(result.waiting_for, WaitingFor::TargetSelection { .. });
    assert_eq!(runner.state().objects[&bear_id].zone, Zone::Battlefield);
    runner.state().clone()
}

fn declare_attackers_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    scenario.add_creature(P0, "Attacker", 3, 3);
    let mut runner = scenario.build();
    runner.pass_both_players();
    assert_matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    );
    runner.state().clone()
}

fn declare_blockers_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    scenario.add_creature(P1, "Blocker", 2, 2);
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
        })
        .expect("declare attackers should succeed");
    runner.pass_both_players();
    assert_matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    );
    runner.state().clone()
}

fn assign_combat_damage_state() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let attacker = {
        let mut builder = scenario.add_creature(P0, "Trampler", 5, 5);
        builder.trample();
        builder.id()
    };
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 2, 2).id();
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
        })
        .expect("declare attackers should succeed");
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker_a, attacker), (blocker_b, attacker)],
        })
        .expect("declare blockers should succeed");
    runner.pass_both_players();
    assert_matches!(
        runner.state().waiting_for,
        WaitingFor::AssignCombatDamage { .. }
    );
    runner.state().clone()
}

fn assert_zone_consistency(state: &GameState) {
    for player in &state.players {
        for &id in &player.hand {
            let obj = state.objects.get(&id).expect("hand object must exist");
            assert_eq!(obj.zone, Zone::Hand, "hand object must remain in hand");
            assert_eq!(obj.owner, player.id, "hand object owner must match player");
        }
        for &id in &player.library {
            let obj = state.objects.get(&id).expect("library object must exist");
            assert_eq!(
                obj.zone,
                Zone::Library,
                "library object must remain in library"
            );
            assert_eq!(
                obj.owner, player.id,
                "library object owner must match player"
            );
        }
        for &id in &player.graveyard {
            let obj = state.objects.get(&id).expect("graveyard object must exist");
            assert_eq!(
                obj.zone,
                Zone::Graveyard,
                "graveyard object must remain in graveyard"
            );
            assert_eq!(
                obj.owner, player.id,
                "graveyard object owner must match player"
            );
        }
    }

    for &id in &state.battlefield {
        let obj = state
            .objects
            .get(&id)
            .expect("battlefield object must exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "battlefield object must remain on the battlefield"
        );
    }

    for &id in &state.exile {
        let obj = state.objects.get(&id).expect("exile object must exist");
        assert_eq!(obj.zone, Zone::Exile, "exile object must remain in exile");
    }

    for &id in &state.command_zone {
        let obj = state
            .objects
            .get(&id)
            .expect("command-zone object must exist");
        assert_eq!(
            obj.zone,
            Zone::Command,
            "command-zone object must remain in the command zone"
        );
    }

    // CR 601.2a: a spell enters the stack at announcement, but the engine
    // defers the origin-zone → Stack `obj.zone` flip to `finalize_cast` so
    // off-zone statics (escape, flashback, cast-from-exile) still apply
    // during cost/target/mode resolution. For that one in-flight entry,
    // `obj.zone` is allowed to equal the pending cast's `origin_zone`.
    let pending_cast = state
        .waiting_for
        .pending_cast_ref()
        .or(state.pending_cast.as_deref());
    for entry in &state.stack {
        let obj = state
            .objects
            .get(&entry.source_id)
            .expect("stack source object must exist");
        let pre_commit_ok = pending_cast
            .is_some_and(|pc| pc.object_id == entry.source_id && obj.zone == pc.origin_zone);
        assert!(
            obj.zone == Zone::Stack || pre_commit_ok,
            "stack source must be in the stack zone (or in its pre-commit origin zone for a pending cast); got {:?}",
            obj.zone
        );
    }
}

fn assert_assign_combat_damage_actions_respect_budget(state: &GameState) {
    if let WaitingFor::AssignCombatDamage { total_damage, .. } = &state.waiting_for {
        for action in legal_actions(state) {
            if let GameAction::AssignCombatDamage {
                mode,
                assignments,
                trample_damage,
                controller_damage,
            } = action
            {
                match mode {
                    engine::types::game_state::CombatDamageAssignmentMode::Normal => {
                        let assigned_total: u32 =
                            assignments.iter().map(|(_, amount)| *amount).sum();
                        assert_eq!(
                            assigned_total + trample_damage + controller_damage,
                            *total_damage,
                            "combat damage assignments must spend the full damage budget"
                        );
                    }
                    engine::types::game_state::CombatDamageAssignmentMode::AsThoughUnblocked => {
                        assert!(
                            assignments.is_empty() && trample_damage == 0 && controller_damage == 0,
                            "as-though-unblocked combat damage should not use blocker/trample splits"
                        );
                    }
                }
            }
        }
    }
}

fn assert_all_legal_actions_apply(state: &GameState) {
    for action in legal_actions(state) {
        let mut sim = state.clone();
        apply_as_current(&mut sim, action.clone()).unwrap_or_else(|err| {
            panic!(
                "legal action {} should remain reducer-legal: {err}",
                action.variant_name()
            )
        });
    }
}

fn exercise_state(mut state: GameState, picks: &[u8], max_steps: usize) {
    for step in 0..max_steps {
        assert_zone_consistency(&state);
        assert_assign_combat_damage_actions_respect_budget(&state);
        assert_all_legal_actions_apply(&state);

        let actions = legal_actions(&state);
        if actions.is_empty() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }

        let choice = picks[step % picks.len()] as usize % actions.len();
        apply_as_current(&mut state, actions[choice].clone())
            .expect("chosen legal action should apply");
    }

    assert_zone_consistency(&state);
    assert_assign_combat_damage_actions_respect_budget(&state);
    assert_all_legal_actions_apply(&state);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    #[test]
    fn legal_action_driven_sequences_preserve_engine_invariants(
        picks in prop::collection::vec(any::<u8>(), 1..16),
        step_count in 1usize..8,
        state_kind in 0u8..4,
    ) {
        let state = match state_kind {
            0 => GameState::new_two_player(42),
            1 => target_selection_state(),
            2 => declare_attackers_state(),
            3 => declare_blockers_state(),
            _ => assign_combat_damage_state(),
        };

        exercise_state(state, &picks, step_count);
    }
}

#[test]
fn generated_priority_actions_remain_reducer_legal_in_default_opening_state() {
    let state = GameState::new_two_player(42);
    assert!(legal_actions(&state).iter().all(|action| {
        let mut sim = state.clone();
        apply_as_current(&mut sim, action.clone()).is_ok()
    }));
}
