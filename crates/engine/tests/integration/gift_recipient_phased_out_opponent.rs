//! R4g — CR 702.174a: Gift is *"you may **choose** an opponent"*, a CHOICE and not a
//! target (CR 115.10a), so the recipient list is the seats that still exist to be chosen.
//!
//! Two arms, and the second one is the reason this file exists at all. Narrowing the
//! recipient list moves boards across the gift seam's low-cardinality branches, and one of
//! those branches is the only ERRORING branch in the whole choice-enumeration class: with
//! no choosable opponent, an accepted gift promise becomes an engine error. That branch was
//! previously reachable only by eliminating every opponent — which ends the game — and is
//! newly reachable at a LIVE table once phasing narrows the list. An erroring branch nobody
//! asserts is a branch that changes silently.
//!
//! The production entry chain is the `DecideOptionalCost` beat, NOT `handle_cast_spell`:
//! `GameAction::DecideOptionalCost { pay: true }` → `engine_casting::handle_optional_cost_
//! choice` → `casting_costs::handle_decide_additional_cost` → `continue_after_gift_promised`
//! (which is private and is never called directly here). `handle_cast_spell` opens the cast
//! that publishes the `OptionalCostChoice`, one action earlier.

use engine::game::engine::EngineError;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AdditionalCostOrigin;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const PEERLESS_RECYCLING: &str =
    "Gift a card (You may promise an opponent a gift as you cast this spell. \
If you do, they draw a card before its other effects.)\n\
Return target permanent card from your graveyard to your hand. If the gift was promised, \
instead return two target permanent cards from your graveyard to your hand.";

/// A 5-seat board with a castable Gift spell, `phase_out` seats transitioned through the
/// PRODUCTION phasing API, and P2 eliminated unless the caller asks otherwise.
///
/// Both arms differ ONLY in which seats are phased out, so the pair is one narrowing
/// series rather than two unrelated boards.
fn gift_board(
    phase_out: &[PlayerId],
    eliminate: Option<PlayerId>,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(5, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.add_creature_to_graveyard(P0, "Bear Cub", 2, 2);
    scenario.add_creature_to_graveyard(P0, "Cougar Cub", 2, 2);
    // MTGJSON supplies the bare "Gift" keyword hint; scenario inference cannot FromStr a
    // "Gift a card (reminder…)" line, so pass the production hint.
    let spell = scenario
        .add_spell_to_hand(P0, "Peerless Recycling", true)
        .from_oracle_text_with_keywords(&["Gift"], PEERLESS_RECYCLING)
        .id();

    let mut runner = scenario.build();
    let mut events = Vec::new();
    for seat in phase_out {
        // Setup anti-vacuity: `phase_out_player` reports the seats it transitioned, so a
        // silent no-op fails loudly here instead of quietly weakening the arm.
        let transitioned =
            engine::game::phasing::phase_out_player(runner.state_mut(), *seat, &mut events);
        assert_eq!(
            transitioned,
            vec![*seat],
            "phase_out_player must actually transition {seat:?}"
        );
    }
    if let Some(seat) = eliminate {
        engine::game::elimination::eliminate_player(runner.state_mut(), seat, &mut events);
        assert!(
            runner.state().players[seat.0 as usize].is_eliminated,
            "{seat:?} must read as eliminated"
        );
    }
    (runner, spell)
}

/// Drive the cast up to and including the gift promise, asserting the two SHIPPED guards
/// on the way. Returns whatever the promise beat returned, so the caller can assert on
/// either the published prompt or the `Err`.
///
/// GUARD 1 — the `OptionalCostChoice` carrying the Gift origin is published. That is the
/// in-test proof the promise path was entered at all, taken BEFORE the promise is
/// submitted, so a later `Err` cannot be an earlier rejection wearing a gift's name.
/// GUARD 2 — `players::opponents` (the UN-routed sibling the fix does not touch) is still
/// non-empty on this same board, so any emptiness the routed seam reports is attributable
/// to the routing rather than to a board with no opponents.
///
/// Both guards are runnable in the SHIPPED tree by construction: the gift promise is
/// queued with no opponents gate, and `players::opponents` is not narrowed by this phase.
fn promise_the_gift(
    runner: &mut engine::game::scenario::GameRunner,
    spell: ObjectId,
) -> Result<engine::types::game_state::ActionResult, EngineError> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Peerless Recycling");

    for _ in 0..50 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("complete mana payment");
            }
            WaitingFor::OptionalCostChoice { origin, .. } => {
                // GUARD 1.
                assert_eq!(
                    *origin,
                    AdditionalCostOrigin::Gift,
                    "the promise path must be entered: the published optional cost is Gift"
                );
                // GUARD 2.
                assert!(
                    !engine::game::players::opponents(runner.state(), P0).is_empty(),
                    "the UN-routed opponent relation is still non-empty on this board, so \
                     an empty CHOOSABLE list is the routing's doing and not the board's"
                );
                return runner.act(GameAction::DecideOptionalCost { pay: true });
            }
            other => panic!("unexpected beat before the gift promise: {other:?}"),
        }
    }
    panic!("the cast never reached the Gift optional-cost choice");
}

/// R4g arm 1 — the offer arm. A phased-out seat (the CR 702.26b MIRROR) and a departed one
/// (CR 800.4 + CR 102.1) are both absent from the published recipient list, and the two
/// surviving opponents are both present.
///
/// THE OFFER MUST STILL FIRE, which is both the reach-guard and the anti-vacuity control:
/// with a single choosable opponent the seam takes the sole-opponent auto-latch branch and
/// publishes nothing at all, so a `!contains` assertion would pass on a board where no
/// choice was ever offered. Hence total equality on the published variant.
///
/// REVERT-PROBE: restore `players::opponents` at the gift candidate derivation ⇒ P1
/// reappears ⇒ the equality FAILS.
#[test]
fn gift_recipient_offer_excludes_a_phased_out_opponent_and_still_offers_the_rest() {
    let (mut runner, spell) = gift_board(&[P1], Some(PlayerId(2)));
    promise_the_gift(&mut runner, spell).expect("the promise is accepted with two recipients");

    match &runner.state().waiting_for {
        WaitingFor::ChooseGiftRecipient { candidates, .. } => {
            assert_eq!(
                *candidates,
                vec![PlayerId(3), PlayerId(4)],
                "phased-out P1 and eliminated P2 are out; both valid opponents are in"
            );
        }
        other => panic!("expected ChooseGiftRecipient, got {other:?}"),
    }
}

/// R4g arm 2 — the `0`-crossing, this class's only ERROR branch.
///
/// Every opponent is phased out and NONE is eliminated, so the table is still live: a board
/// that reaches zero choosable opponents by elimination would also end the game, which is a
/// different thing to assert. An accepted gift promise with no choosable recipient is an
/// invalid action, and 5c PINS that pre-existing `Err` rather than changing it — what 5c
/// changes is that the branch is newly reachable at a live table.
///
/// The two shipped guards inside `promise_the_gift` are what make this arm non-vacuous: a
/// post-fix drive CANNOT reach `ChooseGiftRecipient` on this board, so a "the prompt is not
/// published" assertion would need the fix reverted to mean anything, and a revert-probe is
/// an executor action rather than a shipped guard.
///
/// REVERT-PROBE: restore `players::opponents` at the gift candidate derivation ⇒ the
/// candidate list is non-empty again ⇒ the prompt publishes instead of erroring ⇒ FAILS.
#[test]
fn gift_promise_with_every_opponent_phased_out_is_an_invalid_action() {
    let (mut runner, spell) = gift_board(&[P1, PlayerId(2), PlayerId(3), PlayerId(4)], None);
    let outcome = promise_the_gift(&mut runner, spell);

    match outcome {
        Err(EngineError::InvalidAction(message)) => {
            assert!(
                message.to_lowercase().contains("gift"),
                "the rejection must name the gift it refused, got: {message}"
            );
        }
        other => panic!("expected InvalidAction naming the gift, got {other:?}"),
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseGiftRecipient { .. }
        ),
        "no recipient prompt may be published when no opponent is choosable"
    );
}
