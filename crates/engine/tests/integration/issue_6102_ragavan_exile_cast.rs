//! Regression for issue #6102: Ragavan's combat-damage trigger must exile the
//! damaged player's top card and grant Ragavan's controller permission to cast
//! that exiled card until end of turn.
//!
//! https://github.com/phase-rs/phase/issues/6102

use engine::game::casting::{exile_lands_playable_by_permission, spell_objects_available_to_cast};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{CardPlayMode, CastingPermission};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RAGAVAN_ORACLE: &str = "Whenever Ragavan deals combat damage to a player, create a Treasure token and exile the top card of that player's library. Until end of turn, you may cast that card.\nDash {1}{R}";

fn drain_until_ragavan_trigger_resolves(
    runner: &mut engine::game::scenario::GameRunner,
    expected_exiled_card: Option<ObjectId>,
) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order Ragavan trigger");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty()
                    && treasure_count(runner.state(), P0) == 1
                    && expected_exiled_card
                        .is_none_or(|card| runner.state().objects[&card].zone == Zone::Exile)
                {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while draining combat trigger");
            }
            other => panic!(
                "unexpected waiting state while draining Ragavan trigger: {other:?} \
                 (phase={:?})",
                runner.state().phase
            ),
        }
    }
    panic!("Ragavan trigger did not resolve");
}

fn advance_to_post_combat_main_with_p0_priority(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..16 {
        let state = runner.state();
        if state.phase == Phase::PostCombatMain
            && state.stack.is_empty()
            && matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0)
        {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "combat did not advance to P0 priority in postcombat main: phase={:?}, waiting={:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn treasure_count(state: &engine::types::game_state::GameState, owner: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.owner == owner && obj.is_token && obj.name == "Treasure")
        })
        .count()
}

#[test]
fn ragavan_exiles_damaged_players_top_card_and_grants_cast_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let controller_top = scenario.add_card_to_library_top(P0, "Controller Top");
    let opponent_top = scenario.add_card_to_library_top(P1, "Opponent Bolt");
    let ragavan = scenario
        .add_creature(P0, "Ragavan, Nimble Pilferer", 2, 1)
        .from_oracle_text(RAGAVAN_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let spell = runner.state_mut().objects.get_mut(&opponent_top).unwrap();
        spell.card_types.core_types.push(CoreType::Instant);
        spell.mana_cost = ManaCost::zero();
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ragavan, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Ragavan attacking P1");

    drain_until_ragavan_trigger_resolves(&mut runner, Some(opponent_top));

    {
        let state = runner.state();
        assert_eq!(
            treasure_count(state, P0),
            1,
            "Ragavan must create one Treasure token for its controller"
        );
        assert_eq!(
            state.objects[&opponent_top].zone,
            Zone::Exile,
            "Ragavan must exile the damaged player's top card"
        );
        assert_eq!(
            state.objects[&controller_top].zone,
            Zone::Library,
            "Ragavan must not exile from its controller's library"
        );
        assert!(
            state.objects[&opponent_top]
                .casting_permissions
                .iter()
                .any(|permission| matches!(
                    permission,
                    CastingPermission::PlayFromExile {
                        granted_to: P0,
                        mode: CardPlayMode::Cast,
                        ..
                    }
                )),
            "the exiled card must receive a PlayFromExile grant for Ragavan's controller"
        );
        assert!(
            spell_objects_available_to_cast(state, P0).contains(&opponent_top),
            "the granted spell must surface on P0's cast path"
        );
    }

    let cast_outcome = runner.cast(opponent_top).resolve();
    cast_outcome.assert_zone(&[opponent_top], Zone::Graveyard);
    assert!(
        !spell_objects_available_to_cast(cast_outcome.state(), P0).contains(&opponent_top),
        "casting the granted spell must consume the exile-cast path"
    );
}

#[test]
fn ragavan_cast_permission_does_not_authorize_the_exiled_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opponent_land = scenario.add_card_to_library_top(P1, "Opponent Island");
    let ragavan = scenario
        .add_creature(P0, "Ragavan, Nimble Pilferer", 2, 1)
        .from_oracle_text(RAGAVAN_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&opponent_land)
        .expect("scenario land exists")
        .card_types
        .core_types
        .push(CoreType::Land);

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ragavan, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Ragavan attacking P1");
    drain_until_ragavan_trigger_resolves(&mut runner, Some(opponent_land));
    advance_to_post_combat_main_with_p0_priority(&mut runner);

    let card_id = runner.state().objects[&opponent_land].card_id;
    let battlefield_before = runner.state().battlefield.len();
    let lands_played_before = runner.state().lands_played_this_turn;
    {
        let state = runner.state();
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(state.stack.is_empty());
        assert!(matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0));
        assert_eq!(
            state.lands_played_this_turn, 0,
            "the normal land drop is unused"
        );
        assert!(state.objects[&opponent_land]
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to: P0,
                    mode: CardPlayMode::Cast,
                    ..
                }
            )));
        assert!(
            !exile_lands_playable_by_permission(state, P0)
                .iter()
                .any(|(object_id, _)| *object_id == opponent_land),
            "Ragavan's cast-only grant must not surface as a legal land action"
        );
    }

    let result = runner.act(GameAction::PlayLand {
        object_id: opponent_land,
        card_id,
    });
    assert!(
        result.is_err(),
        "the real land action must reject Ragavan's Cast grant"
    );
    let state = runner.state();
    assert_eq!(state.objects[&opponent_land].zone, Zone::Exile);
    assert_eq!(state.battlefield.len(), battlefield_before);
    assert_eq!(state.lands_played_this_turn, lands_played_before);

    let CastingPermission::PlayFromExile { mode, .. } = runner
        .state_mut()
        .objects
        .get_mut(&opponent_land)
        .expect("exiled land remains in the scenario")
        .casting_permissions
        .iter_mut()
        .find(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to: P0,
                    mode: CardPlayMode::Cast,
                    ..
                }
            )
        })
        .expect("Ragavan's parsed Cast permission remains attached")
    else {
        unreachable!("matched PlayFromExile permission")
    };
    *mode = CardPlayMode::Play;

    assert!(
        exile_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(object_id, _)| *object_id == opponent_land),
        "the same permission must authorize a land once its mode is Play"
    );
    runner
        .act(GameAction::PlayLand {
            object_id: opponent_land,
            card_id,
        })
        .expect("the public land action must accept a Play-mode exile permission");
    let state = runner.state();
    assert_eq!(state.objects[&opponent_land].zone, Zone::Battlefield);
    assert_eq!(state.battlefield.len(), battlefield_before + 1);
    assert_eq!(state.lands_played_this_turn, lands_played_before + 1);
}

#[test]
fn ragavan_creates_treasure_when_damaged_player_has_empty_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ragavan = scenario
        .add_creature(P0, "Ragavan, Nimble Pilferer", 2, 1)
        .from_oracle_text(RAGAVAN_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ragavan, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Ragavan attacking P1");

    drain_until_ragavan_trigger_resolves(&mut runner, None);

    assert_eq!(
        treasure_count(runner.state(), P0),
        1,
        "Ragavan must create its Treasure even when ExileTop finds no card"
    );
}
