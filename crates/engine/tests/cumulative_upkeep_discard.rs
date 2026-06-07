//! Regression coverage for the **Cumulative upkeep—Discard a card** class
//! (Vexing Sphinx, Phyrexian Soulgorger-adjacent discard variants).
//!
//! ROOT CAUSE under test: `AbilityCost::supports_cumulative_upkeep_payment()`
//! previously hit `_ => false` for `Discard`, so the production synthesizer
//! (`database::synthesis::build`, gated at synthesis.rs:211) emitted **no**
//! cumulative-upkeep trigger for Vexing Sphinx — the permanent sat on the
//! battlefield free, never demanding the discard. The fix makes the Discard
//! runtime payment chain functional (per-counter `scaled_by` expansion +
//! a `remaining` re-prompt loop) and then flips the support gate.
//!
//! These tests drive the REAL pipeline: a Vexing Sphinx built from its
//! authoritative Oracle text through `GameScenario::add_creature_from_oracle`
//! (which runs `synthesize_all`, including `synthesize_cumulative_upkeep`),
//! then advanced through the upkeep step so the synthesized trigger fires,
//! adds an age counter, and surfaces the `UnlessPayment` discard prompt.
//!
//! CR ANCHORS:
//!   * CR 702.24a — "pay [cost] for each age counter on it … If [cost] has
//!     choices … each choice is made separately for each age counter …
//!     Partial payments aren't allowed."
//!   * CR 701.9 / 701.9a — Discard keyword action (hand → graveyard).
//!   * CR 118.12 / 118.12a — unless-payment semantics (decline ≡ unpayable).
//!
//! CARD TEXT (verified from this engine's card-data for Vexing Sphinx):
//!   "Flying\nCumulative upkeep—Discard a card. (At the beginning of your
//!    upkeep, put an age counter on this permanent, then sacrifice it unless
//!    you pay its upkeep cost for each age counter on it.)\nWhen this creature
//!    dies, draw a card for each age counter on it."

use engine::game::scenario::GameScenario;
use engine::types::ability::{AbilityCost, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const VEXING_SPHINX_ORACLE: &str = "Flying\nCumulative upkeep—Discard a card. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)\nWhen this creature dies, draw a card for each age counter on it.";

/// Build a Vexing Sphinx on PlayerId(0)'s battlefield with `preloaded_age`
/// age counters and `hand_card_count` generic discardable cards in hand, then
/// advance to the controller's upkeep and resolve the cumulative-upkeep trigger
/// so the engine is paused at the `UnlessPayment` prompt.
///
/// Returns `(runner, sphinx_id)` paused at `WaitingFor::UnlessPayment`.
fn setup_at_unless_prompt(
    preloaded_age: u32,
    hand_card_count: usize,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    // Start at Untap so a single `auto_advance` (driven by `advance_to_upkeep`)
    // ticks into Upkeep and fires the synthesized cumulative-upkeep trigger
    // onto the stack (CR 503.1a).
    scenario.at_phase(Phase::Untap);

    let sphinx = scenario
        .add_creature_from_oracle(P0, "Vexing Sphinx", 4, 4, VEXING_SPHINX_ORACLE)
        .id();

    if preloaded_age > 0 {
        scenario.with_counter(sphinx, CounterType::Age, preloaded_age);
    }

    let hand_names: Vec<String> = (0..hand_card_count)
        .map(|i| format!("Filler Card {i}"))
        .collect();
    let hand_refs: Vec<&str> = hand_names.iter().map(String::as_str).collect();
    scenario.with_cards_in_hand(P0, &hand_refs);

    let mut runner = scenario.build();
    // `advance_to_upkeep` runs `auto_advance` (Untap → Upkeep), landing the
    // cumulative-upkeep trigger on the stack; `resolve_top` then resolves it,
    // adding the age counter and surfacing the per-counter discard prompt.
    runner.advance_to_upkeep();
    runner.resolve_top();

    (runner, sphinx)
}

/// CR 702.24a + CR 701.9 + CR 118.12a: with one pre-loaded age counter, the
/// first resolved upkeep ticks the counter to TWO, so `expand_per_counter`
/// scales `Discard { Fixed(1) }` to `Discard { Fixed(2) }` and the controller
/// must discard exactly two cards (one per round-trip) to keep the Sphinx.
#[test]
fn vexing_sphinx_cumulative_upkeep_demands_discard_per_age_counter() {
    let (mut runner, sphinx) = setup_at_unless_prompt(1, 3);

    // CR 702.24a: the age counter ticked from 1 (pre-loaded) to 2 before the
    // per-counter unless-cost is computed.
    assert_eq!(
        runner.state().objects[&sphinx]
            .counters
            .get(&CounterType::Age)
            .copied(),
        Some(2),
        "age counter must tick from 1 (pre-loaded) to 2 on this upkeep"
    );

    // CR 702.24a + CR 701.9: the synthesized trigger surfaced an UnlessPayment
    // prompt whose cost is the per-counter-scaled Discard (Fixed(1) × 2 = 2).
    // THIS is the discrimination point: on origin/main the support gate is
    // `false` for Discard, so no trigger synthesizes and `waiting_for` never
    // becomes `UnlessPayment` here.
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "controller is the unless-payer");
            match cost {
                AbilityCost::Discard { count, .. } => {
                    assert_eq!(
                        *count,
                        QuantityExpr::Fixed { value: 2 },
                        "2 age counters × base count 1 = 2 (scaled_by)"
                    );
                }
                other => panic!("expected Discard unless-cost, got {other:?}"),
            }
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    // CR 118.12: pay → engine seeds the `remaining` discard loop and surfaces
    // `WardDiscardChoice { remaining: 2 }`.
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the discard cost must be accepted");

    let (first_choice, second_remaining_cards) = match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice {
            player,
            cards,
            remaining,
            ..
        } => {
            assert_eq!(*player, P0, "controller picks which card to discard");
            assert_eq!(*remaining, 2, "two discards required for two age counters");
            assert!(
                cards.len() >= 2,
                "at least two eligible hand cards to discard"
            );
            (cards[0], cards.clone())
        }
        other => panic!("expected WardDiscardChoice prompt, got {other:?}"),
    };

    // CR 701.9a: discard the first card (hand → graveyard).
    runner
        .act(GameAction::SelectCards {
            cards: vec![first_choice],
        })
        .expect("first discard selection must be accepted");

    // CR 702.24a: one discard remains — the prompt re-derives hand eligibility
    // and the just-discarded card is NO LONGER offered (proves re-derivation
    // excludes the graveyard card rather than filtering by `contains_key`).
    let second_choice = match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice {
            player,
            cards,
            remaining,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*remaining, 1, "one discard remaining after the first");
            assert!(
                !cards.contains(&first_choice),
                "just-discarded card must not be re-offered on the second prompt"
            );
            assert!(
                second_remaining_cards.contains(&cards[0]) || !cards.is_empty(),
                "a distinct eligible card must remain"
            );
            cards[0]
        }
        other => panic!("expected second WardDiscardChoice prompt, got {other:?}"),
    };
    assert_ne!(
        first_choice, second_choice,
        "the two discards must be distinct cards (CR 702.24a per-counter)"
    );

    // CR 701.9a: discard the second card — completing the per-counter payment.
    runner
        .act(GameAction::SelectCards {
            cards: vec![second_choice],
        })
        .expect("second discard selection must be accepted");

    // CR 702.24a: paying the full cost keeps the permanent on the battlefield.
    assert_eq!(
        runner.state().objects[&sphinx].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep discard cost must NOT sacrifice the Sphinx"
    );
    // Hand decreased by exactly two; both discarded cards are in the graveyard.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before - 2,
        "exactly two cards discarded"
    );
    assert_eq!(
        runner.state().objects[&first_choice].zone,
        Zone::Graveyard,
        "first discarded card must be in the graveyard"
    );
    assert_eq!(
        runner.state().objects[&second_choice].zone,
        Zone::Graveyard,
        "second discarded card must be in the graveyard"
    );
}

/// CR 702.24a + CR 118.12a: declining the discard (explicit `pay: false`) or
/// being unable to produce the full per-counter count both make the unless
/// effect happen — the Sphinx is sacrificed. Partial payments aren't allowed.
#[test]
fn vexing_sphinx_declining_discard_sacrifices() {
    // Sub-case (a): explicit decline. Two age counters demanded; hand has
    // enough cards to pay, but the controller chooses not to.
    {
        let (mut runner, sphinx) = setup_at_unless_prompt(1, 3);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
            "must be paused at the discard unless-prompt"
        );

        runner
            .act(GameAction::PayUnlessCost { pay: false })
            .expect("declining the unless cost must be accepted");

        // CR 118.12a: declining ≡ the effect (sacrifice) happens.
        assert_eq!(
            runner.state().objects[&sphinx].zone,
            Zone::Graveyard,
            "declining the cumulative-upkeep discard must sacrifice the Sphinx"
        );
    }

    // Sub-case (b): under-payable hand. Two age counters demanded (counter
    // ticks 1 → 2) but only ONE eligible card in hand — fewer than the
    // required two, so the cost is unpayable even on `pay: true`, and the
    // effect happens (CR 702.24a: partial payments aren't allowed).
    {
        let (mut runner, sphinx) = setup_at_unless_prompt(1, 1);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
            "must be paused at the discard unless-prompt"
        );

        runner
            .act(GameAction::PayUnlessCost { pay: true })
            .expect("attempting to pay an unpayable cost must be accepted");

        // CR 702.24a: the controller can't produce two discards, so the cost
        // is unpayable and the Sphinx is sacrificed — it must NOT consume the
        // single card as a partial payment.
        assert_eq!(
            runner.state().objects[&sphinx].zone,
            Zone::Graveyard,
            "an under-payable discard cost must sacrifice the Sphinx (no partial payment)"
        );
        assert_eq!(
            runner.state().players[P0.0 as usize].hand.len(),
            1,
            "the lone hand card must NOT be discarded as a partial payment"
        );
    }
}
