//! CR 732.2b — which places a responder may name when shortening a proposed loop shortcut.
//!
//! The proposal owns the range (`ShortcutProposal::shortening_places`); `apply()` enforces it,
//! the interaction projection publishes its two ends, and the engine-driven seat's own emitter
//! reads it. Every row drives real `apply()` calls on a board reached through a production path
//! — the restored four-seat drain dump, or the driven two-player optional-drain rig — the
//! zero-count proposal included: the handler floors nothing at zero, so a declaration naming it
//! mints a proposal and opens the poll on a range that holds no place at all.

use engine::ai_support::legal_actions;
use engine::analysis::decision_template::{IterationCount, ShortcutDecisionSchema};
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::engine::{apply, EngineError};
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::printed_cards::apply_card_face_to_object;
use engine::game::scenario::{GameRunner, P0};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponse, InteractionResponseSpec,
    InteractionSessionId, InteractionShortcutReply, InteractionSubmission,
};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::loop_shortcut::{gunzip_dump, reach_2p_optional_drain_offer, restore_dump};
use crate::loop_shortcut_drain_boards::weird_drain_board;

/// The seat now being polled, and the seats still queued behind it.
fn window(state: &GameState) -> (PlayerId, Vec<PlayerId>) {
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "expected a live responder window, got {:?}",
            state.waiting_for
        );
    };
    (*player, remaining_players.clone())
}

/// The range the proposal on this window admits.
fn admitted_places(state: &GameState) -> std::ops::RangeInclusive<u32> {
    let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
        panic!(
            "expected a live responder window, got {:?}",
            state.waiting_for
        );
    };
    proposal.shortening_places()
}

/// Submit a raw `GameAction` as the seat the window is polling — the boundary every emitter
/// crosses, and the one the browser and a restored save reach without the interaction layer.
fn respond(state: &mut GameState, response: ShortcutResponse) -> Result<(), EngineError> {
    let (player, _) = window(state);
    apply(state, player, GameAction::RespondToShortcut { response }).map(|_| ())
}

fn shorten(state: &mut GameState, at_iteration: u32) -> Result<(), EngineError> {
    respond(state, ShortcutResponse::Shorten { at_iteration })
}

/// The restored four-seat drain dump, declared at `count` and parked at its first responder.
/// Its offer is directly declarable, so no drive stands between the restore and the window.
fn weird_window(count: IterationCount) -> GameState {
    let mut state = weird_drain_board();
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for.clone() else {
        panic!(
            "the weird-drain fixture restores at its offer, got {:?}",
            state.waiting_for
        );
    };
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: count.clone(),
            template: None,
        },
    )
    .unwrap_or_else(|error| panic!("declaring {count:?} on the drain dump: {error:?}"));
    let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
        panic!(
            "declaring {count:?} must open a responder window, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        proposal.count, count,
        "the window carries the count that was declared"
    );
    state
}

/// The largest `Fixed` count this board's offer will accept, read off the offer itself so no row
/// transcribes a bound.
fn weird_offer_bound() -> u32 {
    let state = weird_drain_board();
    let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
        panic!(
            "the weird-drain fixture restores at its offer, got {:?}",
            state.waiting_for
        );
    };
    schema.max_iterations
}

/// The driven two-player optional-drain rig, declared at `count` and parked at its responder.
/// This is the only reachable board whose schema admits `UntilLethal`: both committed dumps
/// refuse that declaration against their bounded offers.
fn rig_window(count: IterationCount) -> GameState {
    let (mut runner, _life, _cleric) = reach_2p_optional_drain_offer();
    runner
        .act(GameAction::DeclareShortcut {
            count: count.clone(),
            template: None,
        })
        .unwrap_or_else(|error| panic!("declaring {count:?} on the 2p rig: {error:?}"));
    let state = runner.state().clone();
    let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
        panic!(
            "declaring {count:?} must open a responder window, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        proposal.count, count,
        "the window carries the count that was declared"
    );
    state
}

/// CR 732.2b: a place at or past the proposed count names no choice different from what was
/// proposed, so `apply()` refuses it — while the last place in the sequence is answered on the
/// same board, from the state the refusal left behind. That answer rewrites the count, so the
/// seat still queued is polled on the shortened proposal and the ending point lands only once
/// the poll completes.
#[test]
fn a_place_at_the_proposed_count_is_refused_and_the_place_below_it_is_answered() {
    let count = weird_offer_bound();
    let mut state = weird_window(IterationCount::Fixed(count));
    let (responder, queued) = window(&state);

    let refused = shorten(&mut state, count);
    assert!(
        matches!(refused, Err(EngineError::InvalidAction(_))),
        "place {count} is outside a {count}-repetition proposal: {refused:?}"
    );
    assert_eq!(
        window(&state),
        (responder, queued.clone()),
        "a refused response leaves the poll exactly as it found it"
    );

    shorten(&mut state, count - 1).expect("the last place in the sequence is a legal answer");
    assert_eq!(
        window(&state).0,
        *queued
            .first()
            .expect("a second responder is queued on this board"),
        "CR 732.2b: the shortened proposal is put to the seat still queued behind the shortener"
    );
    assert_eq!(
        admitted_places(&state),
        0..=count - 2,
        "the queued seat answers the SHORTENED count, so its own range ends one below it"
    );

    respond(&mut state, ShortcutResponse::Accept).expect("the last seat accepts");
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: responder },
        "CR 732.2b/c: the shortcut is taken to the place the shortener named, and they hold it"
    );
}

/// CR 704.5a: an `UntilLethal` proposal is ended by the loss state-based action rather than by
/// a count, so it names no place past its end and CR 732.2b's range is not reopened. The largest
/// place it admits would rewrite the count above the budget this engine drives, so `apply()`
/// refuses it — before the event take and before any state write, which is why the poll is still
/// the responder's afterwards and the next place they name is taken on the same board.
///
/// The window clause is what a bare `Err` cannot be: the refusal is scoped to the answer, not to
/// the seat, so the same responder answers again and the shortcut is then TAKEN. Reds under
/// restoring the responder's seam to its old `Ok` path — the place is answered for, the shortcut
/// is dropped, and the window the second leg runs from is gone.
#[test]
fn the_largest_place_an_until_lethal_proposal_admits_is_refused_at_the_responders_seam() {
    let mut state = rig_window(IterationCount::UntilLethal);
    let poll = window(&state);
    assert!(
        poll.1.is_empty(),
        "reach-guard: this shortener is the last responder, so nothing below is attributable to \
         a queued seat"
    );
    let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();

    let refused = shorten(&mut state, u32::MAX);
    assert!(
        matches!(refused, Err(EngineError::InvalidAction(_))),
        "the largest place an unbounded proposal admits is one this engine will not drive: \
         {refused:?}"
    );
    assert_eq!(
        window(&state),
        poll,
        "the refusal precedes the event take and every state write, so the poll is untouched"
    );
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        before,
        "NOT ONE cycle commits"
    );

    shorten(&mut state, 0).expect("the window the refusal left behind still takes a place");
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: poll.0 },
        "CR 732.2b/c: the second answer is taken and its namer holds the ending point — so the \
         refusal above was scoped to the place, not to the seat"
    );
}

/// The four-seat board the loop actually runs on, at its offer, with the responder's interrupt
/// seeded: Withering Torment created in their hand from the export's own face, and every
/// permanent they control untapped so a `{2}{B}` instant is castable off their own black
/// sources. The spliced board crosses the production decoder before any row drives it.
///
/// Returns the board, the sole living responder, the seeded instant, and Exquisite Blood — the
/// loop edge the instant can actually cut. The sibling edge is the wrong aim, and the row below
/// measures that rather than asserting it here.
fn tenacity_board_with_seeded_interrupt(
    db: &engine::database::card_db::CardDatabase,
) -> (GameState, PlayerId, ObjectId, ObjectId) {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    )));
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!(
            "the tenacity fixture restores at its offer, got {:?}",
            state.waiting_for
        );
    };
    let responder = state
        .players
        .iter()
        .find(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .expect("one living opponent is seated behind this offer");
    assert!(
        !state.players.iter().any(|p| {
            p.id == responder
                && p.hand.iter().any(|card| {
                    state
                        .objects
                        .get(card)
                        .is_some_and(|o| o.name == "Withering Torment")
                })
        }),
        "reach-guard: the interrupt is SEEDED — the dump's own hand does not already hold it, so \
         the splice below is what makes the cast leg reachable"
    );

    let face = db
        .get_face_by_name("Withering Torment")
        .expect("the seeded interrupt is in the curated fixture database");
    let card_id = CardId(state.next_object_id);
    let torment = create_object(
        &mut state,
        card_id,
        responder,
        face.name.clone(),
        Zone::Hand,
    );
    apply_card_face_to_object(
        state
            .objects
            .get_mut(&torment)
            .expect("the just-created interrupt is in `objects`"),
        face,
    );

    let theirs: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == responder)
        })
        .collect();
    assert!(
        theirs
            .iter()
            .any(|id| state.objects.get(id).is_some_and(|o| o.tapped)),
        "reach-guard: the dump ships this seat's mana tapped, so the untap below is load-bearing"
    );
    for id in &theirs {
        state
            .objects
            .get_mut(id)
            .expect("a battlefield object this seat controls")
            .tapped = false;
    }

    let payload = serde_json::to_value(&state).expect("the spliced board serializes");
    let state: GameState =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
            .expect("it decodes through the production restore")
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract");

    let blood = state
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Exquisite Blood")
        })
        .expect("the loop edge the interrupt can cut is on the battlefield");
    (state, responder, torment, blood)
}

/// CR 732.2b/c end to end, on a real four-seat board: the sole responder names a place above the
/// budget this engine drives and is REFUSED with their window intact; from the state that
/// refusal left behind they name a place the drive reaches, receive the ending point (CR 732.2c),
/// and cast the instant in their hand at it — cutting the loop rather than losing to it.
///
/// The queue is asserted empty first, so the answer reaches consumption in one step and nothing
/// below is attributable to a queued seat. The ending-point leg's discriminator is that the seat
/// holding it is NOT the seat a handback would route to, which a bare `Priority` wait cannot be.
/// The cast is driven through the pipeline rather than asserted around, and the life delta is
/// deliberately not asserted: the loop's pending stack entries resolve alongside the spell.
///
/// Reds under restoring the responder's seam to its old `Ok` path: the first leg's `Err` fails
/// at once, and the window the other two legs run from no longer exists.
#[test]
fn a_place_the_engine_cannot_drive_is_refused_and_the_next_answer_ends_the_loop() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let (mut state, responder, torment, blood) = tenacity_board_with_seeded_interrupt(db);
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!("the seeded board is still at its offer")
    };
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    )
    .expect("this offer publishes no narrowed bound, so an unbounded declaration is legal");

    let poll = window(&state);
    assert_eq!(
        poll,
        (responder, vec![]),
        "reach-guard: two of the three non-proposing seats are out of the game, so this answer \
         reaches consumption in one step with no queue to drain"
    );
    let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();

    let over_budget = ShortcutDecisionSchema::default().max_iterations + 1;
    assert!(
        admitted_places(&state).contains(&over_budget),
        "reach-guard: CR 732.2b admits this place, so the refusal below is the DRIVE's and not \
         the range's"
    );
    let refused = shorten(&mut state, over_budget);
    assert!(
        matches!(refused, Err(EngineError::InvalidAction(_))),
        "the place one above the budget is refused at `apply()`: {refused:?}"
    );
    assert_eq!(
        window(&state),
        poll,
        "the refusal precedes the event take and every state write, so the window survives it"
    );
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        before,
        "a refused answer commits nothing"
    );

    shorten(&mut state, 0).expect("the same responder names a place this engine drives");
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: responder },
        "CR 732.2c: the shortcut is taken to the place they named and they hold its ending point"
    );
    let fallback = state.active_player;
    assert!(
        fallback != responder
            && state
                .players
                .iter()
                .any(|p| p.id == fallback && !p.is_eliminated),
        "the seat a handback would route to is a DIFFERENT living seat, which is what makes the \
         wait above the ending point rather than a handback"
    );

    let mut runner = GameRunner::from_state(state);
    let outcome = runner.cast(torment).target_object(blood).resolve();
    outcome.assert_zone(&[blood], Zone::Graveyard);
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::GameOver { .. }),
        "the responder cut the loop at the ending point they were given, so nobody won: got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "play resumes at a priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::RespondToShortcut { .. } | WaitingFor::LoopShortcut { .. }
        ),
        "the loop is gone, so nothing re-offers it"
    );
}

/// The paired control for the arc's cast leg, one target apart on the same board: aimed at the
/// OTHER loop edge the same spell resolves, the same seat spends the same priority window, and
/// the loop is still there — the engine re-offers it. Enduring Tenacity is an enchantment
/// CREATURE, so the obvious removal destroys it and its own dies-trigger returns it to the
/// battlefield as an enchantment, still carrying the trigger the loop turns on.
///
/// Without this leg the arc row's "nothing re-offers it" is satisfied by the deliberate-action
/// ring clear alone, which any cast would produce. Here the same cast on the same board DOES
/// re-offer, so the arc row's halt is attributable to the edge that was cut.
#[test]
fn the_loop_edge_that_looks_like_the_answer_leaves_the_loop_standing() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let (mut state, _responder, torment, blood) = tenacity_board_with_seeded_interrupt(db);
    let tenacity = state
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.name == "Enduring Tenacity")
        })
        .expect("the loop's creature edge is on the battlefield");
    assert!(
        state
            .objects
            .get(&tenacity)
            .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature)),
        "reach-guard: this edge is a CREATURE going in, which is what makes it the obvious \
         target for creature removal"
    );

    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!("the seeded board is still at its offer")
    };
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        },
    )
    .expect("this offer publishes no narrowed bound");
    shorten(&mut state, 0).expect("the responder names a place this engine drives");

    let mut runner = GameRunner::from_state(state);
    let outcome = runner.cast(torment).target_object(tenacity).resolve();

    assert_eq!(
        outcome.zone_of(tenacity),
        Zone::Battlefield,
        "positive reach-guard: the spell RESOLVED and destroyed it — its own dies-trigger put it \
         back, so an unchanged zone here is the return and not a fizzle"
    );
    assert!(
        !outcome
            .state()
            .objects
            .get(&tenacity)
            .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature)),
        "it came back as an enchantment, which is why creature removal cannot be aimed at it again"
    );
    assert_eq!(
        outcome.zone_of(blood),
        Zone::Battlefield,
        "the edge the arc row cuts is untouched here, so the two rows differ in the aim alone"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { .. } | WaitingFor::RespondToShortcut { .. }
        ),
        "the loop still closes, so the engine offers the shortcut again — got {:?}",
        outcome.final_waiting_for()
    );
}

/// The class boundary on a RESTORED proposal carrying an over-cap `Fixed` count — the ingress the
/// declare firewall never saw. CR 732.2b publishes every place below that count, and the seam
/// refuses exactly the ones the drive cannot take: the boundary member one above the global
/// budget, and a place INSIDE the budget but above the CR 704.5a ceiling this board re-derives.
/// The place that rewrites the count to a drivable one is ADMITTED and drives, which is what
/// separates a guard reading the place a responder named from one reading the proposal's count.
///
/// Reds under keying the guard on `proposal.count` (the admitted leg goes red — the proposal's
/// own count is over-cap, so every place on it would be refused) and under dropping the ceiling
/// disjunct from the predicate (the middle leg goes red).
#[test]
fn a_restored_over_cap_proposal_refuses_what_it_cannot_drive_and_admits_the_repairing_place() {
    let cap = ShortcutDecisionSchema::default().max_iterations;
    // The tamper is `cap + 2`, not `cap + 1`: at `cap + 1` the published range ends at `cap` and
    // the boundary member below would be outside it, so the range refusal would do the refusing.
    let tampered = || {
        let mut state = weird_window(IterationCount::Fixed(weird_offer_bound()));
        let WaitingFor::RespondToShortcut { proposal, .. } = &mut state.waiting_for else {
            panic!("declaring on the drain dump opens a responder window")
        };
        proposal.count = IterationCount::Fixed(cap + 2);
        let payload = serde_json::to_value(&state).expect("the tampered board serializes");
        let state: GameState =
            serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
                .expect("it decodes through the production restore")
                .into_game_state()
                .expect("persisted test snapshot satisfies the checked restore contract");
        let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
            panic!("the restore keeps the responder window")
        };
        assert_eq!(
            proposal.count,
            IterationCount::Fixed(cap + 2),
            "the tampered count survives the decode, so no scrubber can be doing the refusing"
        );
        state
    };

    let places = admitted_places(&tampered());
    assert_eq!(
        places,
        0..=cap + 1,
        "CR 732.2b: the restored proposal publishes every place below its own count"
    );
    // Inside the budget, and above the ceiling this board's per-cycle charge re-derives: the
    // victim seat cannot survive this many repetitions.
    let above_ceiling = 100u32;
    let admitted_place = 9u32;
    assert!(
        places.contains(&(cap + 1)) && cap + 1 > cap,
        "reach-guard: the first place is published AND above the budget, so it is the budget \
         disjunct at its boundary member"
    );
    assert!(
        places.contains(&above_ceiling) && above_ceiling <= cap,
        "reach-guard: the second place is published and INSIDE the budget, so refusing it is the \
         re-derived CR 704.5a ceiling's doing and not the budget's"
    );
    assert!(
        places.contains(&admitted_place) && admitted_place <= cap,
        "reach-guard: the third place is published and inside the budget"
    );

    for place in [cap + 1, above_ceiling] {
        let mut state = tampered();
        let poll = window(&state);
        let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        let refused = shorten(&mut state, place);
        assert!(
            matches!(refused, Err(EngineError::InvalidAction(_))),
            "place {place} is one this engine will not drive: {refused:?}"
        );
        assert_eq!(
            window(&state),
            poll,
            "place {place}: the refusal leaves the poll exactly as it found it"
        );
        assert_eq!(
            state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
            before,
            "place {place}: not one repetition commits"
        );
    }

    // The magnitude of one repetition, driven on this same board rather than transcribed.
    let charge = {
        let mut one = tampered();
        let before: Vec<i32> = one.players.iter().map(|p| p.life).collect();
        shorten(&mut one, 1).expect("one repetition is inside both ceilings");
        respond(&mut one, ShortcutResponse::Accept).expect("the queued seat accepts");
        one.players
            .iter()
            .map(|p| p.life)
            .zip(before)
            .map(|(after, before)| after - before)
            .collect::<Vec<_>>()
    };
    assert!(
        charge.iter().any(|delta| *delta != 0),
        "reach-guard: one repetition of this loop must move a life total, got {charge:?}"
    );

    let mut state = tampered();
    let (shortener, queued) = window(&state);
    let next = *queued
        .first()
        .expect("a second responder is queued on this board");
    let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
    shorten(&mut state, admitted_place)
        .expect("an in-budget, in-ceiling place REPAIRS the restored count and is admitted");
    assert_eq!(
        window(&state).0,
        next,
        "CR 732.2b: the shortened proposal is put to the seat still queued"
    );
    respond(&mut state, ShortcutResponse::Accept).expect("the last seat accepts");
    assert_eq!(
        state
            .players
            .iter()
            .map(|p| p.life)
            .zip(before)
            .map(|(after, before)| after - before)
            .collect::<Vec<_>>(),
        charge
            .iter()
            .map(|delta| delta * admitted_place as i32)
            .collect::<Vec<_>>(),
        "CR 732.2c: the named number of repetitions commit, measured against one of them on this \
         same board"
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: shortener },
        "the drive reached the place they named, so the shortener holds the ending point"
    );
}

/// Where the seam's extraction stops, pinned by TAKING the answer. An `Err` that leaves the
/// window intact is sound only for a condition the responder's own next answer can change; a
/// foreign-owner template is not one, so refusing it there would wedge the seat instead of
/// refusing the answer. The seam therefore answers, the poll drains, and CONSUMPTION refuses on
/// the disjunct the extraction left behind — which is also what proves that guard did not become
/// unreachable.
///
/// The matched control is the same board ONE FIELD apart: with the owner untampered the same
/// place drives and the ending point lands on the shortener. Without it the tampered leg's
/// no-delta observation is satisfied by a fixture that never reached the guard.
///
/// Reds under widening the extraction past the count arm — lift the `template.owner` disjunct to
/// the responder's seam too and the `Ok` assertion fails, the responder refused for a fact no
/// answer of theirs can change.
#[test]
fn a_foreign_owner_template_is_answered_at_the_seam_and_refused_at_consumption() {
    let place = 1u32;
    let board = |tamper: bool| {
        let mut state = weird_window(IterationCount::Fixed(weird_offer_bound()));
        let WaitingFor::RespondToShortcut { proposal, .. } = &state.waiting_for else {
            panic!("declaring on the drain dump opens a responder window")
        };
        let proposer = proposal.proposer;
        assert_eq!(
            proposal.template.as_ref().map(|t| t.owner),
            Some(proposer),
            "reach-guard: a `template: None` declaration resolves to the offer's OWN \
             engine-issued one, so the tamper below is a single field off an honest board"
        );
        if tamper {
            let foreign = state
                .players
                .iter()
                .map(|p| p.id)
                .find(|id| *id != proposer)
                .expect("another seat is at the table");
            let WaitingFor::RespondToShortcut { proposal, .. } = &mut state.waiting_for else {
                unreachable!("the window was just destructured")
            };
            proposal
                .template
                .as_mut()
                .expect("the template just read")
                .owner = foreign;
        }
        let payload = serde_json::to_value(&state).expect("the board serializes");
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
            .expect("it decodes through the production restore")
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract")
    };

    for tamper in [false, true] {
        let mut state: GameState = board(tamper);
        let (shortener, _) = window(&state);
        let fallback = state.active_player;
        let before: Vec<i32> = state.players.iter().map(|p| p.life).collect();

        shorten(&mut state, place).unwrap_or_else(|error| {
            panic!(
                "tampered={tamper}: the seam takes an in-budget place whatever the template \
                 says: {error:?}"
            )
        });
        while matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }) {
            respond(&mut state, ShortcutResponse::Accept).expect("each queued seat accepts");
        }
        let after: Vec<i32> = state.players.iter().map(|p| p.life).collect();

        if tamper {
            assert_eq!(
                after, before,
                "consumption refuses the foreign-owner template, so not one repetition commits"
            );
            assert_eq!(
                state.waiting_for,
                WaitingFor::Priority { player: fallback },
                "the fail-closed handback routes to a living seat, NOT to the shortener \
                 {shortener:?} — no ending point was reached"
            );
            assert_ne!(
                fallback, shortener,
                "reach-guard: the handback seat and the shortener are different seats, so the \
                 assertion above discriminates"
            );
        } else {
            assert_ne!(
                after, before,
                "matched control: one field apart the same place DRIVES, without which the \
                 no-delta observation above is satisfied by a board that never reached the guard"
            );
            assert_eq!(
                state.waiting_for,
                WaitingFor::Priority { player: shortener },
                "the drive reached the named place, so the shortener holds the ending point"
            );
        }
    }
}

/// The member the class must refuse, reached the way the engine reaches one: the handler floors
/// nothing at zero, so a `Fixed(0)` declaration mints a proposal and polls on it. A
/// zero-repetition proposal offers nothing to diverge from, so it admits no place at all — while
/// `Accept` still answers, which is what proves the refusal is scoped to the shortening response
/// and did not swallow the reply path.
#[test]
fn a_zero_count_proposal_admits_no_place_while_accept_still_answers() {
    let mut state = weird_window(IterationCount::Fixed(0));
    let (responder, queued) = window(&state);
    let next = *queued.first().expect("two seats are polled on this board");
    assert!(
        admitted_places(&state).is_empty(),
        "a zero-count proposal admits no place"
    );

    for place in [0u32, 1] {
        let refused = shorten(&mut state, place);
        assert!(
            matches!(refused, Err(EngineError::InvalidAction(_))),
            "place {place} on a zero-count proposal: {refused:?}"
        );
        assert_eq!(
            window(&state),
            (responder, queued.clone()),
            "the window survives the refusal of place {place}"
        );
    }

    respond(&mut state, ShortcutResponse::Accept).expect("Accept answers a zero-count proposal");
    assert_eq!(
        window(&state).0,
        next,
        "CR 732.2b: accepting advances the poll to the queued seat"
    );
    respond(&mut state, ShortcutResponse::Accept).expect("the last seat accepts");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2c: the last acceptance takes the shortcut and lands on a priority point, got {:?}",
        state.waiting_for
    );
}

/// The engine-driven seat at a window whose range is empty. Its answer has to be one the reducer
/// takes: `ai_support::candidates` builds a single candidate here and validates it against the
/// reducer, so a verdict naming a place the proposal admits nowhere is dropped rather than
/// refused on submit, leaving the polled seat nothing to play. The guard is the same rig at a
/// count whose range does hold a place, where the seat still names one — so a seat that Accepted
/// everywhere could not satisfy the row.
#[test]
fn the_engine_driven_seat_can_answer_a_window_whose_range_is_empty() {
    let mut state = rig_window(IterationCount::Fixed(0));
    assert!(
        admitted_places(&state).is_empty(),
        "reach-guard: this window's proposal admits no place"
    );
    let (responder, _) = window(&state);
    let candidates = legal_actions(&state);
    assert_eq!(
        candidates.len(),
        1,
        "the polled seat holds one candidate against an empty range, got {candidates:?}"
    );
    apply(&mut state, responder, candidates[0].clone())
        .expect("the generated candidate must be one the reducer accepts");

    let mut guard = rig_window(IterationCount::Fixed(1));
    assert_eq!(
        admitted_places(&guard),
        0..=0,
        "reach-guard: a one-repetition proposal admits exactly place 0"
    );
    let (guard_responder, _) = window(&guard);
    let guard_candidates = legal_actions(&guard);
    assert_eq!(
        guard_candidates,
        vec![GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 0 }
        }],
        "reach-guard: on a range that holds a place this seat still names one"
    );
    apply(&mut guard, guard_responder, guard_candidates[0].clone())
        .expect("the named place is legal on the proposal that admits it");
}

/// Place 0 is in range on every proposal whose range holds a place at all — `Fixed(n >= 1)` and
/// `UntilLethal` — which is the place every emitter that names a constant one emits. `Fixed(1)`
/// carries the boundary: there 0 is simultaneously the only legal place and the last one. The
/// zero count is the complementary class, and the row above owns it.
#[test]
fn place_zero_is_answered_on_every_proposal_whose_range_holds_a_place() {
    let bounded = IterationCount::Fixed(weird_offer_bound());
    for mut state in [
        weird_window(bounded),
        rig_window(IterationCount::Fixed(1)),
        rig_window(IterationCount::UntilLethal),
    ] {
        let (responder, _) = window(&state);
        let places = admitted_places(&state);
        shorten(&mut state, 0)
            .unwrap_or_else(|error| panic!("place 0 against {places:?}: {error:?}"));
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: responder },
            "CR 732.2c: the shortening responder receives priority"
        );
    }
}

/// The `[min, max]` the interaction layer publishes to the seat it is polling.
fn published_ends(state: &GameState) -> (u32, u32) {
    let (responder, _) = window(state);
    let filtered = filter_state_for_viewer(state, responder);
    derive_viewer_interaction(state, &filtered, responder)
        .opportunities
        .iter()
        .find_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::Schema {
                spec:
                    InteractionResponseSpec::ShortcutReply {
                        min_iteration,
                        max_iteration,
                        ..
                    },
                ..
            } => Some((*min_iteration, *max_iteration)),
            _ => None,
        })
        .expect("the responder window publishes a shortcut-reply opportunity")
}

/// Does the interaction layer let this seat submit this place?
fn interaction_admits(state: &GameState, at_iteration: u32) -> bool {
    let (responder, _) = window(state);
    let filtered = filter_state_for_viewer(state, responder);
    let interaction_id = derive_viewer_interaction(state, &filtered, responder)
        .opportunities
        .iter()
        .find(|opportunity| {
            matches!(
                &opportunity.response,
                InteractionOpportunityResponse::Schema {
                    spec: InteractionResponseSpec::ShortcutReply { .. },
                    ..
                }
            )
        })
        .map(|opportunity| opportunity.interaction_id.clone())
        .expect("the responder window publishes a shortcut-reply opportunity");
    resolve_interaction_response(
        state,
        responder,
        &InteractionSubmission {
            interaction_id,
            response: InteractionResponse::ShortcutReply {
                reply: InteractionShortcutReply::Shorten { at_iteration },
            },
        },
    )
    .is_ok()
}

/// The line between a range the responder is SHOWN and a range `apply()` honors is drivability:
/// the projection publishes the proposal's own two ends on every shape, and the reducer refuses
/// every place this engine will not drive. Being above the budget is one way to be undrivable —
/// a board whose per-cycle charge re-derives a lower CR 704.5a ceiling refuses places well
/// inside it — and it is the only one the places probed here meet, so this row's boards part at
/// the budget. Reds if either layer derives its ends a second time — the empty range is the leg
/// that catches it, since a second derivation of it publishes `[0, 0]` and admits a place the
/// reducer refuses.
///
/// ABOVE the budget the two layers deliberately DISAGREE: the projection still admits, and the
/// reducer alone refuses. That is how a responder is TOLD rather than pre-refused by their own
/// client, which would be a second derivation of the same bound in a layer that cannot see the
/// board. CR 732.2b's published range keeps both its ends; the budget is the engine's and is not
/// published as one.
///
/// The at-or-below partition keeps its own counting reach-guard — a dead projection and a
/// blanket refusal both fail it — and the above-budget partition is asserted non-empty, so
/// routing the submission gate through the same predicate would empty it and fire.
#[test]
fn the_published_range_and_the_reducer_agree_below_the_budget_and_part_above_it() {
    let bound = weird_offer_bound();
    let cap = ShortcutDecisionSchema::default().max_iterations;
    let mut admitted = 0usize;
    let mut refused = 0usize;
    let mut above_budget = 0usize;

    for mut state in [
        weird_window(IterationCount::Fixed(0)),
        weird_window(IterationCount::Fixed(bound)),
        rig_window(IterationCount::Fixed(1)),
        rig_window(IterationCount::UntilLethal),
    ] {
        bind_interaction_authority(&mut state, InteractionSessionId("shortening".to_string()))
            .expect("the responder window binds interaction authority");
        let places = admitted_places(&state);
        assert_eq!(
            published_ends(&state),
            (*places.start(), *places.end()),
            "the projection publishes the proposal's own ends for {places:?}"
        );

        let probes = [
            places.start().saturating_sub(1),
            *places.start(),
            places.end().saturating_sub(1),
            *places.end(),
            places.end().saturating_add(1),
        ];
        for place in probes {
            let mut board = state.clone();
            let honored = shorten(&mut board, place).is_ok();
            let published = interaction_admits(&state, place);
            if place <= cap {
                assert_eq!(
                    published, honored,
                    "the two layers disagree on place {place} against {places:?}"
                );
                if honored {
                    admitted += 1;
                } else {
                    refused += 1;
                }
            } else {
                assert!(
                    published,
                    "place {place} is inside the proposal's own range {places:?}, so the \
                     projection publishes it rather than pre-refusing on a budget it cannot see"
                );
                assert!(
                    !honored,
                    "place {place} is above the budget this engine drives, so the reducer alone \
                     refuses it — which is how the responder is told"
                );
                above_budget += 1;
            }
        }
    }

    assert!(
        admitted > 0 && refused > 0,
        "reach-guard on the agreeing partition itself: {admitted} admitted and {refused} refused \
         at or below the budget, so neither a dead projection nor a blanket refusal can satisfy \
         the agreement"
    );
    assert!(
        above_budget > 0,
        "reach-guard on the parting partition: no probed place landed above the budget, so the \
         disagreement above was asserted over nothing"
    );
}

/// One proposal, two responders queued behind it: the refusal binds to the proposal, not to
/// whoever is answering, so it must leave the poll untouched — against an `Accept` on the same
/// board, which does advance it.
#[test]
fn a_refusal_does_not_advance_the_poll() {
    let count = weird_offer_bound();
    let mut refusal_board = weird_window(IterationCount::Fixed(count));
    let before = window(&refusal_board);
    assert!(
        !before.1.is_empty(),
        "reach-guard: this board queues a second responder behind the one being polled"
    );

    assert!(
        shorten(&mut refusal_board, count).is_err(),
        "place {count} is outside a {count}-repetition proposal"
    );
    assert_eq!(
        window(&refusal_board),
        before,
        "the refused response advanced neither the acting responder nor the queue"
    );

    let mut accept_board = weird_window(IterationCount::Fixed(count));
    respond(&mut accept_board, ShortcutResponse::Accept).expect("Accept answers");
    assert_ne!(
        window(&accept_board).0,
        before.0,
        "positive control: an answered response does advance the poll on this same board"
    );
}

/// Every seat's board state, keyed by seat — the axes a drain period can move, plus the object
/// identities a materialization would mint. Read off the state rather than transcribed, so a row
/// comparing two runs compares what the engine produced on both.
fn board_by_seat(state: &GameState) -> Vec<(PlayerId, i32, usize, usize, usize, Vec<ObjectId>)> {
    state
        .players
        .iter()
        .map(|p| {
            let controlled: Vec<ObjectId> = state
                .battlefield
                .iter()
                .filter(|id| state.objects.get(id).is_some_and(|o| o.controller == p.id))
                .copied()
                .collect();
            (
                p.id,
                p.life,
                p.hand.len(),
                p.library.len(),
                p.graveyard.len(),
                controlled,
            )
        })
        .collect()
}

/// Submit the response the window is polling for and keep the events it produced.
fn respond_with_events(
    state: &mut GameState,
    response: ShortcutResponse,
) -> Vec<engine::types::events::GameEvent> {
    let (player, _) = window(state);
    apply(state, player, GameAction::RespondToShortcut { response })
        .expect("the response is one the reducer takes")
        .events
}

/// Drain the rest of the poll with `Accept`, returning the events of the call that took the
/// shortcut — the last one, which is the only one that can materialize anything.
fn accept_out(state: &mut GameState) -> Vec<engine::types::events::GameEvent> {
    let mut last = Vec::new();
    while matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }) {
        last = respond_with_events(state, ShortcutResponse::Accept);
    }
    last
}

/// L3 PREFIX CONSENT under partial materialization (CR 732.2b/c): declaring a count and
/// shortening it to `k` reaches the board — and the event sequence — that declaring `k` outright
/// reaches. The seam is the count rewrite plus the drive it hands an unchanged materializer.
///
/// Reds if the taking materializes the ORIGINAL count or one cycle too many, and reds on the
/// event leg if `k` copies of the per-cycle delta are ever batched into one lump instead of
/// driven. The reach-guard is that both runs MOVED from the offer board: without it two no-ops
/// compare equal. `waiting_for` is deliberately not compared — the two runs end at different
/// seats by the rule the ending-point rows own, and that is not this row's claim.
#[test]
fn shortening_to_a_place_reaches_the_board_declaring_it_outright_reaches() {
    let bound = weird_offer_bound();
    for k in [1u32, bound - 1] {
        let at_offer = board_by_seat(&weird_window(IterationCount::Fixed(k)));

        let mut declared = weird_window(IterationCount::Fixed(k));
        let declared_events = accept_out(&mut declared);

        let mut shortened = weird_window(IterationCount::Fixed(bound));
        let mut shortened_events = vec![respond_with_events(
            &mut shortened,
            ShortcutResponse::Shorten { at_iteration: k },
        )];
        shortened_events.push(accept_out(&mut shortened));
        let shortened_events: Vec<_> = shortened_events.into_iter().flatten().collect();

        assert_ne!(
            board_by_seat(&declared),
            at_offer,
            "reach-guard at k={k}: the declared-{k} run must have MOVED from the offer board, or \
             two no-ops compare equal"
        );
        assert_eq!(
            board_by_seat(&shortened),
            board_by_seat(&declared),
            "CR 732.2c at k={k}: shortening to {k} advances to the same place declaring {k} does"
        );
        assert_eq!(
            format!("{shortened_events:?}"),
            format!("{declared_events:?}"),
            "CR 732.2c at k={k}: the same choices were taken, so the same events were produced — \
             a batched lump would collapse {k} periods into one"
        );
    }
}

/// L5 THIRD-PARTY INVARIANCE under partial materialization (CR 732.2b/c): two shortening lengths
/// leave the seat this period neither charges nor credits identical on every axis, while the
/// proposer's and the victim's lives differ by exactly the length difference times the period's
/// own charge — read off a one-cycle run of this same board rather than transcribed.
///
/// Reds if the drain is driven at the priority seat instead of the pinned one, or if the pin's
/// per-iteration re-resolution is skipped. The two runs are required to DIFFER on the proposer
/// and the victim, or the invariance is satisfied by a board that never moved.
#[test]
fn a_bystander_seat_is_untouched_by_the_length_a_responder_names() {
    let bound = weird_offer_bound();
    let (j, k) = (1u32, 4u32);

    let charge = {
        let mut one = weird_window(IterationCount::Fixed(bound));
        let before = board_by_seat(&one);
        respond_with_events(&mut one, ShortcutResponse::Shorten { at_iteration: 1 });
        accept_out(&mut one);
        let after = board_by_seat(&one);
        before
            .iter()
            .zip(after.iter())
            .map(|(b, a)| (b.0, a.1 - b.1))
            .collect::<Vec<_>>()
    };
    assert!(
        charge.iter().any(|(_, delta)| *delta != 0),
        "reach-guard: one cycle of this loop must move SOME seat's life, got {charge:?}"
    );

    let run = |place: u32| {
        let mut state = weird_window(IterationCount::Fixed(bound));
        respond_with_events(
            &mut state,
            ShortcutResponse::Shorten {
                at_iteration: place,
            },
        );
        accept_out(&mut state);
        board_by_seat(&state)
    };
    let short = run(j);
    let long = run(k);

    for ((seat, per_cycle), (short_row, long_row)) in
        charge.iter().zip(short.iter().zip(long.iter()))
    {
        assert_eq!((*seat, short_row.0, long_row.0), (*seat, *seat, *seat));
        if *per_cycle == 0 {
            assert_eq!(
                short_row, long_row,
                "seat {seat:?} is neither charged nor credited by this period, so the length the \
                 responder named must not reach it on ANY axis"
            );
        } else {
            assert_eq!(
                long_row.1 - short_row.1,
                per_cycle * (k as i32 - j as i32),
                "seat {seat:?} moves by exactly the extra cycles times this loop's own charge"
            );
        }
    }
    assert!(
        charge.iter().filter(|(_, delta)| *delta != 0).count() >= 2,
        "reach-guard: the two runs must differ on the proposer AND the victim, or the invariance \
         above is asserted over a board that never moved; charge={charge:?}"
    );
}

/// CR 732.2c: a place of ZERO rewrites the proposal to one that admits no place, so the shortcut
/// is taken at once — nothing is performed, nobody else is polled, and the shortener holds the
/// ending point. Its pair is the same board at a NON-ZERO place, where the poll advances to the
/// next living opponent instead. Neither leg ships alone: the non-zero leg is what reds at base,
/// where the same response moves no board and hands its responder a window.
///
/// The non-empty queue assertion is the zero leg's guard — without it the leg passes on a queue
/// that had nowhere to advance. The zero leg reds under its own restoration: let the zero case
/// fall through to the poll advance and the queued opponent is prompted.
#[test]
fn a_zero_place_takes_the_empty_shortcut_while_a_named_place_polls_on() {
    let bound = weird_offer_bound();

    let mut zero = weird_window(IterationCount::Fixed(bound));
    let (shortener, queued) = window(&zero);
    assert!(
        !queued.is_empty(),
        "reach-guard: a seat is still queued, so eliding the poll is a real decision"
    );
    let before = board_by_seat(&zero);
    respond_with_events(&mut zero, ShortcutResponse::Shorten { at_iteration: 0 });
    assert_eq!(
        board_by_seat(&zero),
        before,
        "CR 732.2c: arriving at the zero place means performing no iteration"
    );
    assert_eq!(
        zero.waiting_for,
        WaitingFor::Priority { player: shortener },
        "the empty shortcut is taken and its ending point is the shortener's, never a further \
         responder window"
    );

    let mut named = weird_window(IterationCount::Fixed(bound));
    let (named_shortener, named_queued) = window(&named);
    let next = *named_queued.first().expect("a second responder is queued");
    respond_with_events(&mut named, ShortcutResponse::Shorten { at_iteration: 2 });
    assert_eq!(
        window(&named).0,
        next,
        "CR 732.2b: a named place advances the poll to the next living opponent"
    );
    accept_out(&mut named);
    assert_ne!(
        board_by_seat(&named),
        before,
        "the named place drives the cycles the zero place did not — which is what makes the zero \
         leg's unchanged board a statement about the count and not about the fixture"
    );
    assert_eq!(
        named.waiting_for,
        WaitingFor::Priority {
            player: named_shortener
        },
        "once the poll completes, the shortener holds the ending point"
    );
}

/// The colorless mana floating in `player`'s pool.
fn colorless(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| {
            p.mana_pool
                .count_color(engine::types::mana::ManaType::Colorless)
        })
        .unwrap_or(0)
}

/// The mana engine at its offer, plus the seat that proposed it.
fn mana_engine_window(db: &engine::database::card_db::CardDatabase, count: u32) -> GameState {
    use crate::loop_shortcut_mana_engine as rig;
    let mut engine_rig = rig::setup(true, LoopDetectionMode::Interactive, db);
    let mana = rig::mana_ability_index(engine_rig.runner.state(), engine_rig.basalt)
        .expect("Basalt publishes its mana ability");
    let untap = rig::untap_ability_index(engine_rig.runner.state(), engine_rig.basalt)
        .expect("Basalt publishes its untap ability");
    rig::drive_one_period(&mut engine_rig, mana, untap);
    assert!(
        matches!(
            engine_rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "reach-guard: the mana engine must OFFER before a response can be tested, got {:?}",
        engine_rig.runner.state().waiting_for
    );
    engine_rig
        .runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        })
        .expect("the proposer declares");
    engine_rig.runner.state().clone()
}

/// The mana engine's own per-period charge, driven on a detector-`Off` rig of the same shape so
/// no row transcribes it and no offer interferes with the measurement.
fn mana_period_charge(db: &engine::database::card_db::CardDatabase) -> usize {
    use crate::loop_shortcut_mana_engine as rig;
    let mut off = rig::setup(true, LoopDetectionMode::Off, db);
    let mana = rig::mana_ability_index(off.runner.state(), off.basalt).expect("mana ability");
    let untap = rig::untap_ability_index(off.runner.state(), off.basalt).expect("untap ability");
    let before = colorless(off.runner.state(), P0);
    rig::drive_one_period(&mut off, mana, untap);
    let charge = colorless(off.runner.state(), P0) - before;
    assert!(
        charge > 0,
        "reach-guard: one period of this engine must add mana, or every leg below is vacuous"
    );
    charge
}

/// CR 732.2c on the object-growth subclass that REGISTERS NOTHING: a shortened proposal is
/// PERFORMED, not elided. The elision's licence is the unbounded advance the table accepted; a
/// mana engine writes no collapse bound at all, so eliding a shortening here would hand the
/// responder the very advance they declined, with neither ceiling nor seat.
///
/// The Accept control is this row's live instrument: without it both shortening legs would pass
/// on a rig that marks nothing and grows nothing. The unbounded mark is the discriminator
/// because it is that materializer's unconditional first act. The 2k leg is the COUNT
/// discriminator — it separates performing the named number of periods from performing one and
/// from handing back. Both shortening legs red at base, where the response reaches no
/// materializer and the pool does not move.
#[test]
fn a_shortened_mana_engine_is_performed_while_an_accepted_one_is_marked_unbounded() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let charge = mana_period_charge(db);
    let k = 1usize;

    // Control: the same proposal ACCEPTED still takes the elision.
    let mut accepted = mana_engine_window(db, 5);
    let accepted_before = colorless(&accepted, P0);
    respond(&mut accepted, ShortcutResponse::Accept).expect("the opponent accepts");
    assert!(
        accepted
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes
                .iter()
                .any(|axis| matches!(axis, engine::analysis::resource::ResourceAxis::Mana(_)))),
        "control: an accepted mana engine marks an unbounded Mana axis"
    );
    assert!(
        colorless(&accepted, P0) - accepted_before > charge * 8,
        "control: the accepted advance tops the pool to the infinite-mana constant, not to a \
         finite multiple of the period charge"
    );

    for periods in [k, 2 * k] {
        let mut state = mana_engine_window(db, 5);
        let (shortener, _) = window(&state);
        let before = colorless(&state, P0);
        shorten(&mut state, periods as u32).expect("the named place is in range");

        assert!(
            state.unbounded_resources.get(&P0).is_none_or(|axes| !axes
                .iter()
                .any(|axis| matches!(axis, engine::analysis::resource::ResourceAxis::Mana(_)))),
            "a shortened advance is finite, so NO unbounded Mana axis may be marked at {periods} \
             periods"
        );
        assert_eq!(
            colorless(&state, P0) - before,
            charge * periods,
            "CR 732.2c: {periods} periods are PERFORMED, so the pool grows by exactly that many \
             times this loop's own charge"
        );
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: shortener },
            "the drive reached the place the responder named, so they hold the ending point"
        );
    }
}

/// CR 732.2c + CR 704.5a: an in-cap place on an unbounded proposal drives that many cycles and
/// crowns nobody it was not promised. Below the crossing the cycles commit, no game ends and the
/// ending point lands on the shortener; at or past it the game ends with the seat the OFFER
/// named, and the unbounded axes stay unmarked — a finite drive is not the unbounded one.
///
/// The admitted member rides the same board: a restored proposal whose `predicted_winner` the
/// drive contradicts is NOT crowned, however far past the crossing it is shortened to. It reds
/// under its own restoration — clear the name at the rewrite and the drive's winner is crowned
/// through an arm left with nothing to gate on.
///
/// Both legs red at base, where the response reaches no materializer at all. The crown leg's
/// reach-guard is the crown itself, without which the no-crown leg passes on a drive that never
/// reached lethal.
#[test]
fn an_in_cap_place_drives_its_cycles_and_crowns_only_the_named_seat() {
    let below = 5u32;
    let mut short = rig_window(IterationCount::UntilLethal);
    let (shortener, _) = window(&short);
    let victim_before = short
        .players
        .iter()
        .map(|p| (p.id, p.life))
        .collect::<Vec<_>>();

    let per_cycle = {
        let mut one = rig_window(IterationCount::UntilLethal);
        shorten(&mut one, 1).expect("one cycle is in range");
        one.players
            .iter()
            .zip(victim_before.iter())
            .map(|(p, (_, before))| p.life - before)
            .collect::<Vec<_>>()
    };
    assert!(
        per_cycle.iter().any(|delta| *delta != 0),
        "reach-guard: one cycle of this loop must move a life total, got {per_cycle:?}"
    );

    shorten(&mut short, below).expect("an in-cap place is in range");
    assert_eq!(
        short
            .players
            .iter()
            .zip(victim_before.iter())
            .map(|(p, (_, before))| p.life - before)
            .collect::<Vec<_>>(),
        per_cycle
            .iter()
            .map(|delta| delta * below as i32)
            .collect::<Vec<_>>(),
        "CR 732.2c: the named number of cycles commit, measured against one cycle of this board"
    );
    assert_eq!(
        short.waiting_for,
        WaitingFor::Priority { player: shortener },
        "below the crossing no game ends and the shortener holds the ending point"
    );

    let crossing = victim_before
        .iter()
        .map(|(_, life)| *life)
        .max()
        .expect("the rig has players") as u32
        + 1;
    let mut crowned = rig_window(IterationCount::UntilLethal);
    let WaitingFor::RespondToShortcut { proposal, .. } = &crowned.waiting_for else {
        panic!("the rig parks on a responder window")
    };
    let named = proposal
        .predicted_winner
        .expect("this offer's mint is the one that names a winner");
    shorten(&mut crowned, crossing).expect("a place past the crossing is still in range");
    assert_eq!(
        crowned.waiting_for,
        WaitingFor::GameOver {
            winner: Some(named)
        },
        "CR 704.5a: past the crossing the game ends with the seat the offer named"
    );
    assert!(
        crowned.unbounded_resources.is_empty(),
        "a finite drive is not the unbounded one, so no axis is marked"
    );

    // The admitted member: the same shortening on a proposal whose name the drive contradicts.
    let mut contradicted = rig_window(IterationCount::UntilLethal);
    let WaitingFor::RespondToShortcut { proposal, .. } = &mut contradicted.waiting_for else {
        panic!("the rig parks on a responder window")
    };
    proposal.predicted_winner = Some(
        contradicted
            .players
            .iter()
            .map(|p| p.id)
            .find(|id| *id != named)
            .expect("the rig seats an opponent"),
    );
    let payload = serde_json::to_value(&contradicted).expect("the tampered board serializes");
    let mut restored: GameState =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
            .expect("it decodes through the production restore")
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract");
    shorten(&mut restored, crossing).expect("the same place is still in range");
    assert!(
        !matches!(restored.waiting_for, WaitingFor::GameOver { .. }),
        "a drive whose CR 704.5a verdict the proposal's own name contradicts crowns nobody, got \
         {:?}",
        restored.waiting_for
    );
}

/// CR 732.2b: successive shortenings are MONOTONE, and the ending point belongs to the LAST
/// place named. After the first responder shortens to `k`, the second is polled on the live
/// count — its published range narrows to it and `apply()` refuses a place at or past `k` —
/// and shortening again to `j < k` lands the ending point on the SECOND responder.
///
/// Without the paired sibling (the second seat ACCEPTS instead, where the point lands on the
/// first) a handler that seated the first shortener unconditionally would pass. `k - 1` is
/// asserted still legal on the same window, so the refusal is a boundary and not a blanket one.
#[test]
fn the_second_shortener_narrows_the_range_and_holds_the_ending_point() {
    let bound = weird_offer_bound();
    let (k, j) = (4u32, 2u32);

    let mut state = weird_window(IterationCount::Fixed(bound));
    let (first, queued) = window(&state);
    let second = *queued.first().expect("a second responder is queued");
    shorten(&mut state, k).expect("the first responder shortens");

    assert_eq!(
        window(&state).0,
        second,
        "the poll advances to the second responder"
    );
    assert_eq!(
        admitted_places(&state),
        0..=k - 1,
        "CR 732.2b: the second seat's range is derived from the LIVE count, not the declared one"
    );
    let mut refusal_board = state.clone();
    assert!(
        shorten(&mut refusal_board, k).is_err(),
        "a place at the live count names no further divergence and is refused"
    );
    let mut boundary_board = state.clone();
    assert!(
        shorten(&mut boundary_board, k - 1).is_ok(),
        "paired positive: the place one below the live count is still accepted"
    );

    let mut accepting = state.clone();
    respond(&mut accepting, ShortcutResponse::Accept).expect("the second seat accepts instead");
    assert_eq!(
        accepting.waiting_for,
        WaitingFor::Priority { player: first },
        "an Accept names no place, so the FIRST shortener keeps the ending point"
    );

    shorten(&mut state, j).expect("the second responder shortens again");
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: second },
        "CR 732.2b: the last named place is the new ending point, and its namer holds it"
    );
    assert_ne!(
        board_by_seat(&state),
        board_by_seat(&accepting),
        "the two runs took different lengths, so the shorter one is not the same board"
    );
}
