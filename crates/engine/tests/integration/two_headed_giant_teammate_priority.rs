//! CR 805.5a: a player may act when their TEAM has priority, not only the
//! team's representative. In Two-Headed Giant the engine parks priority on the
//! team representative (P0 / P2); the teammate (P1 / P3) must still be able to
//! play lands, cast spells, and activate abilities on the team's turn.

use engine::ai_support;
use engine::game::engine as game_engine;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn team_turn_board() -> GameScenario {
    let mut sc = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    sc.at_phase(Phase::PreCombatMain);
    sc
}

/// The teammate sees their own legal actions while the representative holds
/// the team's priority window.
#[test]
fn teammate_has_legal_actions_on_team_priority() {
    let mut sc = team_turn_board();
    let land = sc.add_land_to_hand(P1, "Forest").id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P1);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::PlayLand { object_id, .. } if *object_id == land
        )),
        "P1 must be offered their own land on the team's turn, got {actions:?}"
    );
}

/// The teammate's land play is accepted through the authenticated boundary
/// while the representative holds priority.
#[test]
fn teammate_land_play_accepted_on_team_priority() {
    let mut sc = team_turn_board();
    let land = sc.add_land_to_hand(P1, "Forest").id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let card_id = runner.state().objects[&land].card_id;
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::PlayLand {
            object_id: land,
            card_id,
        },
    )
    .expect("teammate land play must be accepted on team priority");
    assert_eq!(
        runner.state().objects[&land].zone,
        engine::types::zones::Zone::Battlefield
    );
}

/// The teammate can cast an instant while the representative holds priority.
#[test]
fn teammate_instant_cast_accepted_on_team_priority() {
    use engine::types::ability::TargetRef;
    use engine::types::game_state::{CastPaymentMode, StackEntryKind};

    let mut sc = team_turn_board();
    let bolt = sc.add_bolt_to_hand(P1);
    let target = sc.add_creature(P2, "Target", 0, 8).id();
    sc.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let card_id = runner.state().objects[&bolt].card_id;
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("teammate cast must be accepted on team priority");
    // The cast parks on the teammate's own targeting step, not the
    // representative's.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player, .. } if player == P1
        ),
        "teammate cast must park on P1 targeting, got {:?}",
        runner.state().waiting_for
    );
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        },
    )
    .expect("teammate targeting accepted");
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == bolt
            && matches!(entry.kind, StackEntryKind::Spell { .. })
            && entry.controller == P1),
        "teammate spell must reach the stack under P1 control"
    );
}

/// CR 805.5b: one teammate passing does not pass for the team. Both living
/// members must pass before priority leaves the team.
#[test]
fn team_pass_requires_both_teammates() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // First teammate passes: the team still holds priority.
    game_engine::apply(runner.state_mut(), P0, GameAction::PassPriority)
        .expect("representative pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0 || player == P1
        ),
        "team must retain priority until both members pass, got {:?}",
        runner.state().waiting_for
    );

    // Second teammate passes: now the opposing team receives priority.
    game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
        .expect("teammate pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2 || player == P3
        ),
        "opposing team must receive priority after both teammates pass, got {:?}",
        runner.state().waiting_for
    );
}
