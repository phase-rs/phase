//! CR 603.2c + CR 120.4b: per-SOURCE damage aggregation — Pain Magnification,
//! "Whenever an opponent is dealt 3 or more damage by a single source, that
//! player discards a card."
//!
//! CR 603.2c is the operative rule for the two-attackers row: an ability
//! triggers once per occurrence of its trigger event, and "can trigger
//! repeatedly if one event contains multiple occurrences" — so one damage
//! event carrying two qualifying sources is two occurrences, not one summed
//! one. CR 120.4b names the whole-event default that the "…by a single
//! source" tail narrows away from.
//!
//! The "…by a single source" tail narrows the threshold's aggregation domain
//! from the whole simultaneous damage event (the received-damage grammar's
//! default, CR 120.4b) to one source's share. So two attackers each dealing 2
//! to the same player must NOT fire, even though the event total is 4; and two
//! attackers each dealing 3 must fire TWICE, once per qualifying source.
//! The card's own ruling states the per-source reading directly.
//!
//! These rows are the runtime carriers of the scope axis. Both are broken by
//! the same single mutation: make `parse_single_source_scope` emit
//! `WholeEvent` instead of `PerSource`, and the fold groups both damage events
//! under the single recipient P1 — 2 + 2 = 4 fires where 0 is correct, and
//! 3 + 3 = 6 collapses to one firing where 2 are correct.

use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::{run_combat, GameScenario, P0, P1};

/// Verbatim Oracle text.
const PAIN_MAGNIFICATION: &str =
    "Whenever an opponent is dealt 3 or more damage by a single source, that player discards a card.";

/// Read a player's hand size straight from live state. `GameRunner.state` is a
/// private field, so `runner.state()` is the supported route; `hand_count` on
/// `ScenarioResult` is not reachable from a `GameScenario::build` fixture.
/// Generalized from `batched_trigger_subject_count.rs`.
fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn first_hand_object(state: &GameState, player: PlayerId) -> ObjectId {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .front()
        .copied()
        .expect("hand not empty")
}

/// Drain resolution, alternating priority passes with discard choices until
/// neither remains.
///
/// `advance_until_stack_empty` handles only `OrderTriggers` and a narrow
/// `EffectZoneChoice`; it does NOT clear `WaitingFor::DiscardChoice`, and
/// `scenario.rs`'s own discard drain sits behind `resolve()`, which a
/// combat-driven fixture never enters. Whether a prompt is raised at all
/// depends on hand size versus discard count (a 1-card hand discarding 1 may
/// auto-resolve), so this loop is written to be correct either way: if no
/// prompt appears it simply never takes that arm.
fn drain_resolution(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { player, .. } => {
                let pick = first_hand_object(runner.state(), player);
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("discard choice should succeed");
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                runner.advance_until_stack_empty();
            }
            _ => break,
        }
    }
}

#[test]
fn two_sources_two_each_does_not_fire() {
    // V7 — two sources each dealing 2 to P1. The event total is 4, but no
    // SINGLE source dealt 3, so a per-source threshold must not fire.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Pain Magnification", PAIN_MAGNIFICATION);
    let attacker_a = scenario.add_creature(P0, "Attacker A", 2, 2).id();
    let attacker_b = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    scenario.add_card_to_hand(P1, "Discardable");
    let mut runner = scenario.build();

    // Without a card to lose, "hand unchanged" would hold under every possible
    // implementation and the assertion below would be vacuous.
    let hand_before = hand_count(&runner, P1);
    assert!(
        hand_before >= 1,
        "negative is vacuous unless P1 can actually lose a card"
    );
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker_a, attacker_b], vec![]);
    drain_resolution(&mut runner);

    // State-machine reachability: a stalled harness leaves life unchanged, which
    // is indistinguishable from "the trigger correctly did not fire".
    assert_eq!(
        runner.life(P1),
        life_before - 4,
        "both 2-damage instances must actually reach P1"
    );
    assert_eq!(
        hand_count(&runner, P1),
        hand_before,
        "no single source dealt 3, so a per-source threshold must not fire"
    );
}

#[test]
fn single_source_three_fires_once() {
    // The paired positive for the row above: same shape, one 3-power source.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Pain Magnification", PAIN_MAGNIFICATION);
    let attacker = scenario.add_creature(P0, "Lone Attacker", 3, 3).id();
    scenario.add_card_to_hand(P1, "Discardable");
    let mut runner = scenario.build();

    let hand_before = hand_count(&runner, P1);
    assert!(hand_before >= 1, "P1 must hold a card to discard");
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker], vec![]);
    drain_resolution(&mut runner);

    assert_eq!(
        runner.life(P1),
        life_before - 3,
        "the 3-damage instance must actually reach P1"
    );
    assert_eq!(
        hand_count(&runner, P1),
        hand_before - 1,
        "a single source dealing 3 fires the trigger exactly once"
    );
}

#[test]
fn two_sources_three_each_fires_twice() {
    // V8 — each source independently clears the threshold, so the trigger fires
    // once PER SOURCE. Under a `WholeEvent` mis-scoping the two events would
    // collapse into one 6-damage group under the single recipient P1 and this
    // would be 1 discard, not 2.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Pain Magnification", PAIN_MAGNIFICATION);
    let attacker_a = scenario.add_creature(P0, "Attacker A", 3, 3).id();
    let attacker_b = scenario.add_creature(P0, "Attacker B", 3, 3).id();
    scenario.add_card_to_hand(P1, "Discardable One");
    scenario.add_card_to_hand(P1, "Discardable Two");
    let mut runner = scenario.build();

    // Two cards, or the "2" below would be capped by the hand rather than by
    // per-source semantics.
    let hand_before = hand_count(&runner, P1);
    assert!(
        hand_before >= 2,
        "P1 must hold two cards or the discard count is hand-capped, not semantics-capped"
    );
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker_a, attacker_b], vec![]);
    drain_resolution(&mut runner);

    assert_eq!(
        runner.life(P1),
        life_before - 6,
        "both 3-damage instances must actually reach P1"
    );
    assert_eq!(
        hand_count(&runner, P1),
        hand_before - 2,
        "two qualifying sources fire the per-source trigger twice"
    );
}
