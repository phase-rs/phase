//! Liliana, Dreadhorde General — multi-death "Whenever a creature you control
//! dies" trigger (issue #1505).
//!
//! Oracle: "Whenever a creature you control dies, draw a card."
//!
//! Bug: when multiple creatures the controller controlled died in one
//! simultaneous batch (board wipe / mass SBA destruction), the dies trigger
//! fired only once instead of once per death. Per CR 603.2c each event that
//! satisfies the trigger condition produces its own trigger instance, so three
//! simultaneous deaths must produce three independent trigger instances and
//! three drawn cards.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 603.2c: "An ability triggers only once each time its trigger event
//!     occurs. However, it can trigger repeatedly if one event contains
//!     multiple occurrences." Three creatures dying simultaneously is three
//!     occurrences of the trigger event, not one.
//!   - CR 603.3: Once an ability has triggered, its controller puts it on the
//!     stack the next time a player would receive priority — so three
//!     occurrences put three independent trigger instances on the stack.
//!   - CR 603.3b: When one player controls multiple simultaneously-triggered
//!     abilities, they go on the stack in any order that player chooses.
//!     Indistinguishable instances may be auto-ordered (commit 7fdeac25).
//!   - CR 704.5g + CR 704.7: A creature with lethal damage marked on it is
//!     destroyed; SBAs are checked simultaneously and resolve as a single
//!     simultaneous event, so three lethal-damage creatures all die in one
//!     batch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Drive any auto-ordering / priority passes until the stack drains.
fn drain_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// CR 603.2c regression: three creatures dying simultaneously produces three
/// independent dies-trigger instances on Liliana's "Whenever a creature you
/// control dies, draw a card." A wrap-up board-wipe scenario marks lethal
/// damage on three P0 creatures so one SBA pass destroys them as a single
/// simultaneous event.
#[test]
fn liliana_dreadhorde_general_draws_once_per_simultaneous_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Liliana stand-in carries only the trigger we are testing. Modeled as a
    // creature so `add_creature_from_oracle` installs the dies trigger; the
    // observer's own card type is irrelevant to whether the trigger fires.
    let liliana_id = scenario
        .add_creature_from_oracle(
            P0,
            "Liliana, Dreadhorde General",
            1,
            6,
            "Whenever a creature you control dies, draw a card.",
        )
        .id();

    // Three vanilla creatures the trigger's controller controls.
    let bear_a = scenario.add_vanilla(P0, 2, 2);
    let bear_b = scenario.add_vanilla(P0, 2, 2);
    let bear_c = scenario.add_vanilla(P0, 2, 2);

    // Opponent creature that should NOT cause a draw (filter: "creature you
    // control"). Discriminator — confirms we are not just counting all dies.
    let opp_bear = scenario.add_vanilla(P1, 2, 2);

    // Library: 5 named cards so P0 can draw up to 4 (3 expected + headroom).
    let _ = scenario.add_card_to_library_top(P0, "Top1");
    let _ = scenario.add_card_to_library_top(P0, "Top2");
    let _ = scenario.add_card_to_library_top(P0, "Top3");
    let _ = scenario.add_card_to_library_top(P0, "Top4");
    let _ = scenario.add_card_to_library_top(P0, "Top5");

    let mut runner = scenario.build();

    // Hand size before the board wipe so we can measure draws.
    let p0_idx = runner
        .state()
        .players
        .iter()
        .position(|p| p.id == P0)
        .expect("P0 exists");
    let hand_before = runner.state().players[p0_idx].hand.len();

    // Mark lethal damage on all four creatures (board wipe simulation).
    // Liliana stays at 1 damage (sublethal) so she survives — the test is about
    // observers that survive while their controllers' creatures die. SBA runs
    // once and destroys all four in a single simultaneous event.
    for &id in &[bear_a, bear_b, bear_c, opp_bear] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .damage_marked = 2;
    }

    // Apply SBA. This emits ZoneChanged events with `co_departed` stamped on
    // each so they are recognized as simultaneous (CR 704.7).
    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);

    // Fire dies triggers from those SBA events. CR 603.3b auto-ordering kicks
    // in here: the three indistinguishable Liliana triggers must auto-order
    // and dispatch directly to the stack rather than waiting on an
    // `OrderTriggers` prompt.
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);

    // Stack should now hold three Liliana trigger instances — one per P0
    // creature death. The opponent's creature death is filtered out.
    let liliana_trigger_count = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. }
                    if *source_id == liliana_id
            )
        })
        .count();
    assert_eq!(
        liliana_trigger_count, 3,
        "CR 603.2c: three simultaneous P0 creature deaths must put three \
         independent Liliana dies-trigger instances on the stack; got {liliana_trigger_count}"
    );

    // Drain priority passes / auto-order prompts until the stack empties.
    drain_stack(&mut runner);

    // After all triggers resolve, P0 has drawn exactly 3 cards.
    let hand_after = runner.state().players[p0_idx].hand.len();
    assert_eq!(
        hand_after - hand_before,
        3,
        "Liliana must draw exactly one card per simultaneous P0 creature death \
         (CR 603.2c); hand grew by {}",
        hand_after - hand_before
    );
}

/// Sanity guard: the prior single-death path still works after the multi-death
/// fix. One creature dies → exactly one trigger instance → one card drawn.
#[test]
fn liliana_dreadhorde_general_single_death_still_draws_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let liliana_id = scenario
        .add_creature_from_oracle(
            P0,
            "Liliana, Dreadhorde General",
            1,
            6,
            "Whenever a creature you control dies, draw a card.",
        )
        .id();
    let lone_bear = scenario.add_vanilla(P0, 2, 2);

    let _ = scenario.add_card_to_library_top(P0, "Top1");
    let _ = scenario.add_card_to_library_top(P0, "Top2");

    let mut runner = scenario.build();
    let p0_idx = runner
        .state()
        .players
        .iter()
        .position(|p| p.id == P0)
        .expect("P0 exists");
    let hand_before = runner.state().players[p0_idx].hand.len();

    runner
        .state_mut()
        .objects
        .get_mut(&lone_bear)
        .unwrap()
        .damage_marked = 2;

    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);

    let liliana_trigger_count = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. }
                    if *source_id == liliana_id
            )
        })
        .count();
    assert_eq!(liliana_trigger_count, 1, "single death = one trigger");

    drain_stack(&mut runner);

    let hand_after = runner.state().players[p0_idx].hand.len();
    assert_eq!(
        hand_after - hand_before,
        1,
        "single P0 creature death must draw exactly one card"
    );
}
