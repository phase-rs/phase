//! Issue #6942: a `DigChoice` whose filter (or a short library) leaves fewer
//! selectable cards than `keep_count` must accept the largest possible
//! selection, not reject every selection.
//!
//! CR 609.3 ("If an effect attempts to do something impossible, it does only as
//! much as possible") and CR 101.3 ("Any part of an instruction that's
//! impossible to perform is ignored"). Before the fix, the exact-cardinality
//! gate in `engine_resolution_choices.rs` demanded `keep_count` ids while
//! `validate_dig_selection` required every kept id to be in `selectable_cards`
//! — so when `selectable_cards.len() < keep_count` the two rules had NO common
//! solution and every controller (AI, human, multiplayer server) softlocked.
//!
//! The candidate enumerator (`ai_support/candidates.rs`) and
//! `cheap_reject_candidate` (`ai_support/mod.rs`) already clamped to
//! `keep_count.min(selectable_cards.len())`; the resolution handler was the
//! outlier.
use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);

/// A filtered dig that looked at three cards but whose filter matched only one,
/// with `keep_count: 2` and `up_to: false`.
///
/// This is the shape `effects/dig.rs` produces: `selectable_cards` is pruned by
/// the effect's filter while `keep_count` stays at the card-literal value.
fn filtered_dig_runner() -> (engine::game::scenario::GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looked_at: Vec<ObjectId> = ["Dug One", "Dug Two", "Dug Three"]
        .iter()
        .map(|name| scenario.add_spell_to_library_top(P0, name, false).id())
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P0,
        library_owner: P0,
        cards: looked_at.clone(),
        keep_count: 2,
        up_to: false,
        // The filter matched exactly one of the three looked-at cards.
        selectable_cards: vec![looked_at[0]],
        kept_destination: Some(Zone::Hand),
        rest_destination: Some(Zone::Graveyard),
        source_id: None,
        enter_tapped: false,
    };
    (runner, looked_at)
}

/// PAIRED NEGATIVE, run first because it must leave the prompt intact: the
/// clamp relaxes the *cardinality* gate only. A selection of the clamped size
/// whose id is outside `selectable_cards` is still rejected by
/// `validate_dig_selection`, so the filter check was not disabled.
#[test]
fn dig_clamp_does_not_disable_the_filter_check() {
    let (mut runner, looked_at) = filtered_dig_runner();

    let err = runner
        .act(GameAction::SelectCards {
            cards: vec![looked_at[1]],
        })
        .expect_err("a non-matching id must still be refused");
    assert!(
        format!("{err:?}").contains("does not match the effect's filter"),
        "the refusal must come from validate_dig_selection, not the cardinality \
         gate — got {err:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "a refused selection must leave the prompt pending"
    );
}

/// MAIN TEST. FAILS BEFORE THE FIX: `kept.len() != keep_count` evaluates
/// `1 != 2` and returns `InvalidAction("Must select exactly 2 cards, got 1")`,
/// while every larger selection is refused by `validate_dig_selection` — no
/// legal action exists.
#[test]
fn dig_with_fewer_selectable_cards_than_keep_count_keeps_as_many_as_possible() {
    let (mut runner, looked_at) = filtered_dig_runner();
    let (kept, unkept) = (looked_at[0], [looked_at[1], looked_at[2]]);

    runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect(
            "CR 609.3: the only selection the filter permits must be accepted \
             when keep_count exceeds the selectable set",
        );

    let hand = &runner.state().players[P0.0 as usize].hand;
    assert!(
        hand.contains(&kept),
        "the single filter-matching card must reach kept_destination (hand)"
    );
    let graveyard = &runner.state().players[P0.0 as usize].graveyard;
    for id in unkept {
        assert!(
            graveyard.contains(&id),
            "the unkept cards must reach rest_destination (graveyard); \
             graveyard = {graveyard:?}"
        );
    }
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "the dig prompt must be resolved, not re-parked"
    );
}
