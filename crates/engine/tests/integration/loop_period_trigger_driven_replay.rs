//! CR 603.3 + CR 732.2a — which answers a trigger-driven period the trace offers carries from the
//! cycle that performed it.
//!
//! Two boards, because their prompt sets are disjoint apart from the ordering prompt: Board A
//! (Abdel Adrian + Animate Dead + Altar of the Brood, built by
//! `abdel_adrian_animate_dead_altar_board` and driven here, never rebuilt) raises `OrderTriggers`
//! and `EffectZoneChoice`; Board B (Preston, the Vanisher + Felidar Guardian + Animate Dead +
//! Altar, built by `loop_period_trigger_driven_arming` and driven here) raises `OrderTriggers`,
//! `TriggerTargetSelection` and `OptionalEffectChoice`.
//!
//! Every board is driven only through `game::engine::apply()`.
//!
//! **Each board's declaration of the step at which the drive declines the loop's voluntary
//! choice.** Board A's cycle is voluntary at Abdel Adrian's exile ("any number", and zero is a
//! number): the row answers that choice in full while accepts remain and with an empty selection
//! after. Board B's cycle is voluntary at Felidar Guardian's "you may exile": the row accepts
//! while accepts remain and declines at the first choice of that kind after them.

use engine::game::scenario::{GameRunner, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::loop_period_trigger_driven_arming::{build_board_b, PrestonBoard};

/// Beat cap for every live drive here. Read by no assertion; it bounds a runaway drive.
const BEAT_CAP: usize = 320;

/// How many of the board's voluntary choices each row accepts before it declines.
const ACCEPTS: usize = 6;

/// The answer classes of the period the trace offers, by the answering action's variant.
struct Performed {
    classes: Vec<&'static str>,
}

/// Drive one board through `apply()` under its own declared answering policy to its first offer,
/// and read the answers of the span that offer names.
///
/// `answer` is the board's policy: it sees the live state and returns the action this row's
/// declaration says to take, or `None` to fall through to the first non-pass legal action.
fn perform(
    runner: &mut GameRunner,
    mut answer: impl FnMut(&GameState) -> Option<GameAction>,
) -> Performed {
    for _ in 0..BEAT_CAP {
        let state = runner.state();
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            let view = engine::game::play_trace_view(state).expect("an offer stands on a trace");
            let span = view.offered.expect("the offer names its span");
            let mut classes: Vec<&'static str> = view.entries[span.start..span.end]
                .iter()
                .filter_map(|entry| match &entry.kind {
                    engine::game::EntryKind::Answer { action, .. }
                        if !matches!(action, GameAction::PassPriority) =>
                    {
                        Some(action.variant_name())
                    }
                    _ => None,
                })
                .collect();
            classes.sort_unstable();
            classes.dedup();
            return Performed { classes };
        }
        let action = match answer(state) {
            Some(action) => action,
            None => engine::ai_support::legal_actions(state)
                .into_iter()
                .find(|action| !matches!(action, GameAction::PassPriority))
                .unwrap_or(GameAction::PassPriority),
        };
        if runner.act(action).is_err() {
            break;
        }
    }
    panic!("the drive never reached an offer, so the row would pass vacuously");
}

// ---------------------------------------------------------------------------
// Board A
// ---------------------------------------------------------------------------

fn cast_animate_dead_on(runner: &mut GameRunner, animate_dead: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&animate_dead].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: animate_dead,
            card_id,
            targets: vec![target],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Animate Dead is castable with the seeded mana");
}

/// Board A's policy: answer Abdel Adrian's voluntary exile in full while accepts remain, with an
/// empty selection after. `accepts` is a cell so the closure can spend them.
fn board_a_policy(accepts: &mut usize) -> impl FnMut(&GameState) -> Option<GameAction> + '_ {
    move |state: &GameState| match &state.waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            let answer = if *accepts > 0 {
                *accepts -= 1;
                cards.clone()
            } else {
                vec![]
            };
            Some(GameAction::SelectCards { cards: answer })
        }
        _ => None,
    }
}

fn board_a_performed(accepts: usize) -> Option<Performed> {
    let mut board = crate::abdel_adrian_animate_dead_altar_board::build()?;
    // The trace carries the sampler's gate, so a board built with detection OFF records nothing
    // at all; Board A's builder is shared with rows that do not need it.
    board.runner.state_mut().loop_detection =
        engine::types::game_state::LoopDetectionMode::Interactive;
    cast_animate_dead_on(&mut board.runner, board.animate_dead, board.abdel);
    let mut left = accepts;
    let mut policy = board_a_policy(&mut left);
    Some(perform(&mut board.runner, |state| policy(state)))
}

// ---------------------------------------------------------------------------
// Board B
// ---------------------------------------------------------------------------

/// Board B's policy: aim each enters trigger at Felidar Guardian and accept its exile while
/// accepts remain, and decline after. `aim_otherwise` is where a trigger that cannot target Felidar
/// Guardian is aimed; `None` takes the first legal action.
fn board_b_policy(
    felidar: ObjectId,
    aim_otherwise: Option<ObjectId>,
    mut left: usize,
) -> impl FnMut(&GameState) -> Option<GameAction> {
    move |state: &GameState| {
        let legal = engine::ai_support::legal_actions(state);
        let aimed_at = |target: ObjectId| {
            legal.iter().find(|action| {
                matches!(
                    action,
                    GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) } if *id == target
                )
            })
        };
        if left > 0 {
            if let Some(action) = aimed_at(felidar) {
                return Some(action.clone());
            }
        }
        if let Some(action) = aim_otherwise.and_then(aimed_at) {
            return Some(action.clone());
        }
        let accept = left > 0;
        if let Some(action) = legal.iter().find(|action| {
            matches!(
                action,
                GameAction::DecideOptionalEffect { accept: answered } if *answered == accept
            )
        }) {
            if accept {
                left -= 1;
            }
            return Some(action.clone());
        }
        None
    }
}

/// Board B built, with Animate Dead cast onto Felidar Guardian.
fn board_b_cast() -> Option<PrestonBoard> {
    let mut board = build_board_b()?;
    cast_animate_dead_on(&mut board.runner, board.animate_dead, board.felidar);
    Some(board)
}

fn board_b_performed(accepts: usize) -> Option<Performed> {
    let mut board = board_b_cast()?;
    Some(perform(
        &mut board.runner,
        board_b_policy(board.felidar, None, accepts),
    ))
}

/// CR 732.2a — **the two boards' answer classes are disjoint apart from the ordering prompt**,
/// which is why neither board alone satisfies R1. Asserted on the offered spans themselves rather
/// than inherited from the measurement that chose the boards.
#[test]
fn the_two_boards_record_disjoint_classes_apart_from_the_ordering_prompt() {
    let (Some(a), Some(b)) = (board_a_performed(ACCEPTS), board_b_performed(ACCEPTS)) else {
        return;
    };
    let (pa, pb) = (a.classes, b.classes);
    assert!(
        !pa.is_empty() && !pb.is_empty(),
        "positive control: both boards must record at least one class, got {pa:?} / {pb:?}"
    );
    let shared: Vec<&str> = pa
        .iter()
        .filter(|class| pb.contains(class))
        .copied()
        .collect();
    assert!(
        shared.iter().all(|class| *class == "OrderTriggers"),
        "the boards' recorded classes are disjoint apart from the ordering prompt; A {pa:?}, \
         B {pb:?}, shared {shared:?}"
    );
}

/// The seat every board here drives for. Named so a reader does not have to infer it from the
/// builders.
#[allow(dead_code)]
const DRIVER: PlayerId = P0;
