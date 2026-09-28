//! Issue #7510 — declining an optional discard replacement during a
//! forced-count multi-card discard.
//!
//! Reported: Balance instructs P0 to discard 2 cards while P0 controls Library
//! of Leng; only ONE replacement prompt fires where two are owed, and only one
//! card leaves the hand. P0 finishes at 2 instead of 1.
//!
//! Library of Leng's replacement is DECLINED here, so CR 614.6 ("if an event is
//! replaced, it never happens") is not engaged at all — the discard must proceed
//! unmodified. The reported failure loses the card from the batch rather than
//! replacing it.
//!
//! `chain_of_smog_copy.rs` already covers this mechanism for a FIXED count
//! ("Target player discards two cards"). Balance is the COMPUTED-count arm: its
//! discard count is a `QuantityExpr::Difference` against a frozen cross-player
//! hand minimum, so it reaches the discard batch through
//! `clause_minimum_snapshot` rather than a literal. This file pins that arm.
//!
//! Discriminators (what each assertion catches):
//!   - `declines == 2` — the reported "only one prompt fires" symptom;
//!   - `hand_len(P0) == 1` — the reported "one discard lost" symptom;
//!   - the accept-polarity test — that the prompts are real and Leng's own
//!     effect works, so the decline test cannot pass by the replacement never
//!     being offered at all.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Scryfall Oracle text (verified 2026-09-09).
const BALANCE_ORACLE: &str = "Each player chooses a number of lands they \
control equal to the number of lands controlled by the player who controls \
the fewest, then sacrifices the rest. Players discard cards and sacrifice \
creatures the same way.";
const LIBRARY_SENTINEL: &str = "Library sentinel";
const LIBRARY_OF_LENG: &str = "You have no maximum hand size.\nIf an effect \
causes you to discard a card, discard it, but you may put it on top of your \
library instead of into your graveyard.";

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map_or(0, |p| p.hand.len())
}

fn card_names(
    state: &GameState,
    ids: impl Iterator<Item = engine::types::identifiers::ObjectId>,
) -> Vec<String> {
    ids.filter_map(|id| state.objects.get(&id).map(|o| o.name.clone()))
        .collect()
}

/// P0's graveyard minus the resolving Balance card. CR 608.2n puts a sorcery
/// into its owner's graveyard as the final part of its own resolution, so a
/// bare graveyard count would report 3 for 2 discards.
fn discards_in_graveyard(
    state: &GameState,
    balance: engine::types::identifiers::ObjectId,
) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Graveyard && o.owner == P0 && o.id != balance)
        .count()
}

fn zone_len(state: &GameState, player: PlayerId, zone: Zone) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == zone && o.owner == player)
        .count()
}

/// The reporter's fixture: P0 hand 3, P1 hand 1, P0 controls Library of Leng,
/// Balance cast for free. P0 owes 2 discards and must finish at 1.
fn balance_with_leng() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..3 {
        scenario.add_card_to_hand(P0, &format!("P0 hand card {i}"));
    }
    scenario.add_card_to_hand(P1, "P1 hand card");
    // Sentinel on top of P0's library. Leng puts a discarded card ON TOP, so
    // after an applied replacement the sentinel must be BURIED beneath both
    // discards — a library-size assertion alone would pass if the engine put
    // them on the bottom instead.
    scenario.with_library_top(P0, &[LIBRARY_SENTINEL]);
    scenario.add_artifact_from_oracle(P0, "Library of Leng", LIBRARY_OF_LENG);
    let balance = scenario
        .add_spell_to_hand_from_oracle(P0, "Balance", false, BALANCE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let runner = scenario.build();
    (runner, balance)
}

/// Drive Balance to its discard choice and submit both required discards.
fn cast_and_select_both_discards(
    runner: &mut engine::game::scenario::GameRunner,
    balance: engine::types::identifiers::ObjectId,
) {
    runner.cast(balance).resolve();

    let WaitingFor::DiscardChoice { cards, count, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach guard: Balance must park a discard choice, got {:?}",
            runner.state().waiting_for
        );
    };
    // Reach guard: if the computed count were not 2, the fixture would not be
    // exercising the multi-card path this issue is about.
    assert_eq!(
        count, 2,
        "P0 holds 3 and P1 holds 1, so Balance must ask for exactly 2 discards"
    );
    runner
        .act(GameAction::SelectCards {
            cards: cards.into_iter().take(count).collect(),
        })
        .expect("submitting Balance's two required discards must resume the cast");
}

/// Which polarity to answer each Library of Leng prompt with.
///
/// The applied candidate is identified as "the one that is not Decline" rather
/// than by label: the engine describes a replacement candidate with the
/// REPLACEMENT'S ORACLE TEXT, not the card name, so matching a literal would
/// silently rot if that description were ever reworded.
#[derive(Clone, Copy)]
enum LengAnswer {
    Decline,
    Apply,
}

/// Answer every Library of Leng prompt with `answer`, returning the count.
fn answer_leng_prompts(
    runner: &mut engine::game::scenario::GameRunner,
    answer: LengAnswer,
) -> usize {
    let mut answered = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    {
        let index = candidates
            .iter()
            .position(|candidate| match answer {
                LengAnswer::Decline => candidate.description == "Decline",
                LengAnswer::Apply => candidate.description != "Decline",
            })
            .unwrap_or_else(|| {
                panic!(
                    "Library of Leng must offer both polarities; got {:?}",
                    candidates
                        .iter()
                        .map(|c| c.description.clone())
                        .collect::<Vec<_>>()
                )
            });
        runner
            .act(GameAction::ChooseReplacement { index })
            .unwrap_or_else(|e| panic!("answering Leng prompt {answered} must succeed: {e:?}"));
        answered += 1;
        assert!(
            answered <= 4,
            "replacement prompts must terminate; a loop here means the batch never drains"
        );
    }
    answered
}

/// Issue #7510: declining Leng must leave the discard unmodified — BOTH cards
/// reach the graveyard and P0 finishes at the frozen minimum of 1.
#[test]
fn balance_multi_discard_completes_when_library_of_leng_is_declined() {
    let (mut runner, balance) = balance_with_leng();
    cast_and_select_both_discards(&mut runner, balance);

    let declines = answer_leng_prompts(&mut runner, LengAnswer::Decline);
    runner.advance_until_stack_empty();

    assert_eq!(
        declines, 2,
        "one Leng prompt is owed per discarded card; the report observed only one"
    );
    assert_eq!(
        hand_len(runner.state(), P0),
        1,
        "P0 owes 2 discards and must finish at the frozen minimum of 1; the \
         report observed 2 (one discard silently dropped)"
    );
    assert_eq!(
        hand_len(runner.state(), P1),
        1,
        "P1 was already at the minimum and discards nothing"
    );
    // CR 701.9a: a DECLINED replacement means the cards take the ordinary
    // hand -> graveyard route, not Leng's library-top route. Balance itself is
    // also in P0's graveyard by now (CR 608.2n puts a resolving sorcery there),
    // so the spell is excluded rather than counted as a discard.
    assert_eq!(
        discards_in_graveyard(runner.state(), balance),
        2,
        "both declined discards must reach the graveyard"
    );
}

/// The paired polarity. Without this, the decline test above could pass on a
/// build where Leng never offers a choice at all — the prompts would be zero,
/// the discards unreplaced, and the hand still 1.
#[test]
fn balance_multi_discard_routes_both_cards_to_library_top_when_leng_is_accepted() {
    let (mut runner, balance) = balance_with_leng();
    let library_before = zone_len(runner.state(), P0, Zone::Library);
    cast_and_select_both_discards(&mut runner, balance);

    let accepts = answer_leng_prompts(&mut runner, LengAnswer::Apply);
    runner.advance_until_stack_empty();

    assert_eq!(accepts, 2, "one Leng prompt is owed per discarded card");
    assert_eq!(
        hand_len(runner.state(), P0),
        1,
        "accepting Leng still completes both discards — it only changes destination"
    );
    assert_eq!(
        discards_in_graveyard(runner.state(), balance),
        0,
        "an accepted Leng replacement routes to the library top, not the graveyard"
    );
    assert_eq!(
        zone_len(runner.state(), P0, Zone::Library),
        library_before + 2,
        "both discarded cards must land in P0's library"
    );

    // Size alone would pass if the engine appended to the BOTTOM. Leng says
    // "on top", so prove position through the production draw flow: the next
    // two cards drawn must be the discards, and the pre-seeded sentinel must
    // still be buried underneath both of them.
    let discarded: Vec<String> = card_names(
        runner.state(),
        runner.state().players[0].library.iter().copied().take(2),
    );
    assert!(
        !discarded.contains(&LIBRARY_SENTINEL.to_string()),
        "the sentinel must be buried beneath both discards, got library top {discarded:?}"
    );
    // Pin the exact depth, not just "not in the top two": the sentinel started
    // at index 0, so it must now sit directly beneath BOTH discards. This is
    // what distinguishes "on top" (CR: Leng's own wording) from "somewhere
    // above the sentinel".
    let sentinel_depth = runner.state().players[0]
        .library
        .iter()
        .position(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.name == LIBRARY_SENTINEL)
        })
        .expect("the sentinel must still be somewhere in P0's library");
    assert_eq!(
        sentinel_depth, 2,
        "the sentinel began on top; both Leng placements must now sit above it"
    );

    runner.state_mut().debug_mode = true;
    let hand_before = hand_len(runner.state(), P0);
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 2,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(runner.state(), P0),
        hand_before + 2,
        "reach guard: both draws must actually resolve"
    );
    let drawn = card_names(
        runner.state(),
        runner.state().players[0]
            .hand
            .iter()
            .copied()
            .skip(hand_before),
    );
    assert_eq!(
        drawn, discarded,
        "the two cards drawn back must be exactly the two Leng put on top"
    );
    assert!(
        !drawn.contains(&LIBRARY_SENTINEL.to_string()),
        "drawing the sentinel means the discards went somewhere other than the top"
    );
}
