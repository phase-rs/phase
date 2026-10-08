//! Bombur, Gentle Dreamer (HOB): "Storied (If you control three or more
//! artifacts, legendaries, and/or Sagas, you have an enduring story for the
//! rest of the game.) Bombur doesn't untap during your untap step unless you
//! have an enduring story."
//!
//! CR 502.3 (an effect can keep a permanent from untapping during the untap
//! step) + CR 702.195a-b (Storied grants the "enduring story" designation once
//! its controller controls the Storied permanent plus three or more historic
//! permanents; the designation persists for the rest of the game). "Unless" is
//! a negative-polarity conditional gate — the restriction applies precisely
//! when the trailing condition is false — and per CR 611.3a that condition is
//! re-evaluated dynamically at every untap step rather than "locked in" once.
//!
//! Two-sided regression: "doesn't untap ... unless [condition]" is a negative
//! conditional — the restriction (staying tapped) is the DEFAULT, and the
//! "unless" clause is the exception that lifts it. So without an enduring
//! story Bombur stays tapped through its controller's own untap step; with
//! one, it untaps like any other permanent. Both cases are driven through the
//! real turn-structure production path (`GameRunner::advance_to_phase`), not
//! a direct call into the untap-step internals.

use engine::game::scenario::{GameScenario, P1};
use engine::types::phase::Phase;

const BOMBUR: &str = "Storied (If you control three or more artifacts, legendaries, and/or Sagas, you have an enduring story for the rest of the game.)\nBombur doesn't untap during your untap step unless you have an enduring story.";

#[test]
fn bombur_stays_tapped_without_enduring_story() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 controls only Bombur itself: one historic permanent (legendary), short
    // of Storied's three-or-more threshold, so no enduring story is granted.
    let bombur = scenario
        .add_creature_from_oracle(P1, "Bombur, Gentle Dreamer", 5, 3, BOMBUR)
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&bombur).unwrap().tapped = true;

    // Advance past P0's remaining phases and into P1's own untap step (CR 502.3),
    // stopping at the next Upkeep priority window.
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "should now be P1's turn (their untap step has processed)"
    );

    // Reach guard: confirm the premise this test hinges on before trusting the
    // negative assertion below.
    assert!(
        !runner.state().enduring_story.contains(&P1),
        "reach guard: P1 must NOT have an enduring story with only one historic permanent"
    );
    assert!(
        runner.state().objects[&bombur].tapped,
        "without an enduring story, Bombur must stay tapped through its controller's untap step"
    );
}

#[test]
fn bombur_untaps_normally_with_enduring_story() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bombur = scenario
        .add_creature_from_oracle(P1, "Bombur, Gentle Dreamer", 5, 3, BOMBUR)
        .as_legendary()
        .id();
    // Two more legendary permanents so P1 controls three historic permanents
    // (Bombur itself plus these two) alongside the Storied permanent (Bombur),
    // satisfying CR 702.195a's enduring-story grant well before P1's own untap
    // step is reached (state-based actions run on every intervening priority
    // pass while the turn structure advances).
    scenario
        .add_creature(P1, "Legendary Friend One", 1, 1)
        .as_legendary();
    scenario
        .add_creature(P1, "Legendary Friend Two", 1, 1)
        .as_legendary();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&bombur).unwrap().tapped = true;

    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "should now be P1's turn (their untap step has processed)"
    );

    // Reach guard: confirm the premise (enduring story actually granted) before
    // trusting the "untaps normally" assertion — otherwise a broken CantUntap
    // condition and a broken enduring-story grant could both silently pass.
    assert!(
        runner.state().enduring_story.contains(&P1),
        "reach guard: P1 must have gained an enduring story from controlling three historic permanents"
    );
    assert!(
        !runner.state().objects[&bombur].tapped,
        "with an enduring story, Bombur must untap normally during its controller's untap step"
    );
}

/// Runtime regression for PR #8012 maintainer review round 5 (HIGH).
///
/// CR 118.12a: "unless [a player] pays [cost]" is an OPTIONAL cost — the
/// player must actually be offered the choice. The engine offers it exactly
/// once, at attack/block declaration (`WaitingFor::CombatTaxPayment`). CR 502.3
/// untapping is a turn-based action: the untap loop in `game::turns` only skips
/// permanents that have `CantUntap`, it has no payment prompt or continuation,
/// and `game::layers::evaluate_condition` accordingly hard-codes `UnlessPay` to
/// `false`. Attaching such a condition to a `CantUntap` static therefore
/// produces a restriction no player can ever satisfy, while the parser reports
/// the card as fully supported.
///
/// Both halves are asserted through the real production path — the parsed
/// static definitions carried on the live game object, and an actual untap step
/// driven by `GameRunner::advance_to_phase`:
///
/// 1. the condition is the honest `Not(Unrecognized { .. })` deferral shape,
///    NOT a typed `UnlessPay` that would be a false green; and
/// 2. no unpayable lock is silently imposed — the permanent untaps.
///
/// Synthetic Oracle text: this probes the engine's acceptance boundary, not a
/// printed card. No printed card pairs an untap-step restriction with a payment
/// gate today, which is exactly why the false green survived four review
/// rounds. Bombur's own controller-scoped gate above is unaffected and must
/// keep working — that pairing is the point of this file.
const PAYMENT_GATED_UNTAP: &str = "Bombur doesn't untap during your untap step unless you pay {2}.";

#[test]
fn payment_gated_untap_restriction_is_deferred_not_falsely_supported() {
    use engine::types::ability::StaticCondition;
    use engine::types::statics::StaticMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bombur = scenario
        .add_creature_from_oracle(P1, "Bombur, Gentle Dreamer", 5, 3, PAYMENT_GATED_UNTAP)
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&bombur).unwrap().tapped = true;

    // 1. AST honesty, read off the live object's parsed static definitions.
    let cant_untap: Vec<_> = runner.state().objects[&bombur]
        .static_definitions
        .iter_unchecked()
        .filter(|d| matches!(d.mode, StaticMode::CantUntap))
        .collect();
    assert_eq!(
        cant_untap.len(),
        1,
        "expected exactly one CantUntap static, got {cant_untap:#?}"
    );
    match cant_untap[0].condition.as_ref() {
        Some(StaticCondition::Not { condition }) => assert!(
            matches!(**condition, StaticCondition::Unrecognized { .. }),
            "the payment gate must be deferred as Not(Unrecognized), got {condition:?}"
        ),
        other => panic!(
            "a payment-based untap gate has no untap-step continuation and must              be deferred as Not(Unrecognized), not accepted as a typed condition;              got {other:?}"
        ),
    }

    // 2. Runtime: no unpayable lock is silently imposed.
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "should now be P1's turn (their untap step has processed)"
    );
    assert!(
        !runner.state().objects[&bombur].tapped,
        "CR 502.3 + CR 118.12a: the untap step never offers the optional payment,          so the engine must NOT hold the permanent tapped on a condition the          controller has no way to satisfy"
    );
}
