//! CR 602.2a + CR 732.3: an on-stack activation is a play of its own seat, beside another seat's
//! cast, whatever its ability tree mints. Driven on the committed
//! `witherbloom_altar_sprout_swarm_4p` dump: seat 0's buyback Sprout Swarm cast (CR 702.27a) opens a
//! period, then another seat activates with the spell still on the stack.

use engine::game::scenario::GameRunner;
use engine::game::{EntryKind, PlayLocus};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::loop_shortcut::{gunzip_dump, restore_dump};

const SEAT_0: PlayerId = PlayerId(0);
const SEAT_1: PlayerId = PlayerId(1);
const SEAT_2: PlayerId = PlayerId(2);

fn witherbloom_board() -> GameState {
    restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/witherbloom_altar_sprout_swarm_4p.json.gz"
    )))
}

/// The window's plays, each with the seat that made it.
fn plays(state: &GameState) -> Vec<(PlayerId, PlayLocus)> {
    engine::game::play_trace_view(state).map_or_else(Vec::new, |view| {
        view.entries
            .into_iter()
            .filter_map(|entry| match entry.kind {
                EntryKind::Play { locus, .. } => Some((entry.seat, locus)),
                _ => None,
            })
            .collect()
    })
}

fn object_named(state: &GameState, name: &str, controller: PlayerId, zone: Zone) -> ObjectId {
    state
        .objects
        .values()
        .filter(|o| o.name == name && o.controller == controller && o.zone == zone)
        .map(|o| o.id)
        .min()
        .unwrap_or_else(|| panic!("{name} controlled by {controller:?} in {zone:?}"))
}

/// Answers a target prompt with the first legal target.
fn choose_first_target(runner: &mut GameRunner) {
    let pick = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|a| matches!(a, GameAction::ChooseTarget { target: Some(_) }))
        .expect("a legal target");
    runner.act(pick).expect("target accepted");
}

/// Seat 0 casts Sprout Swarm with buyback, convoking an untapped Saproling, in seat 2's precombat
/// main phase; priority then passes until `holder` holds it with the spell still on the stack.
fn foreign_recast_period_with_priority_at(holder: PlayerId) -> GameRunner {
    let mut runner = GameRunner::from_state(witherbloom_board());
    for _ in 0..600 {
        let s = runner.state();
        if s.active_player == SEAT_2
            && s.phase == Phase::PreCombatMain
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == SEAT_0)
        {
            break;
        }
        let action = match &s.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected prompt on the way to seat 2's main phase: {other:?}"),
        };
        runner.act(action).expect("pass toward seat 2's main phase");
    }
    let state = runner.state();
    assert!(
        state.active_player == SEAT_2
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == SEAT_0),
        "seat 0 holds priority in seat 2's precombat main phase"
    );
    let sprout = object_named(state, "Sprout Swarm", SEAT_0, Zone::Hand);
    let fodder = state
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Saproling" && o.controller == SEAT_0 && !o.tapped)
        })
        .expect("an untapped Saproling to convoke");

    let state = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder])
        .commit()
        .state()
        .clone();
    let mut runner = GameRunner::from_state(state);
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == holder)
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass with the spell on the stack");
    }
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == holder),
        "{holder:?} holds priority"
    );
    assert!(
        state.stack.iter().any(|e| e.source_id == sprout),
        "Sprout Swarm is still on the stack"
    );
    assert_eq!(
        plays(state),
        vec![(SEAT_0, PlayLocus::Cast(sprout))],
        "the buyback cast is seat 0's play"
    );
    runner
}

#[test]
fn activation_reaching_investigate_below_its_root_opens_its_own_period() {
    let mut runner = foreign_recast_period_with_priority_at(SEAT_2);
    let greyfax = object_named(
        runner.state(),
        "Inquisitor Greyfax",
        SEAT_2,
        Zone::Battlefield,
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: greyfax,
            ability_index: 0,
        })
        .expect("Greyfax activates");
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        choose_first_target(&mut runner);
    }

    let state = runner.state();
    assert_eq!(
        state.objects[&greyfax].zone,
        Zone::Battlefield,
        "the source stays on the battlefield"
    );
    assert_eq!(
        plays(state).last(),
        Some(&(SEAT_2, PlayLocus::Activate(greyfax, 0))),
        "the activation is seat 2's play"
    );

    for _ in 0..64 {
        let state = runner.state();
        match &state.waiting_for {
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass while the stack resolves");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                choose_first_target(&mut runner)
            }
            _ => break,
        }
    }
    let state = runner.state();
    assert!(
        state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == SEAT_2),
        "once the stack resolves, seat 2 holds priority on an empty stack: {:?}",
        state.waiting_for
    );
}

#[test]
fn activation_reaching_no_token_clears_the_foreign_period() {
    let mut runner = foreign_recast_period_with_priority_at(SEAT_1);
    let bloodcaster = object_named(
        runner.state(),
        "Marshland Bloodcaster",
        SEAT_1,
        Zone::Battlefield,
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: bloodcaster,
            ability_index: 0,
        })
        .expect("Marshland Bloodcaster activates");

    let state = runner.state();
    assert_eq!(
        state.objects[&bloodcaster].zone,
        Zone::Battlefield,
        "the source stays on the battlefield"
    );
    assert_eq!(
        plays(state).last(),
        Some(&(SEAT_1, PlayLocus::Activate(bloodcaster, 0))),
        "the activation is seat 1's play"
    );
}
