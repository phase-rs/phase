//! Regression: a scry that triggers 2+ simultaneous "whenever you scry"
//! abilities must still surface the `ScryChoice` prompt — and the triggers must
//! still fire after the choice resolves.
//!
//! Reported in-game (Track Down with Matoya, Archon Elder + Elrond, Master of
//! Healing both on the battlefield): "Scry is now a no-op, the modal never
//! appeared." Root cause: when a scry effect set `WaitingFor::ScryChoice`
//! mid-resolution, the post-resolution trigger scan collected the scry-watching
//! triggers and — with 2+ same-controller triggers — set
//! `WaitingFor::OrderTriggers`, overwriting the pending `ScryChoice` (CR 603.3b
//! violation: those triggers must wait until the spell finishes resolving). A
//! single trigger used the non-ordering dispatch path and didn't clobber, so
//! the bug only manifested with two or more — which is why plain scry (and Opt
//! with only one watcher present) appeared to work.
//!
//! CR 603.3b: a triggered ability is put on the stack the next time a player
//! would receive priority — i.e. AFTER the current spell, including its scry
//! choice, finishes resolving.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// CR 603.3b + CR 701.22a: With two "whenever you scry" triggers on the
/// battlefield, casting a scry spell must still pause at `ScryChoice` (the
/// modal the user picks from), not jump straight into trigger handling.
#[test]
fn scry_with_two_watchers_still_prompts_and_fires_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two DISTINCT "whenever you scry" watchers (draw vs. gain life). They must
    // differ so the two same-controller scry-watcher triggers still surface the
    // CR 603.3b OrderTriggers prompt — the exact path that clobbered ScryChoice
    // (identical no-input triggers now auto-order, which would bypass the
    // clobber path this test guards).
    scenario.add_creature_from_oracle(
        P0,
        "Scry Watcher A",
        2,
        2,
        "Whenever you scry, draw a card.",
    );
    scenario.add_creature_from_oracle(
        P0,
        "Scry Watcher B",
        2,
        2,
        "Whenever you scry, you gain 1 life.",
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Augury Owl Spell", false, "Scry 2.")
        .id();
    for name in ["Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&spell).unwrap().card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the scry spell");
    runner.advance_until_stack_empty();

    // The spell left the hand on cast; after it resolves the engine MUST be
    // paused on the scry choice, not on a trigger-ordering / target prompt.
    let WaitingFor::ScryChoice { player, cards } = runner.state().waiting_for.clone() else {
        panic!(
            "scry must pause at ScryChoice even with two scry-watchers, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0);
    assert_eq!(cards.len(), 2, "Scry 2 looks at the top two cards");

    let hand_after_cast = runner.state().players[0].hand.len();
    let life_after_cast = runner.state().players[0].life;

    // Submit the scry (keep both on top), then let the now-unparked triggers
    // resolve through any ordering prompt.
    runner
        .act(GameAction::SelectCards { cards })
        .expect("submit the scry ordering");
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        }
        runner.advance_until_stack_empty();
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
    }

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must settle back to Priority, got {}",
        runner.waiting_for_kind()
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_after_cast + 1,
        "the 'whenever you scry, draw' trigger must fire after the scry choice"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_after_cast + 1,
        "the 'whenever you scry, gain 1 life' trigger must fire after the scry choice"
    );
}
