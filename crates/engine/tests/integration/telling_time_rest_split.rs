//! Telling Time-class remainder split: "look at the top N, put some into your
//! hand, and split what's left between the TOP and the BOTTOM of your library."
//!
//! CARD TEXT (Telling Time, {1}{U} instant, Scryfall-verified):
//! "Look at the top three cards of your library. Put one of those cards into
//! your hand, one on top of your library, and one on the bottom of your
//! library."
//!
//! Before this change the trailing "one on top ... and one on the bottom"
//! clause was SILENTLY SWALLOWED — the card parsed to
//! `Dig { destination: Hand, keep_count: 1, rest_destination: None }`, and a
//! `rest_destination` of `None` defaults to the GRAVEYARD. Telling Time milled
//! two cards instead of returning them to the library, with no
//! `Effect::Unimplemented` and no red coverage to show for it.
//!
//! The capability is composed of shipped building blocks:
//!   * `Effect::Dig.rest_split_top_count` (CR 401.2 — a library is one
//!     face-down pile, so top and bottom are the only two positions an
//!     instruction can name) carries how many of the remainder go on top.
//!   * `WaitingFor::DigRestSplitChoice` (CR 701.20e — the remainder was shown
//!     only to the looking player) is the follow-up prompt.
//!   * `route_rest_split_then` reuses the same per-card
//!     `ZoneMoveRequest::effect(..).at_library_position(..)` primitive as the
//!     uniform `route_rest_partition_then`; only the position varies.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    DigRestOrder, DigRestSplitScope, DigSource, Effect, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text — a paraphrase can take a different parser branch and
/// go green while the real card stays broken.
const TELLING_TIME_ORACLE: &str = "Look at the top three cards of your library. \
Put one of those cards into your hand, one on top of your library, and one on \
the bottom of your library.";

fn telling_time_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 1,
    }
}

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Put a plain non-land card into P0's library (pushed on top of the existing
/// library contents, so the last one added is deepest-added-last).
fn add_library_card(runner: &mut GameRunner, name: &str) -> ObjectId {
    add_library_card_for(runner, P0, name)
}

/// Same, for an arbitrary library owner — the cross-player fixtures need P1 to
/// own the cards so `library_owner` and the dig's chooser genuinely differ.
fn add_library_card_for(
    runner: &mut GameRunner,
    owner: engine::types::player::PlayerId,
    name: &str,
) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        owner,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    id
}

// ---------------------------------------------------------------------------
// Test 1 (PARSER): the split clause reaches the AST instead of being swallowed.
// ---------------------------------------------------------------------------

#[test]
fn telling_time_parses_a_library_top_bottom_rest_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        TELLING_TIME_ORACLE,
        "Telling Time",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let effect = parsed
        .abilities
        .first()
        .map(|a| a.effect.as_ref())
        .expect("Telling Time must parse to one spell ability");
    match effect {
        Effect::Dig {
            count,
            destination,
            keep_count,
            rest_destination,
            rest_split_top_count,
            ..
        } => {
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 3 },
                "look at top three"
            );
            assert_eq!(*destination, Some(Zone::Hand), "one goes into your hand");
            assert_eq!(*keep_count, Some(1), "exactly one is kept");
            // CR 401.2: the remainder goes back into the LIBRARY, not the
            // graveyard that `None` would have defaulted to.
            assert_eq!(
                *rest_destination,
                Some(Zone::Library),
                "the split remainder is library-bound"
            );
            // THE REGRESSION ASSERTION: this is `None` if the "one on top ...
            // and one on the bottom" clause is swallowed again.
            assert_eq!(
                *rest_split_top_count,
                Some(QuantityExpr::Fixed { value: 1 }),
                "exactly one of the remainder goes on TOP"
            );
        }
        other => panic!("expected Effect::Dig, got {other:?}"),
    }
}

/// PAIRED NEGATIVE: the sibling uniform-remainder grammar is untouched. A dig
/// that says "and the rest on the bottom of your library" names ONE position
/// for the whole remainder, so it must keep `rest_split_top_count: None` and
/// its existing uniform routing.
#[test]
fn plain_rest_on_bottom_dig_does_not_parse_as_a_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Look at the top three cards of your library. Put one of them into your \
         hand and the rest on the bottom of your library in any order.",
        "Uniform Remainder Dig",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let effect = parsed
        .abilities
        .first()
        .map(|a| a.effect.as_ref())
        .expect("the sibling dig must parse to one spell ability");
    match effect {
        Effect::Dig {
            rest_destination,
            rest_split_top_count,
            ..
        } => {
            assert_eq!(
                *rest_destination,
                Some(Zone::Library),
                "the uniform remainder still goes to the library"
            );
            assert_eq!(
                *rest_split_top_count, None,
                "a single named position is not a split"
            );
        }
        other => panic!("expected Effect::Dig, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 2 (PRODUCTION PATH): cast the real card, submit both real GameActions.
// ---------------------------------------------------------------------------

/// Cast Telling Time for real and drive BOTH prompts through `apply()`.
///
/// This is the test that proves the whole stack: parser → `Effect::Dig` →
/// `DigChoice` → kept delivery → `DigRestSplitChoice` → `route_rest_split_then`
/// → library top/bottom. Reverting any single layer fails it.
#[test]
fn telling_time_splits_its_remainder_between_library_top_and_bottom() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Telling Time", false, TELLING_TIME_ORACLE);
    builder.with_mana_cost(telling_time_cost());
    let spell_id = builder.id();

    let mut runner = scenario.build();

    // A deep-library sentinel proves a bottomed card really reached the
    // BOTTOM rather than merely "not the top".
    // `create_object` appends to the library's back, so the three looked-at
    // cards go in first (top) and the sentinel last (bottom).
    add_library_card(&mut runner, "Look0");
    add_library_card(&mut runner, "Look1");
    add_library_card(&mut runner, "Look2");
    let sentinel = add_library_card(&mut runner, "Deep Sentinel");

    add_mana(&mut runner, ManaType::Blue, 2);

    let lib_before = runner.state().players[0].library.len();
    let outcome = runner.cast(spell_id).resolve();

    // Stage 1: the ordinary keep prompt — look at 3, keep exactly 1. Read the
    // looked-at window from the engine rather than assuming a library order.
    let looked_at = match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            cards, keep_count, ..
        } => {
            assert_eq!(cards.len(), 3, "look at the top three cards");
            assert_eq!(*keep_count, 1, "exactly one card is kept");
            cards.clone()
        }
        other => panic!("expected DigChoice, got {other:?}"),
    };
    assert!(
        !looked_at.contains(&sentinel),
        "the deep sentinel must sit below the looked-at window; \
         the test needs an untouched card deeper than the three"
    );
    let kept = looked_at[0];
    let to_top = looked_at[1];
    let to_bottom = looked_at[2];

    runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("keeping one of three must be accepted");

    // THE REGRESSION ASSERTION. Before the fix the engine auto-routed the two
    // unkept cards to the GRAVEYARD here and never raised a second prompt.
    let (split_pile, split_top_count) = match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            cards, top_count, ..
        } => (cards.clone(), *top_count),
        other => panic!("expected DigRestSplitChoice after the keep step, got {other:?}"),
    };
    assert_eq!(
        split_pile.len(),
        2,
        "the two unkept cards form the remainder"
    );
    assert_eq!(split_top_count, 1, "one of them goes on top");
    assert!(
        split_pile.contains(&to_top) && split_pile.contains(&to_bottom),
        "the remainder is exactly the two unkept cards"
    );
    assert_eq!(
        runner.state().objects[&to_top].zone,
        Zone::Library,
        "an unkept card must NOT have been milled to the graveyard"
    );
    assert_eq!(runner.state().objects[&to_bottom].zone, Zone::Library);

    // Stage 2: submit the split as a full ARRANGEMENT of the pile (CR 401.4) —
    // the leading `top_count` entries take the top, the rest take the bottom.
    runner
        .act(GameAction::SelectCards {
            cards: vec![to_top, to_bottom],
        })
        .expect("a full two-card arrangement must be accepted");
    runner.advance_until_stack_empty();

    let st = runner.state();
    assert_eq!(
        st.objects[&kept].zone,
        Zone::Hand,
        "the kept card is in hand"
    );
    let library: Vec<ObjectId> = st.players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&to_top),
        "the chosen card is on TOP of the library"
    );
    assert_eq!(
        library.last(),
        Some(&to_bottom),
        "the unchosen card is on the BOTTOM of the library, below the sentinel"
    );
    assert!(
        library.contains(&sentinel),
        "the pre-existing library card is untouched"
    );
    assert_eq!(
        st.objects[&to_top].zone,
        Zone::Library,
        "neither remainder card reached the graveyard"
    );
    assert_eq!(st.objects[&to_bottom].zone, Zone::Library);
    // Exactly one card (the kept one) left the library.
    assert_eq!(
        st.players[0].library.len(),
        lib_before - 1,
        "only the kept card leaves the library"
    );
}

// ---------------------------------------------------------------------------
// Shared production-path harness.
// ---------------------------------------------------------------------------

/// Cast a synthetic Telling Time-class card for real and drive it up to — and
/// no further than — the `DigRestSplitChoice` pause, returning the live runner,
/// the remainder pile in prompt order, and the prompt's `top_count`.
///
/// Every hostile / cleanup test below builds its fixture through THIS, so the
/// state under test is one production actually parks. A hand-assembled
/// `WaitingFor` can encode a prompt shape production never reaches (and can
/// carry `completion: None`, which production never parks), which makes any
/// assertion about it a claim about a state that cannot occur.
fn production_split_pause(
    card_name: &str,
    oracle: &str,
    library_cards: usize,
) -> (GameRunner, Vec<ObjectId>, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, card_name, false, oracle);
    builder.with_mana_cost(telling_time_cost());
    let spell_id = builder.id();
    let mut runner = scenario.build();

    for index in 0..library_cards {
        add_library_card(&mut runner, &format!("Lib{index}"));
    }
    add_mana(&mut runner, ManaType::Blue, 2);

    let outcome = runner.cast(spell_id).resolve();
    let looked_at = match outcome.final_waiting_for() {
        WaitingFor::DigChoice { cards, .. } => cards.clone(),
        other => panic!("expected the keep prompt first, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0]],
        })
        .expect("keeping one looked-at card must be accepted");

    match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            cards, top_count, ..
        } => {
            let pile = cards.clone();
            let top_count = *top_count;
            (runner, pile, top_count)
        }
        other => panic!("expected DigRestSplitChoice after the keep step, got {other:?}"),
    }
}

/// Telling Time's own shape: look at 3, keep 1, remainder of 2 with one on top.
fn telling_time_split_pause() -> (GameRunner, Vec<ObjectId>, usize) {
    production_split_pause("Telling Time", TELLING_TIME_ORACLE, 4)
}

// ---------------------------------------------------------------------------
// Test 3 (BLOCKER 1a): a multi-card BOTTOM pile gets a CR 401.4 order choice.
// ---------------------------------------------------------------------------

/// The numeric split grammar admits partitions wider than Telling Time's
/// 1-to-top / 1-to-bottom. This is the class member with a 3-card bottom pile.
const WIDE_SPLIT_ORACLE: &str = "Look at the top five cards of your library. \
Put one of those cards into your hand, one on top of your library, and three on \
the bottom of your library.";

/// CR 401.4: "If an effect puts two or more cards in a specific position in a
/// library at the same time, the owner of those cards may arrange them in any
/// order."
///
/// The bottom pile here holds THREE cards, so its internal order is the
/// owner's to choose — it is not whatever order the cards happened to be
/// encountered in. The regression assertion submits the bottom segment in
/// REVERSE of the pile's prompt order and requires the library to reflect that
/// submitted order.
///
/// Before this fix the resolver derived the bottom pile by filtering the pile
/// in ITS OWN order (`cards.iter().filter(|id| !top.contains(id))`), so the
/// submitted bottom order was discarded and this test's final ordering
/// assertion fails on revert.
#[test]
fn a_multi_card_bottom_pile_is_arranged_by_the_owner() {
    let (mut runner, pile, top_count) = production_split_pause("Wide Split", WIDE_SPLIT_ORACLE, 6);
    assert_eq!(pile.len(), 4, "look at five, keep one, four remain");
    assert_eq!(top_count, 1, "one of the remainder goes on top");

    // Deliberately reverse the three bottom-bound cards relative to prompt order.
    let chosen_top = pile[0];
    let bottom_in_submitted_order = vec![pile[3], pile[1], pile[2]];
    let mut arrangement = vec![chosen_top];
    arrangement.extend(bottom_in_submitted_order.iter().copied());

    runner
        .act(GameAction::SelectCards { cards: arrangement })
        .expect("a full four-card arrangement must be accepted");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&chosen_top),
        "the leading arrangement entry is on TOP"
    );

    // THE REGRESSION ASSERTION: the last three library slots must read back in
    // the SUBMITTED bottom order, not in the pile's encounter order.
    let tail: Vec<ObjectId> = library[library.len() - 3..].to_vec();
    assert_eq!(
        tail, bottom_in_submitted_order,
        "CR 401.4: the owner's submitted bottom order must be honored; \
         got {tail:?}, wanted {bottom_in_submitted_order:?}"
    );
    // PAIRED NEGATIVE: the encounter order really is different, so the
    // assertion above cannot pass by coincidence.
    assert_ne!(
        bottom_in_submitted_order,
        vec![pile[1], pile[2], pile[3]],
        "the fixture must submit a bottom order that differs from pile order"
    );
    for id in &pile {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Library,
            "no remainder card may leave the library"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 4 (BLOCKER 1b): a DEGENERATE partition still needs an ORDER choice.
// ---------------------------------------------------------------------------

/// Same class, but the printed counts ask for two on top and one on the
/// bottom. Run against a three-card library the dig exhausts, the remainder is
/// two cards and `top_count` clamps to 2 — a degenerate, unique PARTITION.
const TOP_HEAVY_SPLIT_ORACLE: &str = "Look at the top three cards of your library. \
Put one of those cards into your hand, two on top of your library, and one on \
the bottom of your library.";

/// CR 401.4 again, and the correction this blocker is about: **a unique
/// partition is not a unique order.**
///
/// With a two-card remainder and `top_count == 2` there is exactly one way to
/// SPLIT the pile — everything goes on top. There are still two ways to
/// ARRANGE it, and CR 401.4 gives that choice to the owner. The previous code
/// fast-pathed every degenerate partition straight into the move with no
/// prompt at all, so the owner silently lost the ordering decision.
#[test]
fn a_degenerate_all_top_partition_still_prompts_for_order() {
    // REGRESSION ASSERTION #1: the prompt exists at all. The old code
    // short-circuited `top_count == pile.len()` into an immediate route, and
    // `production_split_pause` panics if no split prompt is parked.
    let (mut runner, pile, top_count) =
        production_split_pause("Top Heavy Split", TOP_HEAVY_SPLIT_ORACLE, 3);
    assert_eq!(pile.len(), 2, "look at three, keep one, two remain");
    assert_eq!(
        top_count, 2,
        "the whole remainder goes on top — the partition is forced"
    );

    // REGRESSION ASSERTION #2: the submitted order is what lands. Submit the
    // pile reversed, so encounter order and chosen order disagree.
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[1], pile[0]],
        })
        .expect("a full two-card arrangement must be accepted");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&pile[1]),
        "the owner put the SECOND pile card topmost (CR 401.4)"
    );
    assert_eq!(
        library.get(1),
        Some(&pile[0]),
        "and the first pile card directly beneath it"
    );
}

/// PAIRED NEGATIVE for the prompt gate: a pile of ONE card has neither a
/// partition nor an order to decide, so it must still route with no prompt.
/// This is what keeps `a_degenerate_all_top_partition_still_prompts_for_order`
/// from being satisfied by a blanket "always prompt".
#[test]
fn a_single_card_remainder_routes_without_any_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Telling Time", false, TELLING_TIME_ORACLE);
    builder.with_mana_cost(telling_time_cost());
    let spell_id = builder.id();
    let mut runner = scenario.build();
    // Only two cards: the dig looks at both, keeps one, and exactly one card
    // remains — a single-card remainder.
    add_library_card(&mut runner, "Only A");
    add_library_card(&mut runner, "Only B");
    add_mana(&mut runner, ManaType::Blue, 2);

    let outcome = runner.cast(spell_id).resolve();
    let looked_at = match outcome.final_waiting_for() {
        WaitingFor::DigChoice { cards, .. } => cards.clone(),
        other => panic!("expected the keep prompt, got {other:?}"),
    };
    assert_eq!(
        looked_at.len(),
        2,
        "a two-card library yields a two-card look"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0]],
        })
        .expect("keeping one must be accepted");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "a one-card remainder has exactly one arrangement and must not prompt"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&looked_at[0]].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&looked_at[1]].zone,
        Zone::Library,
        "the lone remainder card still went back to the library"
    );
}

// ---------------------------------------------------------------------------
// Test 5 (BLOCKER 2): the completion contract is validated BEFORE any mutation.
// ---------------------------------------------------------------------------

/// `game/visibility.rs` strips `completion` to `None` in every per-player
/// client projection, so a `None` completion on an inbound submission means a
/// redacted client view was echoed back at the engine (or the state is
/// corrupt). Either way the dig's deferred tail — `RevealRestPile`'s
/// reveal-marker cleanup, tracked-set publication and continuation wiring — is
/// not there to run.
///
/// The resolver must reject that BEFORE it moves a card. Moving first and
/// discovering the missing tail afterwards leaves the library rearranged, the
/// reveal markers stale and the tracked set unpublished, with no way back.
#[test]
fn a_missing_completion_is_rejected_before_any_card_moves() {
    let (mut runner, pile, _top_count) = telling_time_split_pause();

    // Take the REAL parked prompt and strip only its completion — exactly the
    // shape `visibility.rs` hands to a client. Everything else (pile, counts,
    // source, and the pending dig bookkeeping in `state`) stays genuine.
    let WaitingFor::DigRestSplitChoice {
        player,
        library_owner,
        cards,
        top_count,
        scope,
        source_id,
        completion,
        ..
    } = runner.state().waiting_for.clone()
    else {
        unreachable!("the harness just asserted this variant");
    };
    assert!(
        completion.is_some(),
        "production must park a real completion; if this fails the fixture is \
         no longer proving anything about the redacted-echo case"
    );
    runner.state_mut().waiting_for = WaitingFor::new_dig_rest_split(
        player,
        library_owner,
        cards,
        top_count,
        scope,
        source_id,
        None,
    );

    let library_before: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    let zones_before: Vec<Zone> = pile
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();

    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], pile[1]],
        })
        .expect_err("a completion-less split state must be rejected");
    assert!(
        format!("{err:?}").contains("completion"),
        "expected a missing-completion rejection, got {err:?}"
    );

    // NO PARTIAL MUTATION: the library is byte-for-byte what it was.
    let library_after: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library_after, library_before,
        "a rejected split must not have moved anything"
    );
    let zones_after: Vec<Zone> = pile
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    assert_eq!(zones_after, zones_before);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "the prompt is not consumed by a rejected submission"
    );
}

/// The positive half: with the real completion in place, the split path runs
/// the FULL `RevealRestPile` contract, not just the continuation drain.
///
/// Observable tail effects asserted here:
///   * tracked-set publication — `publish_fresh_tracked_set` inserts into
///     `tracked_object_sets` and stamps `chain_tracked_set_id`;
///   * reveal-marker cleanup — no looked-at card is left marked revealed;
///   * continuation drain — resolution actually finishes.
///
/// Reverting the fix to a bare `finish_with_continuation` after the move drops
/// the first two and fails this test.
#[test]
fn a_valid_completion_runs_the_whole_dig_tail_not_just_the_continuation() {
    let (mut runner, pile, _top_count) = telling_time_split_pause();
    let sets_before = runner.state().tracked_object_sets.len();

    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], pile[1]],
        })
        .expect("a full arrangement must be accepted");
    runner.advance_until_stack_empty();

    let st = runner.state();
    assert!(
        st.tracked_object_sets.len() > sets_before,
        "the dig tail must publish its tracked set through the split path"
    );
    assert!(
        st.chain_tracked_set_id.is_some(),
        "the published set must be wired as the chain's tracked set"
    );
    for id in &pile {
        assert!(
            !st.revealed_cards.contains(id),
            "reveal markers must be cleared by the completion tail"
        );
    }
    assert!(
        !matches!(st.waiting_for, WaitingFor::DigRestSplitChoice { .. }),
        "the continuation must drain off the split prompt"
    );
}

// ---------------------------------------------------------------------------
// Test 6 (HOSTILE): malformed arrangements are rejected at a REAL pause.
// ---------------------------------------------------------------------------

#[test]
fn split_rejects_a_partial_arrangement() {
    let (mut runner, pile, _top_count) = telling_time_split_pause();
    // A bare subset is the OLD contract. The response must arrange the whole
    // pile, or a card would be left with no position.
    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0]],
        })
        .expect_err("a subset of the pile is not a complete arrangement");
    assert!(
        format!("{err:?}").contains("exactly all"),
        "expected a completeness rejection, got {err:?}"
    );
    // REACH GUARD (paired positive): the prompt is still live and the full
    // arrangement IS accepted, so the rejection is about completeness rather
    // than an already-spent prompt.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DigRestSplitChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], pile[1]],
        })
        .expect("the complete arrangement must be accepted");
}

#[test]
fn split_rejects_an_empty_selection() {
    let (mut runner, _pile, _top_count) = telling_time_split_pause();
    runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect_err("declining is not a legal response to a forced split");
}

/// Same class again, sized so the remainder is THREE cards with two of them
/// bound for the top — a genuinely non-degenerate `C(3,2)` partition that
/// production really pauses on.
const WIDE_TOP_SPLIT_ORACLE: &str = "Look at the top four cards of your library. \
Put one of those cards into your hand, two on top of your library, and one on \
the bottom of your library.";

/// NB: this fixture reaches a GENUINE non-degenerate production pause — a
/// three-card remainder with `top_count == 2` — rather than hand-installing a
/// prompt shape. Production auto-routes nothing here, so the rejection under
/// test is about a real reachable boundary.
///
/// The duplicate id keeps the payload the right LENGTH while stranding a card,
/// which is exactly the input a membership check alone would miss.
#[test]
fn split_rejects_a_duplicate_id_at_a_real_pause() {
    let (mut runner, pile, top_count) =
        production_split_pause("Wide Top Split", WIDE_TOP_SPLIT_ORACLE, 5);
    assert_eq!(pile.len(), 3, "look at four, keep one, three remain");
    assert_eq!(top_count, 2, "a genuine C(3,2) partition choice");

    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], pile[0], pile[1]],
        })
        .expect_err("a duplicate id must be rejected");
    assert!(
        format!("{err:?}").contains("duplicate"),
        "expected a duplicate rejection, got {err:?}"
    );
    // REACH GUARD: the prompt survives and a clean permutation is accepted.
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], pile[1], pile[2]],
        })
        .expect("a clean permutation must be accepted");
}

#[test]
fn split_rejects_a_foreign_id() {
    let (mut runner, pile, _top_count) = telling_time_split_pause();
    let foreign = add_library_card(&mut runner, "Not In The Pile");
    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![pile[0], foreign],
        })
        .expect_err("an id outside the remainder pile must be rejected");
    assert!(
        format!("{err:?}").contains("not in the rest pile"),
        "expected a membership rejection, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 7 (AI): the candidate enumerator lists every legal split, as arrangements.
// ---------------------------------------------------------------------------

#[test]
fn ai_enumerates_every_legal_split_as_a_full_arrangement() {
    let (runner, pile, top_count) = telling_time_split_pause();
    assert_eq!(top_count, 1);
    let selections: Vec<Vec<ObjectId>> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::SelectCards { cards } => Some(cards),
            _ => None,
        })
        .collect();
    assert_eq!(
        selections.len(),
        2,
        "C(2,1) = 2 legal partitions, got {selections:?}"
    );
    // Each candidate is a FULL arrangement of the pile, so the engine can
    // apply it without the AI having to know the bottom complement.
    assert!(selections.contains(&vec![pile[0], pile[1]]));
    assert!(selections.contains(&vec![pile[1], pile[0]]));
}

// ---------------------------------------------------------------------------
// Test 8 (BLOCKER 3): remainder precedence across a conditional "instead"
// alternative — the alternative's own text wins over the base branch's.
// ---------------------------------------------------------------------------

/// CR 608.2c: "read the whole text and apply the rules of English to the text."
///
/// THE DISCRIMINATING HALF of Blocker 3. A conditional "instead" alternative
/// that names its OWN top/bottom split must keep it. Before this fix
/// `try_parse_dig_instead_alternative` destructured the alternative's
/// `rest_split_top_count` into `..` and threw it away, then cloned the base
/// branch's instead — so an alternative whose own sentence said "one on top of
/// your library, and one on the bottom of your library" parsed with NO split
/// and silently routed its whole remainder uniformly.
///
/// The fixture inverts the usual arrangement (plain base, splitting
/// alternative) so the alternative is the only place the split can come from.
#[test]
fn an_alternative_branch_keeps_its_own_top_bottom_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Look at the top three cards of your library. Put two of those cards into \
         your hand and the rest on the bottom of your library. If you gained life \
         this turn, you may instead put one of them into your hand, one on top of \
         your library, and one on the bottom of your library.",
        "Conditional Split Alternative",
        &[],
        &["Instant".to_string()],
        &[],
    );

    let digs = collect_dig_chain(&parsed);
    assert!(
        digs.len() >= 2,
        "expected a base Dig plus an alternative Dig, got {digs:?}"
    );

    let mut saw_alternative = false;
    let mut saw_base = false;
    for effect in &digs {
        let Effect::Dig {
            keep_count,
            rest_destination,
            rest_split_top_count,
            ..
        } = effect
        else {
            continue;
        };
        if *keep_count == Some(1) {
            // THE REGRESSION ASSERTION: the alternative's own split survives.
            assert_eq!(
                *rest_split_top_count,
                Some(QuantityExpr::Fixed { value: 1 }),
                "the alternative branch's OWN top/bottom split must be kept, \
                 not discarded in favour of the base branch's remainder"
            );
            assert_eq!(*rest_destination, Some(Zone::Library));
            saw_alternative = true;
        } else if *keep_count == Some(2) {
            // PAIRED NEGATIVE / REACH GUARD: the plain base branch really is
            // split-free, so the assertion above cannot be reading the base.
            assert_eq!(
                *rest_split_top_count, None,
                "the base branch names one uniform remainder position"
            );
            saw_base = true;
        }
    }
    assert!(
        saw_alternative && saw_base,
        "fixture must reach BOTH branches; alternative={saw_alternative}, \
         base={saw_base}"
    );
}

/// Walk an ability and its `else_ability` chain, collecting every `Effect::Dig`.
fn collect_dig_chain(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    let mut digs: Vec<&Effect> = Vec::new();
    for ability in &parsed.abilities {
        let mut current = Some(ability);
        while let Some(node) = current {
            if matches!(*node.effect, Effect::Dig { .. }) {
                digs.push(node.effect.as_ref());
            }
            current = node.else_ability.as_deref();
        }
    }
    digs
}

/// CHARACTERIZATION (not a revert-discriminating regression test — see below).
///
/// The complementary precedence rule: when the alternative names a UNIFORM
/// remainder destination, it must not inherit a split from the base branch.
///
/// This assertion is currently satisfied both with and without the precedence
/// fix, and deliberately does not claim otherwise. Reason, verified by
/// instrumenting `try_parse_dig_instead_alternative`: for the intra-chain call
/// site in `parser/oracle_effect/mod.rs` (`prev_temp`, built from
/// `prev_clause.parsed.effect` before Phase-1 assembly patches
/// `rest_split_top_count` onto the base branch), the `previous` ability it
/// reads is still the RAW look-only Dig (`destination: None, keep_count:
/// None, rest_destination: None, rest_split_top_count: None`), so
/// `prev_rest_split_top_count` is `None` there and the old unconditional
/// clone had nothing to contaminate the alternative WITH.
///
/// This does NOT cover the second call site (`oracle.rs`'s
/// `previous_spell = emitter.last_ability_definition()`), where `previous` is
/// a fully-assembled prior ability whose `rest_split_top_count` CAN be
/// non-`None`. That site is where the precedence fix is actually load-bearing,
/// and it is now covered by its own discriminating fixtures:
/// `a_cross_line_uniform_override_clears_the_inherited_split`,
/// `a_cross_line_override_keeps_its_own_split`, and the runtime
/// `a_cross_line_uniform_override_routes_uniformly_at_runtime`.
///
/// The fix is kept because the precedence it encodes is the correct reading of
/// CR 608.2c and because the sibling `alt_rest` / `alt_rest_order` fields at
/// the same site already follow it; this test pins the resulting contract so a
/// future change to continuation ordering cannot regress it unnoticed.
#[test]
fn an_alternative_with_an_explicit_uniform_remainder_carries_no_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Look at the top three cards of your library. Put one of those cards into \
         your hand, one on top of your library, and one on the bottom of your \
         library. If you gained life this turn, you may instead put two of them \
         into your hand and the rest on the bottom of your library.",
        "Conditional Split Override",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // The alternative Dig is the one carrying the appended condition; the base
    // Dig is wired behind it as the else-branch.
    let digs = collect_dig_chain(&parsed);
    assert!(
        digs.len() >= 2,
        "expected a base Dig plus an alternative Dig, got {digs:?}"
    );

    let mut saw_split_base = false;
    let mut saw_uniform_alternative = false;
    for effect in &digs {
        let Effect::Dig {
            keep_count,
            rest_destination,
            rest_split_top_count,
            ..
        } = effect
        else {
            continue;
        };
        if *keep_count == Some(2) {
            // The alternative branch named a single uniform remainder
            // position, so it must carry NO split.
            assert_eq!(
                *rest_split_top_count, None,
                "an explicit uniform remainder must override the base branch's \
                 split, not inherit it"
            );
            assert_eq!(
                *rest_destination,
                Some(Zone::Library),
                "the alternative's own remainder destination is the library bottom"
            );
            saw_uniform_alternative = true;
        } else if *keep_count == Some(1) {
            // PAIRED POSITIVE / REACH GUARD: the base branch really does carry
            // a split, so the assertion above is proving an override rather
            // than passing because nothing was ever set.
            assert_eq!(
                *rest_split_top_count,
                Some(QuantityExpr::Fixed { value: 1 }),
                "the base branch keeps its own top/bottom split"
            );
            saw_split_base = true;
        }
    }
    assert!(
        saw_split_base && saw_uniform_alternative,
        "fixture must reach BOTH branches; base={saw_split_base}, \
         alternative={saw_uniform_alternative}"
    );
}

/// STATE-LEVEL CONTRACT, deliberately NOT a regression test for the parser
/// precedence fix.
///
/// It installs a `DigChoice` directly, so it never runs the parser and cannot
/// fail if `try_parse_dig_instead_alternative`'s precedence is reverted. What
/// it does pin is one narrow resolver contract the sibling
/// `non_splitting_dig_still_auto_routes_its_whole_remainder` does not cover:
/// `rest_split_top_count: None` with a LIBRARY rest destination routes the
/// whole remainder to the bottom with no split prompt (that sibling uses a
/// graveyard destination, which cannot reach the split gate at all).
///
/// The discriminating runtime tests for the precedence fix are
/// `a_conditional_alternative_with_its_own_split_splits_at_runtime` and
/// `a_cross_line_uniform_override_routes_uniformly_at_runtime` below, which
/// parse real Oracle text and cast it.
#[test]
fn a_library_bound_remainder_with_no_split_routes_uniformly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let a = add_library_card(&mut runner, "Alt A");
    let b = add_library_card(&mut runner, "Alt B");
    let c = add_library_card(&mut runner, "Alt C");
    // The alternative branch's resolved shape: keep 2, remainder uniformly to
    // the library bottom, `rest_split_top_count: None`.
    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![a, b, c],
        keep_count: 2,
        up_to: false,
        selectable_cards: vec![a, b, c],
        kept_destination: Some(Zone::Hand),
        rest_destination: Some(Zone::Library),
        rest_split_top_count: None,
        rest_order: engine::types::ability::DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("keeping two of three must be accepted");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "an overridden split must never raise the split prompt"
    );
    runner.advance_until_stack_empty();
    let st = runner.state();
    assert_eq!(st.objects[&c].zone, Zone::Library);
    assert_eq!(
        st.players[0].library.iter().copied().last(),
        Some(c),
        "the remainder went uniformly to the library BOTTOM"
    );
}

// ---------------------------------------------------------------------------
// Test 9 (SIBLING REGRESSION): a non-splitting dig is entirely unaffected.
// ---------------------------------------------------------------------------

/// `rest_split_top_count: None` must still auto-route the whole remainder to
/// `rest_destination` with NO second prompt — the unchanged path through
/// `route_rest_partition_then`.
#[test]
fn non_splitting_dig_still_auto_routes_its_whole_remainder() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let a = add_library_card(&mut runner, "Dug A");
    let b = add_library_card(&mut runner, "Dug B");
    let c = add_library_card(&mut runner, "Dug C");
    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: vec![a, b, c],
        keep_count: 1,
        up_to: false,
        selectable_cards: vec![a, b, c],
        kept_destination: Some(Zone::Hand),
        rest_destination: Some(Zone::Graveyard),
        rest_split_top_count: None,
        rest_order: engine::types::ability::DigRestOrder::Preserve,
        source_id: None,
        enter_tapped: false,
        enters_attacking: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keeping one of three must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "a dig with no split must never raise the split prompt"
    );
    let st = runner.state();
    assert_eq!(st.objects[&a].zone, Zone::Hand);
    assert_eq!(
        st.objects[&b].zone,
        Zone::Graveyard,
        "the whole remainder still goes uniformly to rest_destination"
    );
    assert_eq!(st.objects[&c].zone, Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Test 10 (BLOCKER 1): the CR 401.4 ORDERING authority is the library's OWNER,
// which is not always the player the effect gave the partition to.
// ---------------------------------------------------------------------------

/// Build and cast a synthetic cross-player dig: "Look at the top `count` cards
/// of TARGET PLAYER's library. Put one of them into your hand, `top` on top of
/// that player's library, and the rest on the bottom."
///
/// No printed card prints this shape today, so the `Effect::Dig` is assembled
/// here — but it is assembled as a real ability on a real card and driven
/// through the real cast pipeline (`GameRunner::cast(..).resolve()`), so every
/// prompt under test is one production actually parks. Hand-installing a
/// `WaitingFor` would assert about a state the engine may never produce, which
/// is exactly the defect this blocker is about.
fn cross_player_split_cast(
    count: i32,
    top: i32,
    library_cards: usize,
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder = scenario.add_spell_to_hand(P0, "Borrowed Foresight", true);
    builder.with_mana_cost(telling_time_cost());
    builder.with_ability(Effect::Dig {
        // "target player's library" — the axis that makes the chooser (P0, the
        // spell's controller) and the library's owner (P1) different players.
        player: TargetFilter::Player,
        count: QuantityExpr::Fixed { value: count },
        destination: Some(Zone::Hand),
        keep_count: Some(1),
        keep_count_expr: None,
        up_to: false,
        filter: TargetFilter::Any,
        rest_destination: Some(Zone::Library),
        rest_split_top_count: Some(QuantityExpr::Fixed { value: top }),
        rest_order: DigRestOrder::Preserve,
        reveal: false,
        enter_tapped: false,
        enters_attacking: false,
        source: DigSource::Library,
    });
    let spell_id = builder.id();
    let mut runner = scenario.build();

    for index in 0..library_cards {
        add_library_card_for(&mut runner, P1, &format!("Theirs{index}"));
    }
    add_mana(&mut runner, ManaType::Blue, 2);

    let outcome = runner.cast(spell_id).target_player(P1).resolve();
    let looked_at = match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            cards,
            library_owner,
            player,
            ..
        } => {
            assert_eq!(*library_owner, P1, "the dig reads P1's library");
            assert_eq!(*player, P0, "P0 is the chooser");
            cards.clone()
        }
        other => panic!("expected the keep prompt first, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0]],
        })
        .expect("keeping one looked-at card must be accepted");
    (runner, looked_at)
}

/// CR 401.4: "If an effect puts two or more cards in a specific position in a
/// library at the same time, **the owner of those cards** may arrange them in
/// any order."
///
/// The reviewer's minimal case. P0 digs P1's library and the whole three-card
/// remainder goes to the BOTTOM (`top_count == 0`), so there is no partition
/// decision at all — the only decision left is CR 401.4's arrangement, and it
/// belongs to P1.
///
/// THE REGRESSION ASSERTION is the acting player: before this fix the prompt
/// was parked for `player` (P0, the chooser) unconditionally, so P0 both made a
/// decision that was never theirs and P1's submission was refused.
#[test]
fn a_degenerate_cross_player_split_asks_the_library_owner_for_the_order() {
    let (mut runner, _looked_at) = cross_player_split_cast(4, 0, 5);

    let (acting, owner, pile, scope) = match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            player,
            library_owner,
            cards,
            scope,
            ..
        } => (*player, *library_owner, cards.clone(), *scope),
        other => panic!("expected a split prompt after the keep step, got {other:?}"),
    };
    assert_eq!(pile.len(), 3, "look at four, keep one, three remain");
    assert_eq!(owner, P1);
    // THE REGRESSION ASSERTION.
    assert_eq!(
        acting, P1,
        "CR 401.4 gives the arrangement of a 2+ card pile to the OWNER of those \
         cards (P1), not to the player the effect chose for (P0)"
    );
    assert_eq!(
        scope,
        DigRestSplitScope::OrderOnly,
        "a forced partition leaves only the CR 401.4 arrangement"
    );
    // PAIRED NEGATIVE: the acting-authority census agrees, so the multiplayer
    // server routes the prompt to P1 too rather than just this assertion
    // reading a field nobody consults.
    assert_eq!(
        runner.state().waiting_for.acting_authority(),
        engine::types::game_state::ActingAuthority::One(P1),
        "the engine's own acting-authority answer must name the owner, not the \
         chooser — this is what routes the prompt in multiplayer"
    );

    // (b) the final library order is what the OWNER submitted. Reverse the
    // pile so encounter order and submitted order disagree.
    let submitted = vec![pile[2], pile[0], pile[1]];
    runner
        .act(GameAction::SelectCards {
            cards: submitted.clone(),
        })
        .expect("the owner's full arrangement must be accepted");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[1].library.iter().copied().collect();
    let tail: Vec<ObjectId> = library[library.len() - 3..].to_vec();
    assert_eq!(
        tail, submitted,
        "the bottom pile must read back in the owner's submitted order"
    );
    assert_ne!(
        submitted, pile,
        "the fixture must submit an order that differs from the pile order"
    );
}

/// The complementary half: when the partition IS a genuine choice, it stays
/// with the chooser (CR 608.2d — the effect says "*you* put ... on top") and
/// only the CR 401.4 arrangement moves to the owner. Two prompts, two actors.
#[test]
fn a_genuine_cross_player_split_partitions_then_hands_the_order_to_the_owner() {
    // Look at 5, keep 1 -> a 4-card remainder, 2 on top and 2 on the bottom:
    // a real partition AND both resulting piles need a CR 401.4 order.
    let (mut runner, _looked_at) = cross_player_split_cast(5, 2, 6);

    let (acting, pile, scope) = match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            player,
            cards,
            scope,
            ..
        } => (*player, cards.clone(), *scope),
        other => panic!("expected a partition prompt, got {other:?}"),
    };
    assert_eq!(pile.len(), 4);
    assert_eq!(
        acting, P0,
        "CR 608.2d: the partition is the chooser's decision"
    );
    assert_eq!(scope, DigRestSplitScope::PartitionOnly);

    // P0 partitions: pile[3] and pile[0] take the top.
    let partition = vec![pile[3], pile[0], pile[2], pile[1]];
    runner
        .act(GameAction::SelectCards {
            cards: partition.clone(),
        })
        .expect("the chooser's partition must be accepted");

    // THE REGRESSION ASSERTION: a SECOND prompt, addressed to the owner.
    let (order_actor, order_pile, order_scope, order_top) = match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            player,
            cards,
            scope,
            top_count,
            ..
        } => (*player, cards.clone(), *scope, *top_count),
        other => panic!("expected an owner-addressed order prompt, got {other:?}"),
    };
    assert_eq!(
        order_actor, P1,
        "CR 401.4: each 2+ card pile is arranged by the library's owner"
    );
    assert_eq!(order_scope, DigRestSplitScope::OrderOnly);
    assert_eq!(
        order_pile, partition,
        "the settled partition is carried over"
    );
    assert_eq!(order_top, 2);

    // HOSTILE: the owner may reorder within a pile but may not re-partition —
    // that decision was P0's and is already spent.
    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![pile[1], pile[2], pile[0], pile[3]],
        })
        .expect_err("the owner must not be able to change which cards go on top");
    assert!(
        format!("{err:?}").contains("may not change which cards are on top"),
        "expected a partition-preservation rejection, got {err:?}"
    );

    // The owner swaps the order WITHIN each pile; that is legal and binding.
    let owner_order = vec![partition[1], partition[0], partition[3], partition[2]];
    runner
        .act(GameAction::SelectCards {
            cards: owner_order.clone(),
        })
        .expect("a within-pile reorder must be accepted");
    runner.advance_until_stack_empty();

    let library: Vec<ObjectId> = runner.state().players[1].library.iter().copied().collect();
    assert_eq!(
        &library[..2],
        &owner_order[..2],
        "the top pile reads back in the OWNER's order, topmost first"
    );
    assert_eq!(
        &library[library.len() - 2..],
        &owner_order[2..],
        "and so does the bottom pile"
    );
}

/// PAIRED NEGATIVE / NO-CHANGE GUARD for the common case: when the chooser IS
/// the library's owner (every printed card today, Telling Time included), both
/// decisions are the same player's and exactly ONE prompt is parked. This is
/// what keeps the cross-player fix from silently adding a second prompt to
/// Telling Time.
#[test]
fn a_same_player_split_still_answers_both_decisions_in_one_prompt() {
    let (mut runner, pile, _top_count) =
        production_split_pause("Wide Top Split", WIDE_TOP_SPLIT_ORACLE, 5);
    match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            player,
            library_owner,
            scope,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*library_owner, P0);
            assert_eq!(
                *scope,
                DigRestSplitScope::PartitionAndOrder,
                "one player owns both decisions, so one prompt answers both"
            );
        }
        other => panic!("expected a split prompt, got {other:?}"),
    }
    // A single submission finishes the split — no second prompt appears.
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[2], pile[0], pile[1]],
        })
        .expect("the full arrangement must be accepted");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "a same-player split must not chain a second arrangement prompt"
    );
}

// ---------------------------------------------------------------------------
// Test 11 (BLOCKER 3): the "instead"-alternative precedence at BOTH call
// sites, proven through the real parser and — where the branch is reachable —
// a real cast.
// ---------------------------------------------------------------------------

/// Walk an ability's `sub_ability` chain, collecting every `Effect::Dig`. The
/// CROSS-LINE binder (`oracle.rs`'s `previous_spell =
/// emitter.last_ability_definition()` call site) parks the alternative as the
/// printed Dig's `sub_ability`, where `collect_dig_chain`'s `else_ability` walk
/// cannot see it.
fn cross_line_alternative_dig(parsed: &engine::parser::oracle::ParsedAbilities) -> &Effect {
    let base = parsed
        .abilities
        .first()
        .expect("the two-line card must publish one bound ability, not two siblings");
    assert!(
        matches!(*base.effect, Effect::Dig { .. }),
        "line 1 must stay the printed Dig, got {:?}",
        base.effect
    );
    let sub = base
        .sub_ability
        .as_ref()
        .expect("the ability-word override must BIND to the Dig as its sub_ability");
    assert!(
        matches!(
            sub.condition,
            Some(engine::types::ability::AbilityCondition::ConditionInstead { .. })
        ),
        "CR 614.15: the override is a ConditionInstead branch, got {:?}",
        sub.condition
    );
    sub.effect.as_ref()
}

fn dig_split(effect: &Effect) -> Option<QuantityExpr> {
    match effect {
        Effect::Dig {
            rest_split_top_count,
            ..
        } => rest_split_top_count.clone(),
        other => panic!("expected Effect::Dig, got {other:?}"),
    }
}

/// Line 1 is a complete split-Dig (Telling Time's own shape); line 2 is a
/// separate ability-word "instead" override naming a UNIFORM remainder.
const CROSS_LINE_UNIFORM_OVERRIDE: &str = "Look at the top three cards of your library. \
Put one of those cards into your hand, one on top of your library, and one on the bottom \
of your library.\nSpell mastery — If there are two or more instant and/or sorcery cards \
in your graveyard, put two of those cards into your hand and the rest on the bottom of \
your library instead.";

/// Line 1 is a plain uniform-remainder Dig; line 2's override names its OWN
/// top/bottom split.
const CROSS_LINE_OWN_SPLIT_OVERRIDE: &str = "Look at the top four cards of your library. \
Put one of those cards into your hand and the rest on the bottom of your library.\n\
Spell mastery — If there are two or more instant and/or sorcery cards in your graveyard, \
put one of those cards into your hand, two on top of your library, and one on the bottom \
of your library instead.";

/// CR 608.2c, at the CROSS-LINE call site (`parser/oracle.rs`'s
/// `previous_spell = emitter.last_ability_definition()`).
///
/// This is the site the prior round's characterization test explicitly left
/// unexercised. Unlike the intra-chain site, `previous` here is a
/// FULLY-ASSEMBLED prior ability, so its `rest_split_top_count` really is
/// `Some(1)` when `try_parse_dig_instead_alternative` runs — which makes the
/// old unconditional `prev_rest_split_top_count.clone()` actively wrong rather
/// than merely redundant.
///
/// THE REGRESSION ASSERTION: the override named a single uniform remainder
/// position, so it must carry NO split. Reverting the precedence to the
/// unconditional clone gives the alternative the base line's `Some(1)`.
#[test]
fn a_cross_line_uniform_override_clears_the_inherited_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        CROSS_LINE_UNIFORM_OVERRIDE,
        "Cross Line Uniform Override",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let base = parsed.abilities[0].effect.as_ref();
    // REACH GUARD / PAIRED POSITIVE: the base line really does carry a split,
    // so there is something for the alternative to have wrongly inherited.
    assert_eq!(
        dig_split(base),
        Some(QuantityExpr::Fixed { value: 1 }),
        "line 1 must keep its own top/bottom split"
    );
    assert_eq!(
        dig_split(cross_line_alternative_dig(&parsed)),
        None,
        "an explicit uniform remainder on the override must override the base \
         line's split, not inherit it"
    );
}

/// The complementary precedence rule at the same cross-line site: an override
/// that names its OWN split keeps it.
///
/// THE REGRESSION ASSERTION: reverting to the unconditional
/// `prev_rest_split_top_count.clone()` discards the override's own `Some(2)`
/// and substitutes the base line's `None`.
#[test]
fn a_cross_line_override_keeps_its_own_split() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        CROSS_LINE_OWN_SPLIT_OVERRIDE,
        "Cross Line Own Split Override",
        &[],
        &["Instant".to_string()],
        &[],
    );
    // REACH GUARD: the base line is split-free, so the assertion below cannot
    // be reading an inherited value.
    assert_eq!(dig_split(parsed.abilities[0].effect.as_ref()), None);
    assert_eq!(
        dig_split(cross_line_alternative_dig(&parsed)),
        Some(QuantityExpr::Fixed { value: 2 }),
        "the override's OWN top/bottom split must survive the rebuild"
    );
}

/// Cast a synthetic conditional-Dig card for real, with `graveyard_spells`
/// instants already in P0's graveyard so a "spell mastery" style condition can
/// be turned on or off, and `creatures` creatures on the battlefield for the
/// "if you control a creature" condition.
fn cast_conditional_dig(
    name: &str,
    oracle: &str,
    library_cards: usize,
    graveyard_spells: usize,
    creatures: usize,
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for index in 0..graveyard_spells {
        scenario.add_spell_to_graveyard(P0, &format!("Spent Instant {index}"), true);
    }
    for index in 0..creatures {
        scenario.add_creature(P0, &format!("Witness {index}"), 1, 1);
    }
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle);
    builder.with_mana_cost(telling_time_cost());
    let spell_id = builder.id();
    let mut runner = scenario.build();
    for index in 0..library_cards {
        add_library_card(&mut runner, &format!("Cond{index}"));
    }
    add_mana(&mut runner, ManaType::Blue, 2);

    let outcome = runner.cast(spell_id).resolve();
    let looked_at = match outcome.final_waiting_for() {
        WaitingFor::DigChoice { cards, .. } => cards.clone(),
        other => panic!("expected the keep prompt first, got {other:?}"),
    };
    (runner, looked_at)
}

/// Base branch names a uniform remainder; the CONDITIONAL ALTERNATIVE names its
/// own top/bottom split. This is the runtime half of
/// `an_alternative_branch_keeps_its_own_top_bottom_split` (which only asserted
/// AST shape) for the INTRA-CHAIN call site.
const CONDITIONAL_OWN_SPLIT: &str = "Look at the top three cards of your library. \
Put two of those cards into your hand and the rest on the bottom of your library. \
If you control a creature, you may instead put one of them into your hand, one on top \
of your library, and one on the bottom of your library.";

/// CR 608.2c: parse real Oracle text, CAST it, and assert where the cards
/// actually land.
///
/// With a creature on the battlefield the conditional alternative is the branch
/// that runs, so its OWN split must produce a real `DigRestSplitChoice` and a
/// real top/bottom placement.
///
/// THE REGRESSION ASSERTION is the prompt plus the final library order:
/// reverting `try_parse_dig_instead_alternative` to discard the alternative's
/// `rest_split_top_count` leaves the alternative split-free, so no split prompt
/// is ever parked and `act` on the arrangement fails outright.
#[test]
fn a_conditional_alternative_with_its_own_split_splits_at_runtime() {
    let (mut runner, looked_at) = cast_conditional_dig(
        "Conditional Split Alternative",
        CONDITIONAL_OWN_SPLIT,
        4,
        0,
        1,
    );
    assert_eq!(looked_at.len(), 3, "look at the top three");
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0]],
        })
        .expect("the alternative keeps exactly ONE card, not the base branch's two");

    // THE REGRESSION ASSERTION #1: the alternative's split reached the resolver.
    let pile = match &runner.state().waiting_for {
        WaitingFor::DigRestSplitChoice {
            cards, top_count, ..
        } => {
            assert_eq!(*top_count, 1, "one of the remainder goes on top");
            cards.clone()
        }
        other => {
            panic!("the alternative branch's own split must raise a split prompt, got {other:?}")
        }
    };
    assert_eq!(pile.len(), 2);

    // REGRESSION ASSERTION #2: the cards land where the arrangement says.
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile[1], pile[0]],
        })
        .expect("a full arrangement must be accepted");
    runner.advance_until_stack_empty();

    let st = runner.state();
    let library: Vec<ObjectId> = st.players[0].library.iter().copied().collect();
    assert_eq!(st.objects[&looked_at[0]].zone, Zone::Hand);
    assert_eq!(library.first(), Some(&pile[1]), "chosen card on TOP");
    assert_eq!(library.last(), Some(&pile[0]), "the other on the BOTTOM");
}

/// PAIRED NEGATIVE for the test above: with NO creature the CONDITION is false,
/// so the base branch runs — it keeps two and routes its whole remainder
/// uniformly, with no split prompt. Without this, the test above could pass
/// against a parser that put a split on both branches.
#[test]
fn the_base_branch_of_the_same_card_routes_uniformly_at_runtime() {
    let (mut runner, looked_at) = cast_conditional_dig(
        "Conditional Split Alternative",
        CONDITIONAL_OWN_SPLIT,
        4,
        0,
        0,
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0], looked_at[1]],
        })
        .expect("the base branch keeps TWO cards");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "the base branch names one uniform remainder position and must not split"
    );
    runner.advance_until_stack_empty();
    let st = runner.state();
    assert_eq!(
        st.players[0].library.iter().copied().last(),
        Some(looked_at[2]),
        "the remainder went uniformly to the library BOTTOM"
    );
}

/// RUNTIME discrimination for the CROSS-LINE call site: the same two-line card
/// as `a_cross_line_uniform_override_clears_the_inherited_split`, cast for real
/// with spell mastery ON so the override is the branch that runs.
///
/// THE REGRESSION ASSERTION: the override's uniform remainder must route with
/// NO split prompt. Reverting the precedence makes the override inherit line
/// 1's `Some(1)`, which parks a `DigRestSplitChoice` the card's own text never
/// asks for — and this test fails on the very next line.
#[test]
fn a_cross_line_uniform_override_routes_uniformly_at_runtime() {
    let (mut runner, looked_at) = cast_conditional_dig(
        "Cross Line Uniform Override",
        CROSS_LINE_UNIFORM_OVERRIDE,
        4,
        2,
        0,
    );
    assert_eq!(looked_at.len(), 3, "line 1's source (top three) is reused");
    runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[0], looked_at[1]],
        })
        .expect("spell mastery is on, so the override keeps TWO cards");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigRestSplitChoice { .. }
        ),
        "the override's explicit uniform remainder must not inherit line 1's split"
    );
    runner.advance_until_stack_empty();
    let st = runner.state();
    assert_eq!(st.objects[&looked_at[0]].zone, Zone::Hand);
    assert_eq!(st.objects[&looked_at[1]].zone, Zone::Hand);
    assert_eq!(
        st.players[0].library.iter().copied().last(),
        Some(looked_at[2]),
        "the whole remainder went uniformly to the library BOTTOM"
    );
}

// ---------------------------------------------------------------------------
// Test 12 (COVERAGE): the client-facing projection carries the WHOLE prompt.
// ---------------------------------------------------------------------------

/// `game/visibility.rs` rebuilds `DigRestSplitChoice` field-by-field for each
/// viewer, which is exactly the seam where a newly added field gets silently
/// dropped on its way to a client. This pins the round trip end to end:
/// production pause -> viewer projection -> JSON -> back, for the viewer who
/// must answer the prompt.
///
/// No field-loss defect is known today; this is the coverage that makes the
/// next one loud instead of silent.
#[test]
fn the_split_prompt_survives_the_viewer_projection_and_a_json_round_trip() {
    let (runner, pile, top_count) =
        production_split_pause("Wide Top Split", WIDE_TOP_SPLIT_ORACLE, 5);
    assert_eq!(pile.len(), 3);
    assert_eq!(top_count, 2);

    let projected = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    let json = serde_json::to_string(&projected.waiting_for).expect("prompt must serialize");
    let restored: WaitingFor = serde_json::from_str(&json).expect("prompt must deserialize");

    match restored {
        WaitingFor::DigRestSplitChoice {
            player,
            library_owner,
            cards,
            top_count: restored_top,
            bottom_count,
            scope,
            source_id,
            completion,
        } => {
            assert_eq!(player, P0, "the acting player survives");
            assert_eq!(library_owner, P0, "the CR 401.4 authority survives");
            assert_eq!(cards, pile, "the acting viewer sees real card identities");
            assert_eq!(restored_top, top_count);
            // NON-BLOCKING 2: the bottom half of the prompt's own description
            // is engine-supplied, so the client formats rather than derives.
            assert_eq!(
                bottom_count,
                pile.len() - top_count,
                "the engine states the bottom count instead of leaving the \
                 client to compute `cards.length - top_count`"
            );
            assert_eq!(scope, DigRestSplitScope::PartitionAndOrder);
            assert!(source_id.is_some(), "the prompt's source survives");
            // The one field that is deliberately NOT shipped: engine-internal
            // bookkeeping holding the same private ids.
            assert!(
                completion.is_none(),
                "the deferred dig tail is engine-internal and must be stripped"
            );
        }
        other => panic!("expected DigRestSplitChoice, got {other:?}"),
    }
}

/// PAIRED NEGATIVE: a viewer who is NOT the acting player still receives a
/// prompt of the right SHAPE — same counts, same scope — but with the card
/// identities redacted (CR 701.20e: the remainder was shown only to the
/// looking player). The engine-supplied counts must not be redacted with them,
/// or the observer's UI would have to derive them back.
#[test]
fn a_non_acting_viewer_sees_the_counts_but_not_the_identities() {
    let (runner, pile, top_count) =
        production_split_pause("Wide Top Split", WIDE_TOP_SPLIT_ORACLE, 5);
    let projected = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    match &projected.waiting_for {
        WaitingFor::DigRestSplitChoice {
            cards,
            top_count: seen_top,
            bottom_count,
            scope,
            ..
        } => {
            assert_eq!(cards.len(), pile.len(), "the pile SIZE is public");
            assert!(
                cards.iter().all(|id| *id == ObjectId(0)),
                "an opponent must not learn which cards are in the pile"
            );
            assert_eq!(*seen_top, top_count);
            assert_eq!(*bottom_count, pile.len() - top_count);
            assert_eq!(*scope, DigRestSplitScope::PartitionAndOrder);
        }
        other => panic!("expected DigRestSplitChoice, got {other:?}"),
    }
}
