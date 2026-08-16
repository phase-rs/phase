//! CR 723.2 + CR 723.1 + CR 608.2c: Secret of Bloodbending (TLA #69) — end-to-end
//! cast-pipeline proof of phase-scoped player control.
//!
//! Base leaf: "You control target opponent during their next combat phase."
//! (CR 723.2 → `ControlWindow::NextCombatPhase`). Paid override: "If this spell's
//! additional cost was paid, you control that player during their next turn
//! instead." (CR 723.1 → `ControlWindow::NextTurn`, swapped in via the
//! `AdditionalCostPaidInstead` sub-ability when the optional "you may waterbend
//! {10}" additional cost is paid).
//!
//! These are the non-vacuous full-pipeline proofs (parse → resolve → schedule →
//! pilot → release). The unpaid path additionally advances the target's turn and
//! asserts the owner decides before/after combat while the caster pilots combat
//! (the `finish_enter_phase` hook). Reverting the base-leaf window arm drops the
//! unpaid schedule to `Effect::Unimplemented` (no entry — test 7.6 red); reverting
//! the additional-cost swap or the "that player" anaphora parse schedules the
//! wrong window (test 7.7 red).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::turn_control::turn_decision_maker;
use engine::game::turns::advance_phase;
use engine::types::ability::{ControlWindow, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P0: PlayerId = PlayerId(0); // caster / controller
const P1: PlayerId = PlayerId(1); // targeted opponent

const ORACLE: &str = "As an additional cost to cast this spell, you may waterbend {10}.\n\
You control target opponent during their next combat phase. If this spell's additional cost was paid, you control that player during their next turn instead. (You see all cards that player could see and make all decisions for them.)\n\
Exile Secret of Bloodbending.";

/// Cast Secret of Bloodbending targeting P1, optionally paying the waterbend {10}
/// additional cost. Returns the built runner after the spell has fully resolved.
fn cast_secret(pay_waterbend: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if pay_waterbend {
        // Fund the {10} generic waterbend additional cost from the pool
        // (CR 601.2h — waterbend seeds the pool with eligible objects; a bare
        // pool covers the generic portion here).
        scenario.with_mana_pool(
            P0,
            (0..10)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect(),
        );
    }
    let secret = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Secret of Bloodbending", false, ORACLE);
        // Re-parse with the Waterbend keyword hint (matches the real card) and a
        // free base cost so the additional cost is the only mana under test.
        builder
            .with_mana_cost(engine::types::mana::ManaCost::generic(0))
            .from_oracle_text_with_keywords(&["Waterbend"], ORACLE)
            .id()
    };
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&secret].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: secret,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Secret of Bloodbending");

    for _ in 0..48 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_waterbend })
                    .expect("decide waterbend additional cost");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize mana payment from pool");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("target the opponent");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass to resolve");
            }
            other => panic!("unexpected cast window: {other:?}"),
        }
    }
    runner
}

/// Test 7.6 — unpaid cast schedules a NextCombatPhase control, then the owner
/// decides before/after combat and the caster pilots the combat phase.
#[test]
fn unpaid_schedules_next_combat_phase_and_pilots_combat() {
    let mut runner = cast_secret(false);

    // Parse + resolver chain: one NextCombatPhase entry bound to P1, piloted by P0.
    {
        let sched = &runner.state().scheduled_turn_controls;
        assert_eq!(
            sched.len(),
            1,
            "one scheduled control from the resolved spell"
        );
        assert_eq!(
            sched[0].window,
            ControlWindow::NextCombatPhase,
            "CR 723.2: unpaid → next-combat-phase window"
        );
        assert_eq!(sched[0].target_player, P1);
        assert_eq!(sched[0].controller, P0);
    }

    // Advance the targeted player's next turn and assert the control window.
    let mut events = Vec::new();
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.phase = Phase::Untap;
        st.turn_decision_controller = None;
    }
    // Untap -> Upkeep -> Draw -> PreCombatMain: owner decides before combat.
    for _ in 0..3 {
        advance_phase(runner.state_mut(), &mut events);
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        turn_decision_maker(runner.state()),
        P1,
        "owner decides before combat"
    );
    // PreCombatMain -> BeginCombat: caster pilots.
    advance_phase(runner.state_mut(), &mut events);
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert_eq!(
        turn_decision_maker(runner.state()),
        P0,
        "CR 723.2 + CR 507: caster pilots the controlled combat phase"
    );
    // Advance through combat to PostCombatMain: released, owner decides again.
    for _ in 0..8 {
        advance_phase(runner.state_mut(), &mut events);
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        assert_ne!(
            runner.state().phase,
            Phase::Cleanup,
            "reached cleanup without a postcombat main phase"
        );
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(
        turn_decision_maker(runner.state()),
        P1,
        "CR 511.3: control released at postcombat main — owner decides again"
    );
    assert!(
        runner.state().scheduled_turn_controls.is_empty(),
        "the NextCombatPhase entry is consumed at release"
    );
}

/// Test 7.7 — paying the waterbend additional cost swaps to the full-turn
/// (NextTurn) window, and Secret of Bloodbending exiles itself.
#[test]
fn paid_schedules_next_turn_and_self_exiles() {
    let runner = cast_secret(true);

    let sched = &runner.state().scheduled_turn_controls;
    assert_eq!(
        sched.len(),
        1,
        "one scheduled control from the resolved spell"
    );
    assert_eq!(
        sched[0].window,
        ControlWindow::NextTurn,
        "CR 723.1: paying the additional cost swaps to the full-turn window"
    );
    assert_eq!(sched[0].target_player, P1);
    assert_eq!(sched[0].controller, P0);

    // Self-exile clause resolved (proves the full chain, not just the swap).
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|obj| { obj.name == "Secret of Bloodbending" && obj.zone == Zone::Exile }),
        "Secret of Bloodbending exiles itself on resolution"
    );
}
