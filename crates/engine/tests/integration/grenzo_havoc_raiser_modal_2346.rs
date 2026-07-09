//! Grenzo, Havoc Raiser (issue #2346) — runtime proof that the bullet-line
//! triggered-modal "that player" anaphor binds to the DAMAGED player.
//!
//! Grenzo's printed Oracle text:
//!   "Whenever a creature you control deals combat damage to a player, choose
//!    one —
//!    • Goad target creature that player controls.
//!    • Exile the top card of that player's library. ..."
//!
//! Those bullet modes route through the `TriggeredModal` block path (not the
//! inline `"; or"` path). Before the fix, "that player controls" fell back to
//! `ControllerRef::You` (Grenzo's controller), so the Goad mode could only
//! target creatures the ACTIVE player controlled — the wrong player.
//!
//! CR 109.4 + CR 115.1 + CR 506.2: a "deals combat damage to a
//! player" trigger establishes the damaged player as the triggering player;
//! "that player controls" must resolve to them. This test fires the trigger
//! by having P0's attacker deal combat damage to P1, selects the Goad mode,
//! and asserts the only legal Goad targets are creatures P1 (the damaged
//! player) controls — never P0's.
//!
//! Revert-fail: with the scope fix reverted, the Goad slot's legal targets are
//! P0's creature (the bug), so the "no P0 creature is legal / P1's creature is
//! legal" assertions fail.

use engine::game::combat::AttackTarget;
use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const GRENZO_ORACLE: &str = "Whenever a creature you control deals combat damage to a player, choose one \u{2014}\n\u{2022} Goad target creature that player controls.\n\u{2022} Exile the top card of that player's library.";

#[test]
fn grenzo_bullet_modal_goad_targets_damaged_players_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Grenzo (the trigger source) plus an attacker.
    scenario.add_creature_from_oracle(P0, "Grenzo, Havoc Raiser", 2, 2, GRENZO_ORACLE);
    let attacker = scenario.add_creature(P0, "Goblin Raider", 2, 2).id();
    // P0 ALSO controls a creature — the pre-fix bug would make THIS the only
    // legal Goad target (controller == You).
    let p0_other = scenario.add_creature(P0, "Caster Sidekick", 1, 1).id();
    // P1 (the player who will be damaged) controls TWO creatures — the Goad mode
    // must be able to target them. Two are used so the engine surfaces a real
    // target-selection prompt (a single legal target auto-resolves).
    let p1_creature = scenario.add_creature(P1, "Foe Bear", 2, 2).id();
    let p1_creature_b = scenario.add_creature(P1, "Foe Wolf", 3, 1).id();
    // Library top so the (unused) exile mode would have something to exile.
    scenario.with_library_top(P1, &["Top Card"]);

    let mut runner = scenario.build();

    // Advance to declare-attackers and swing at P1.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // Drive forward: combat damage to P1 fires Grenzo's trigger. The trigger
    // controller (P0) is prompted to order triggers, choose the mode, then
    // select a target. Select the Goad mode (index 0) and capture the legal
    // targets the engine offers for the Goad slot.
    let mut goad_legal_targets: Option<Vec<TargetRef>> = None;
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order triggers");
            }
            WaitingFor::AbilityModeChoice { .. } | WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("select Goad mode");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // The current slot's legal targets are the discriminating proof.
                let slot = target_slots
                    .get(selection.current_slot)
                    .expect("a current target slot");
                goad_legal_targets = Some(slot.legal_targets.clone());
                break;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blocks");
            }
            _ => break,
        }
    }

    let legal = goad_legal_targets
        .expect("Grenzo's Goad mode must reach a target-selection prompt after combat damage");

    let legal_objects: Vec<_> = legal
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();

    // The damaged player (P1) is "that player"; only their creatures
    // are legal Goad targets.
    assert!(
        legal_objects.contains(&p1_creature) && legal_objects.contains(&p1_creature_b),
        "both of P1's creatures (the damaged player's) must be legal Goad targets, got {legal_objects:?}",
    );
    assert!(
        !legal_objects.contains(&p0_other),
        "P0's creature must NOT be a legal Goad target — 'that player' is the damaged player (P1), not Grenzo's controller (revert-fail proof), got {legal_objects:?}",
    );

    // Stronger: every legal Goad object is controlled by P1, none by P0.
    for id in &legal_objects {
        let controller = runner.state().objects.get(id).map(|o| o.controller);
        assert_eq!(
            controller,
            Some(P1),
            "every legal Goad target must be controlled by the damaged player P1, object {id:?} controller {controller:?}",
        );
    }
}
