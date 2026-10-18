//! CR 732.2a: a loop period's classification — whose period it is, whether a play at priority
//! drives it, and so who drives it — answers on the blink boards and on a restored offer as it did
//! at this row's base, read on the trace's named span and on the offer's confirmed period.

use engine::game::scenario::{GameRunner, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, PersistedGameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

/// A period's classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answers {
    pub(crate) controller: Option<PlayerId>,
    pub(crate) priority_driven: bool,
    pub(crate) driver: Option<PlayerId>,
}

impl Answers {
    /// CR 732.3: the period's seat is the one seat making its optional choices; a play made at
    /// priority drives it (CR 117.1a + CR 117.1b).
    fn of(choices: impl IntoIterator<Item = (PlayerId, bool)>) -> Self {
        let mut seats = Vec::new();
        let mut priority_driven = false;
        for (seat, at_priority) in choices {
            seats.push(seat);
            priority_driven |= at_priority;
        }
        seats.sort_unstable_by_key(|seat| seat.0);
        seats.dedup();
        let controller = match seats[..] {
            [seat] => Some(seat),
            _ => None,
        };
        Answers {
            controller,
            priority_driven,
            driver: controller.filter(|_| priority_driven),
        }
    }
}

/// The classification of the span the window's trace offers, or else last named.
pub(crate) fn answers(state: &GameState) -> Answers {
    use engine::game::{AnswerOptionality, EntryKind, PromptClass};
    let view = engine::game::play_trace_view(state).expect("the window is traced");
    let span = view
        .offered
        .or_else(|| view.named.last().copied())
        .expect("the trace names a span");
    Answers::of(
        view.entries[span.start..span.end]
            .iter()
            .filter_map(|entry| match &entry.kind {
                EntryKind::Play { .. } => Some((entry.seat, entry.prompt == PromptClass::Priority)),
                EntryKind::Answer { optional, .. } if *optional == AnswerOptionality::Optional => {
                    Some((entry.seat, false))
                }
                EntryKind::Answer { .. } | EntryKind::Resolution { .. } => None,
            }),
    )
}

/// The classification of the period a standing offer carries, read from its wire form.
fn offer_answers(state: &GameState) -> Answers {
    let WaitingFor::LoopShortcut { period, .. } = &state.waiting_for else {
        panic!("expected an offer, got {:?}", state.waiting_for);
    };
    let wire = serde_json::to_value(period).expect("a period serializes");
    Answers::of(
        wire["items"]
            .as_array()
            .expect("a period is a list")
            .iter()
            .map(|item| {
                let seat =
                    serde_json::from_value(item["seat"].clone()).expect("an item names its seat");
                (seat, item["play"][1] == "Priority")
            }),
    )
}

/// Beat cap for a frame drive; it bounds a runaway drive and is read by no assertion.
const FRAME_BEATS: usize = 400;

/// How many of the board's voluntary choices the declared drive accepts before it declines.
const ACCEPTS: usize = 9;

fn top_trigger_source(state: &GameState) -> Option<ObjectId> {
    match &state.stack.back()?.kind {
        StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
        _ => None,
    }
}

/// CR 603.3b: Altar of the Brood's triggers are put on the stack last, so they resolve first.
pub(crate) fn altar_resolves_first(state: &GameState) -> Option<GameAction> {
    let WaitingFor::OrderTriggers { triggers, .. } = &state.waiting_for else {
        return None;
    };
    let (mut altar, mut order): (Vec<usize>, Vec<usize>) = (0..triggers.len())
        .partition(|&i| triggers[i].source_name.starts_with("Altar of the Brood"));
    order.append(&mut altar);
    Some(GameAction::OrderTriggers { order })
}

fn cast_animate_dead(runner: &mut GameRunner, animate_dead: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&animate_dead].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: animate_dead,
            card_id,
            targets: vec![target],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Animate Dead is castable from the built board");
}

/// Three consecutive windows of one board: each as the period is read there, and each as the
/// cover sees it once an offer there is declined.
pub(crate) struct Frames {
    pub(crate) read: [GameState; 3],
    pub(crate) cover: [GameState; 3],
}

/// Three consecutive windows at which `minting`'s trigger stands on top of the stack with a
/// span the trace names and P0 holds priority or is offered the loop, reached through `apply()` under
/// the board's declared drive. Each offer is declined once its frame is read.
fn capture_frames(
    runner: &mut GameRunner,
    minting: ObjectId,
    mut declared: impl FnMut(&GameState) -> Option<GameAction>,
) -> Frames {
    let (mut read, mut cover) = (Vec::new(), Vec::new());
    for _ in 0..FRAME_BEATS {
        let state = runner.state();
        let offer = matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { proposer, .. } if proposer == P0
        );
        // CR 117.3b/c: an offer can stand before the minting trigger's recurrence window, at the
        // first window after a repeated resolution.
        let window = offer
            || (matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
                && top_trigger_source(state) == Some(minting)
                && engine::game::play_trace_view(state).is_some_and(|view| !view.named.is_empty()));
        if window {
            read.push(state.clone());
        }
        let action = offer
            .then_some(GameAction::DeclineShortcut)
            .or_else(|| altar_resolves_first(state))
            .or_else(|| declared(state))
            .unwrap_or_else(|| match state.waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                _ => engine::ai_support::legal_actions(state)
                    .into_iter()
                    .find(|action| !matches!(action, GameAction::PassPriority))
                    .unwrap_or_else(|| {
                        panic!(
                            "a prompt the declared drive does not answer offers a legal action: {}",
                            state.waiting_for.variant_name()
                        )
                    }),
            });
        if window && !offer {
            cover.push(state.clone());
        }
        runner
            .act(action.clone())
            .unwrap_or_else(|error| panic!("{action:?} was rejected: {error:?}"));
        if window && offer {
            cover.push(runner.state().clone());
        }
        if read.len() == 3 {
            return Frames {
                read: read.try_into().expect("three frames"),
                cover: cover.try_into().expect("three frames"),
            };
        }
    }
    panic!(
        "the drive reached {} of three minting frames in {FRAME_BEATS} beats",
        read.len()
    );
}

/// Board A: Animate Dead returns Abdel Adrian, whose enters trigger exiles only Animate Dead
/// while accepts remain and then exiles nothing.
pub(crate) fn board_a_frames() -> Option<Frames> {
    let crate::abdel_adrian_animate_dead_altar_board::AbdelAnimateAltarBoard {
        mut runner,
        abdel,
        animate_dead,
        ..
    } = crate::abdel_adrian_animate_dead_altar_board::build()?;
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let returned = runner.state().objects[&animate_dead].card_id;
    cast_animate_dead(&mut runner, animate_dead, abdel);
    let mut accepts = ACCEPTS;
    Some(capture_frames(&mut runner, abdel, move |state| {
        let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
            return None;
        };
        let exiled = if accepts > 0 {
            accepts -= 1;
            cards
                .iter()
                .copied()
                .filter(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|object| object.card_id == returned)
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(GameAction::SelectCards { cards: exiled })
    }))
}

/// Board B: Animate Dead returns Felidar Guardian. Each Illusion's enters trigger targets the
/// original Felidar Guardian and accepts while accepts remain; the original's own enters trigger
/// targets Altar of the Brood and declines.
pub(crate) fn board_b_frames() -> Option<Frames> {
    let crate::loop_period_trigger_driven_arming::PrestonBoard {
        mut runner,
        felidar,
        animate_dead,
        preston,
        altar,
    } = crate::loop_period_trigger_driven_arming::build_board_b()?;
    cast_animate_dead(&mut runner, animate_dead, felidar);
    let mut accepts = ACCEPTS;
    Some(capture_frames(
        &mut runner,
        preston,
        move |state| match &state.waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                let aimed = if top_trigger_source(state) == Some(felidar) {
                    altar
                } else {
                    felidar
                };
                engine::ai_support::legal_actions(state)
                    .into_iter()
                    .find(|action| {
                        matches!(action, GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(id)),
                        } if *id == aimed)
                    })
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                let accept = *source_id != felidar && accepts > 0;
                if accept {
                    accepts -= 1;
                }
                Some(GameAction::DecideOptionalEffect { accept })
            }
            _ => None,
        },
    ))
}

#[test]
fn every_census_read_answers_on_the_blink_boards_and_a_restored_offer_as_at_the_base() {
    let trigger_driven = Answers {
        controller: Some(P0),
        priority_driven: false,
        driver: None,
    };
    let (Some(board_a), Some(board_b)) = (board_a_frames(), board_b_frames()) else {
        return;
    };
    for (frame, state) in board_a.read.iter().enumerate() {
        // CR 117.3b/c: board A is offered at the first window after a repeated resolution.
        assert_eq!(
            engine::game::play_trace_view(state)
                .and_then(|view| view.offered)
                .map(|span| span.cause),
            Some(engine::game::NamingCause::Repeat),
            "board A frame {frame} is an offer of the span a repeat named"
        );
        assert_eq!(answers(state), trigger_driven, "board A frame {frame}");
    }
    for (frame, state) in board_b.read.iter().enumerate() {
        assert_eq!(answers(state), trigger_driven, "board B frame {frame}");
    }

    // The committed offer predates the confirmed period, so a live cycle re-reaches it first.
    let live = crate::combo_infinite_pile::offer_state();
    let saved = serde_json::to_string(&PersistedGameState::capture(live)).expect("the offer saves");
    let offer = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the save parses")
        .into_game_state()
        .expect("the offer restores");
    assert!(
        matches!(offer.waiting_for, WaitingFor::LoopShortcut { .. }),
        "the restored board stands at an offer"
    );
    assert_eq!(
        offer_answers(&offer),
        Answers {
            controller: Some(P0),
            priority_driven: true,
            driver: Some(P0),
        },
        "combo_infinite_pile_4p_offer at restore"
    );
}
