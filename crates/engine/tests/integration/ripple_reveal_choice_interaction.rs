//! Regression: the Ripple "you may reveal the top N" prompt panicked the
//! interaction-opportunity builder.
//!
//! CR 702.60a splits Ripple into two decisions with different response shapes:
//!
//!   * `WaitingFor::RippleRevealChoice` — the binary "you **may** reveal"
//!     offer, answered with `GameAction::RippleChoice { Cast | Decline }`. It
//!     is the same response shape as the `WaitingFor::CastOffer` free-cast
//!     decision that follows it, and it selects no cards at all.
//!   * `WaitingFor::RippleBottomOrder` — the "in any order" permutation of the
//!     uncast revealed pile, answered with `GameAction::SelectCards`. That one
//!     really is a selection.
//!
//! `human_response_model` classified BOTH as `HumanResponseModel::Select`, but
//! only `RippleBottomOrder` has a `selection_projection` arm. Building any
//! viewer's interaction while the reveal offer was open therefore reached
//! `unreachable!("selection model requires selection projection")` — a panic
//! that aborts the WASM engine, so the user saw a bare "unreachable" and every
//! subsequent action failed the same way.
//!
//! The gate is `derive_viewer_interaction` completing for both seats at each
//! Ripple prompt, and the reveal offer presenting as a two-way offer rather
//! than a card selection.

use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Give the controller a library deep enough for a Ripple 4 reveal.
fn seed_library(runner: &mut GameRunner, count: usize) {
    let state = runner.state_mut();
    for i in 0..count {
        let card_id = CardId(state.next_object_id);
        engine::game::zones::create_object(
            state,
            card_id,
            P0,
            format!("Library Card {i}"),
            Zone::Library,
        );
    }
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Return the response the prompt's controller (P0) is offered, and pin that
/// the non-owner seat is gated out before any opportunity is built.
///
/// Only the owner reaches `opportunity_for_slot`: `derive_viewer_interaction`
/// returns early with no opportunities once `can_submit` is false. That is why
/// the crash still reached users through the AI seat's `getAiActionProposal` —
/// `ai_semantic_owner` resolves an AI seat that is not an acting player to the
/// live prompt's owner, so the AI built the OWNER's view, not its own.
fn response_for_controller(state: &mut GameState) -> InteractionOpportunityResponse {
    bind_interaction_authority(state, InteractionSessionId("test-session".to_string()))
        .expect("interaction authority binds");
    let state = &*state;
    assert!(
        derive_viewer_interaction(state, state, P1)
            .opportunities
            .is_empty(),
        "the non-owner seat is gated out before an opportunity is built"
    );
    derive_viewer_interaction(state, state, P0)
        .opportunities
        .into_iter()
        .next()
        .expect("the prompt's controller is offered an interaction")
        .response
}

/// CR 702.60a: drive a Ripple 4 spell to its reveal offer and assert the
/// interaction builder survives both Ripple prompts for both seats.
#[test]
fn ripple_prompts_derive_interaction_for_every_seat() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let spell = scenario.add_real_card(P0, "Surging Flame", Zone::Hand, db);
    let hit = scenario.add_real_card(P0, "Surging Flame", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    seed_library(&mut runner, 4);
    // CR 702.60a: put the same-named card inside the revealed top four so the
    // reveal offer is followed by a free-cast offer and a bottom-order prompt —
    // all three Ripple prompts on one run.
    {
        let state = runner.state_mut();
        let player = state.players.iter_mut().find(|p| p.id == P0).unwrap();
        player.library.retain(|id| *id != hit);
        player.library.push_front(hit);
    }
    add_mana(&mut runner, &[ManaType::Red, ManaType::Red, ManaType::Red]);

    let mut commit = runner.cast(spell).target_player(P1).commit();

    // CR 603.3 + CR 608.1: the Ripple trigger became the topmost object on the
    // stack, so the first pair of passes resolves it and opens the reveal.
    commit.act(GameAction::PassPriority).expect("P0 passes");
    commit.act(GameAction::PassPriority).expect("P1 passes");

    assert!(
        matches!(
            commit.state_mut().waiting_for,
            WaitingFor::RippleRevealChoice { .. }
        ),
        "the resolved Ripple trigger must open the optional-reveal prompt, got {:?}",
        commit.state_mut().waiting_for
    );
    // CR 702.60a: the reveal offer is a finite two-way decision, so it must
    // present as exact choices (reveal / decline) rather than a card selection.
    let reveal = response_for_controller(commit.state_mut());
    match &reveal {
        InteractionOpportunityResponse::ExactChoices { choices } => assert_eq!(
            choices.len(),
            2,
            "the reveal offer is exactly reveal-or-decline, got {choices:?}"
        ),
        other => panic!("reveal offer must present as exact choices, got {other:?}"),
    }

    // CR 702.60a: accepting the reveal opens the free-cast offer for the
    // same-named card, which shares the reveal prompt's response shape.
    commit
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("reveal accepted");
    assert!(
        matches!(commit.state_mut().waiting_for, WaitingFor::CastOffer { .. }),
        "revealing a same-named card must open the free-cast offer, got {:?}",
        commit.state_mut().waiting_for
    );
    // CR 702.60a: the free-cast offer shares the reveal offer's shape — this is
    // the sibling the reveal decision was mistakenly separated from.
    let offer = response_for_controller(commit.state_mut());
    assert!(
        matches!(offer, InteractionOpportunityResponse::ExactChoices { .. }),
        "the Ripple free-cast offer must present as exact choices, got {offer:?}"
    );

    // CR 702.60a + CR 608.2d: declining the free cast bottoms the whole pile,
    // which is the genuine selection prompt of the three.
    commit
        .act(GameAction::RippleChoice {
            choice: CastChoice::Decline,
        })
        .expect("free cast declined");
    assert!(
        matches!(
            commit.state_mut().waiting_for,
            WaitingFor::RippleBottomOrder { .. }
        ),
        "declining the free cast must open the bottom-order prompt, got {:?}",
        commit.state_mut().waiting_for
    );
    // CR 608.2d: the bottom-placement order genuinely IS a selection — the one
    // Ripple prompt that belongs to the `Select` response model.
    let order = response_for_controller(commit.state_mut());
    assert!(
        matches!(
            order,
            InteractionOpportunityResponse::Schema {
                spec: InteractionResponseSpec::Select { .. },
                ..
            }
        ),
        "the bottom-order prompt must present as a selection schema, got {order:?}"
    );
}
