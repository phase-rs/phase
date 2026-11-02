//! CR 732.2a: the object-growth cover, asked through the producer's own certification on three
//! frames each blink board reaches through `apply()`, certifies both boards'
//! trigger-driven periods on every condition, beside a committed board whose offer it certifies.

use engine::analysis::resource::ObjectGrowthVerdict;
use engine::game::engine::certify_object_growth_frames_for_tests as certify;
use engine::game::scenario::{GameRunner, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

use crate::loop_period_accessor_answers::{board_a_frames, board_b_frames, Frames};

fn certified_on_every_condition() -> ObjectGrowthVerdict {
    ObjectGrowthVerdict::FodderGrowth([Vec::new(), Vec::new()])
}

/// The objects cast by the plays of the span the window's trace offers, or else last named.
fn span_casts(state: &engine::types::game_state::GameState) -> Vec<ObjectId> {
    use engine::game::{EntryKind, PlayLocus};
    let Some(view) = engine::game::play_trace_view(state) else {
        return Vec::new();
    };
    let Some(span) = view.offered.or_else(|| view.named.last().copied()) else {
        return Vec::new();
    };
    view.entries[span.start..span.end]
        .iter()
        .filter_map(|entry| match entry.kind {
            EntryKind::Play {
                locus: PlayLocus::Cast(id),
                ..
            } => Some(id),
            _ => None,
        })
        .collect()
}

fn board_verdict(frames: &Frames) -> ObjectGrowthVerdict {
    let [first, second, third] = &frames.cover;
    certify([first, second, third], &span_casts(&frames.read[0]), P0)
}

/// The Sprout Swarm dump's offer, declined on each of four casts: the frames after each decline,
/// with the casts the first offer's span made, since declining clears the trace.
fn sprout_control_verdicts() -> Vec<ObjectGrowthVerdict> {
    let mut state = crate::sprout_inalla_realistic_offer::load_realistic_dump();
    let mut frames = Vec::new();
    let mut casts = Vec::new();
    for fodder in [406, 407, 408, 409] {
        let outcome = GameRunner::from_state(state)
            .cast(ObjectId(405))
            .accept_optional()
            .convoke_with(&[ObjectId(fodder)])
            .commit()
            .resolve();
        let mut runner = GameRunner::from_state(outcome.state().clone());
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
            "reach guard: the control's cast with fodder {fodder} raises the object-growth offer"
        );
        if casts.is_empty() {
            casts = span_casts(runner.state());
        }
        runner
            .act(GameAction::DeclineShortcut)
            .expect("the offer is declinable");
        state = runner.state().clone();
        frames.push(state.clone());
    }
    frames
        .windows(3)
        .map(|window| certify([&window[0], &window[1], &window[2]], &casts, P0))
        .collect()
}

#[test]
fn both_blink_boards_certify_on_every_cover_condition() {
    for verdict in sprout_control_verdicts() {
        assert_eq!(
            verdict,
            certified_on_every_condition(),
            "live control: a board whose offer the producer raises certifies through the same \
             hand-off"
        );
    }
    let (Some(board_a), Some(board_b)) = (board_a_frames(), board_b_frames()) else {
        return;
    };
    assert_eq!(
        (board_verdict(&board_a), board_verdict(&board_b)),
        (
            certified_on_every_condition(),
            certified_on_every_condition()
        ),
        "boards A and B: no cover condition refuses either recorded trigger-driven period"
    );
}
