//! `FilterProp::BlockStatus` (CR 509.1h): "blocked" and "unblocked" are the two
//! complementary attacker states, and "blocked" is sticky. Drives the real
//! declare-blockers step through `GameScenario`, never hand-built combat state.

use engine::game::combat::AttackTarget;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AttackerBlockStatus, FilterProp, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WARPATH: &str = "Warpath deals 3 damage to each blocking creature and each blocked creature.";
const MURDER: &str = "Destroy target creature.";

fn drive_declare_blockers(
    runner: &mut GameRunner,
    attacks: Vec<(ObjectId, AttackTarget)>,
    blocks: Vec<(ObjectId, ObjectId)>,
) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .act(GameAction::DeclareBlockers {
            assignments: blocks,
        })
        .expect("DeclareBlockers should succeed");
}

fn block_status_filter(status: AttackerBlockStatus) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::BlockStatus { status }]),
    )
}

/// A Warpath-shaped spell damages the blocker and the blocked attacker but not
/// the unblocked attacker.
#[test]
fn warpath_shape_hits_blocker_and_blocked_attacker_but_not_unblocked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blocked = scenario.add_creature(P0, "Blocked Attacker", 2, 3).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 2, 3).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 3).id();
    let warpath = scenario
        .add_spell_to_hand_from_oracle(P0, "Warpath", true, WARPATH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (blocked, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(blocker, blocked)],
    );

    // CR 509.2: the active player gets priority after blockers are declared.
    runner.cast(warpath).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&blocker].zone, Zone::Graveyard);
    assert_eq!(state.objects[&blocked].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&unblocked].zone,
        Zone::Battlefield,
        "an unblocked attacker matches neither the blocking nor the blocked leg"
    );
}

/// CR 509.1h: once blocked, an attacker stays blocked after its blocker leaves
/// combat, and never matches Unblocked.
#[test]
fn attacker_stays_blocked_after_blocker_is_removed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let unblocked = scenario.add_creature(P0, "Unblocked Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    drive_declare_blockers(
        &mut runner,
        vec![
            (attacker, AttackTarget::Player(P1)),
            (unblocked, AttackTarget::Player(P1)),
        ],
        vec![(blocker, attacker)],
    );
    runner.cast(murder).target_object(blocker).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(
        state.objects[&blocker].zone,
        Zone::Graveyard,
        "reach guard: the blocker must be gone"
    );
    let ctx = FilterContext::from_source_with_controller(attacker, P0);
    let blocked_filter = block_status_filter(AttackerBlockStatus::Blocked);
    let unblocked_filter = block_status_filter(AttackerBlockStatus::Unblocked);
    assert!(matches_target_filter(
        state,
        attacker,
        &blocked_filter,
        &ctx
    ));
    assert!(!matches_target_filter(
        state,
        attacker,
        &unblocked_filter,
        &ctx
    ));
    assert!(matches_target_filter(
        state,
        unblocked,
        &unblocked_filter,
        &ctx
    ));
    assert!(!matches_target_filter(
        state,
        unblocked,
        &blocked_filter,
        &ctx
    ));
}
