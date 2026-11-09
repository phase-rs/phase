//! Production controls for source-owner shuffles and properties-only targets.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

// Full Oracle verified against MTGJSON AtomicCards.json.
const ETRATA: &str = "Etrata can't be blocked.\nWhenever Etrata deals combat damage to a player, exile target creature that player controls and put a hit counter on that card. That player loses the game if they own three or more exiled cards with hit counters on them. Etrata's owner shuffles Etrata into their library.";
const FORGE: &str = "{T}: Add {C}.\n{T}: Choose target commander that entered this turn. Put a +1/+1 counter on it if it's a creature and a loyalty counter on it if it's a planeswalker.";

#[test]
// The unrelated Etrata loss-condition gap is not a support claim here. A third
// player keeps the source-owner instruction observable even if that gap eliminates
// the damaged player; the targeted exile must still precede any elimination.
fn etrata_shuffles_the_source_not_its_earlier_exile_target() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = scenario
        .add_creature_from_oracle(P2, "Etrata, the Silencer", 3, 5, ETRATA)
        .controlled_by(P0)
        .id();
    let victim = scenario.add_creature(P1, "Exile target", 2, 2).id();
    let other_victim = scenario
        .add_creature(P1, "Other exile candidate", 2, 2)
        .id();
    scenario.with_library_top(P0, &["Controller's card"]);
    scenario.with_library_top(P1, &["Defender's card"]);
    scenario.with_library_top(P2, &["Owner A", "Owner B", "Owner C"]);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etrata, AttackTarget::Player(P1))])
        .expect("the controlled Etrata can attack another player");
    runner.combat_damage();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).expect("no blockers declared");
        runner.combat_damage();
    }
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(target_slots.len(), 1);
            assert_eq!(target_slots[0].legal_targets.len(), 2);
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(other_victim)));
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(victim)));
        }
        other => panic!(
            "Etrata's combat trigger must announce its exile target: {other:?}; phase={:?}",
            runner.state().phase,
        ),
    }
    let mut events = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("the damaged player's creature is a legal target")
        .events;
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve the trigger")
                .events,
        );
    }
    let exile = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Exile,
                    ..
                } if *object_id == victim
            )
        })
        .expect("the targeted instruction exiles the victim");
    if let Some(loss) = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::PlayerLost { player_id } | GameEvent::PlayerEliminated { player_id }
                if *player_id == P1
        )
    }) {
        assert!(
            exile < loss,
            "targeted exile must precede any unrelated elimination"
        );
    }
    let state = runner.state();
    // CR 201.5 + CR 400.3: the named source is distinct from the earlier target,
    // and goes to its owner's library even under another player's control.
    assert_eq!(state.objects[&etrata].zone, Zone::Library);
    assert!(state.players[P2.0 as usize].library.contains(&etrata));
    assert!(!state.players[P0.0 as usize].library.contains(&etrata));
    let shuffled_players: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(
        !shuffled_players.is_empty(),
        "the source owner actually shuffles"
    );
    assert!(shuffled_players.iter().all(|player| *player == P2));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

#[test]
fn forge_announces_only_commanders_that_entered_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forge = scenario
        .add_land_from_oracle(P0, "Forge of Heroes", FORGE)
        .id();
    let eligible = scenario
        .add_creature(P0, "New commander", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let other_eligible = scenario
        .add_creature(P1, "Opponent's new commander", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let old_commander = scenario
        .add_creature(P0, "Old commander", 2, 2)
        .commander()
        .id();
    let ordinary = scenario
        .add_creature(P0, "New noncommander", 2, 2)
        .with_summoning_sickness()
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: forge,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Forge's targeted ability can be activated");
    // CR 115.1c + CR 903.3: commander designation and entry timing are both
    // meaningful properties even without a card-type or controller restriction.
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1);
            let legal = &target_slots[0].legal_targets;
            assert_eq!(legal.len(), 2);
            assert!(legal.contains(&TargetRef::Object(eligible)));
            assert!(legal.contains(&TargetRef::Object(other_eligible)));
            assert!(!legal.contains(&TargetRef::Object(old_commander)));
            assert!(!legal.contains(&TargetRef::Object(ordinary)));
        }
        other => panic!("Forge must request its properties-only target: {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(eligible)),
        })
        .expect("the new commander may be selected");
    let announced = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("the activation with its selected target is on the stack");
    assert_eq!(announced.targets, vec![TargetRef::Object(eligible)]);
    // This row covers announcement/activation, not the pre-existing unsupported
    // conditional dual-counter tail in the full Oracle.
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}
