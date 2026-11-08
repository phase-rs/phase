//! CR 732.2a: the confirmer. It replays a candidate period the play trace names, twice, from the
//! priority frame it is asked at, on the proposer's own information set, and certifies the
//! replay's frames through the recurrence cover and the sign check. A period that spends
//! something of its proposer's own is not shown to repeat; it is counted by replaying it until a
//! repetition is refused.
//!
//! The trace (`play_trace`) records and names; this module decides whether a named span is a
//! period a shortcut may repeat.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analysis::decision_template::{
    ChoicePoint, DecisionSlot, DecisionTemplate, MayChoiceOption, PinnedDecision,
    ShortcutDecisionSchema, TargetPin,
};
use crate::analysis::loop_check::{classify_win_kind, LoopCertificate, WinKind};
use crate::analysis::resource::{
    certify_instructed_library_departure, driving_resources_non_decreasing_carried, frame_without,
    CertifiedInstructedDeparture, CleanupPair, CounterClass, CoveredGrowth, ObjectClass,
    ObjectGrowthVerdict, PredictedDeparture, RecurrenceCover, ResourceVector,
};
use crate::game::engine::{
    announced_target_pins, apply, certify_object_growth_frames, clear_frame_bookkeeping,
    counted_offer, has_no_loss_axis, minted_battlefield_ids, object_decision_source,
    period_sign_check, pinnable_mana_color, pinned_decisions_to_points, proliferate_pins,
    replay_bounded_offer, PeriodSign, SimulationProbeGuard,
};
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::game::mana_payment::{select_convoke_taps, ConvokeTapOrder};
use crate::game::play_trace::{
    self, AnswerOptionality, CostChoices, CostMove, EntryKind, NamedSpan, PeriodReach, PlayLocus,
    PromptClass, TraceEntry,
};
use crate::types::ability::TargetRef;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CostResume, GameState, ManaChoiceContext, PayCostKind, StackEntryKind, WaitingFor, YieldTarget,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::mana::ManaSourceSelection;
use crate::types::player::PlayerId;

/// Why the confirmer did not confirm a span, in the order its stages read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferRefusal {
    /// CR 732.3: optional plays or answers from more than one player.
    Fragmented { seats: Vec<PlayerId> },
    /// Engine policy under CR 732.2a: a span is offered to the player whose optional plays and
    /// answers it is made of; the holder is not offered `seat`'s span.
    ForeignSeat { seat: PlayerId },
    /// CR 732.2a: a replayed action drew a random outcome.
    Randomness,
    /// A prompt the replay reached that no recorded answer answers.
    UnanswerablePrompt,
    /// CR 732.2a: the reducer rejected a replayed play.
    IllegalReplayedPlay,
    /// CR 400.7: an object a replayed cost moved arrived somewhere other than where it did.
    ArrivalDiverged,
    /// CR 732.1b: the step ended or the game ended before the period came round again.
    NoRecurrence,
    /// The replay's frames do not cover one another.
    Cover(ObjectGrowthVerdict),
    /// The period makes no progress for its controller.
    NoAxis,
    /// CR 732.2a + CR 613.4c: the period grows history a static reads, so each repetition's delta
    /// grows and neither a threshold count nor a constant per-repetition delta is true of it.
    GrowingLoss,
    /// The replayed play or answer settled a life change the engine cannot represent. This is
    /// an engine limit, not a rules outcome: the replay propagates it so the public shortcut
    /// entry can reject the take instead of silently stopping the repetition.
    LifeChangeUnrepresentable { player: PlayerId },
}

/// One recorded play or answer of a period, as the replay makes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PeriodItem {
    seat: PlayerId,
    action: GameAction,
    /// For a play: the stack depth and prompt class it was made at.
    play: Option<(usize, PromptClass)>,
    /// An answer the instruction asking it did not let its chooser decline.
    mandatory_answer: bool,
    /// The object-id counter when it was recorded.
    next_object_id: u64,
    /// Ids from here up to `next_object_id` were minted in the period before it was recorded.
    minted_since: u64,
    cost_move: Option<CostMove>,
    /// The decision point the answer was made at, which the every-option proof enumerates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    point: Option<ChoicePoint>,
}

/// CR 732.2a: the plays and answers of a confirmed period, as an offer carries them to its take.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfirmedPeriod {
    items: Vec<PeriodItem>,
    /// What the cover that confirmed the period admitted as its growth.
    #[serde(default)]
    growth: CoveredGrowth,
    /// How far past its step the period runs before it comes round again.
    #[serde(default)]
    reach: PeriodReach,
    /// CR 732.2a: the game choices the first replayed cycle's prompts were answered with, which a
    /// recorded take performs.
    #[serde(default)]
    choices: Vec<PinnedDecision>,
    /// Which authority counted the period's offer.
    #[serde(default, skip_serializing_if = "CountedBy::is_threshold")]
    counted_by: CountedBy,
    /// How one repetition is performed; `None` when no plays were read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    performance: Option<PeriodPerformance>,
    /// CR 732.2a: how far each declaration the offer published reaches when performed from the
    /// offer frame; empty on every offer the threshold authority's replay did not count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) performed: Vec<PerformedReach>,
    /// CR 732.2a: how far the recorded answers reach when performed, on an offer that publishes
    /// no declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recorded: Option<PerformedReach>,
    /// CR 705.1 + CR 706.1: whether a take keeps a coin flip or die roll its performance draws, as
    /// the replay that certified the period did.
    #[serde(default, skip_serializing_if = "Outcomes::is_refused")]
    outcomes: Outcomes,
}

/// CR 732.2a: one declaration performed forward from an offer frame, repetition by repetition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PerformedReach {
    /// The performed declaration's pins, which key the record.
    pub(crate) decisions: Vec<PinnedDecision>,
    /// The repetitions its proposer is still in the game after.
    pub(crate) reach: u32,
    /// CR 104.2a: the proposer, when the performance ended the game with it winning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) winner: Option<PlayerId>,
    /// CR 800.4a: the seats that left in each repetition it performed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) departures: Vec<PredictedDeparture>,
}

/// CR 605.3a + CR 605.3b: whether a period's repetition is made only of mana abilities, which its
/// player may activate while paying a cost (CR 601.2g) and which resolve without the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeriodPerformance {
    /// Every play is a mana ability and nothing resolves from the stack.
    ManaAbilitiesOnly,
    /// A play is a spell or a non-mana ability, or a triggered ability resolves, which waits for
    /// priority (CR 603.3), so no cost's payment performs it.
    UsesTheStack,
}

impl PeriodPerformance {
    fn of(body: &[TraceEntry]) -> Self {
        let uses_the_stack = body.iter().any(|entry| match &entry.kind {
            EntryKind::Play { locus, .. } => match locus {
                PlayLocus::Mana(..) => false,
                PlayLocus::Cast(_) | PlayLocus::Activate(..) | PlayLocus::Unread => true,
            },
            EntryKind::Resolution { .. } => true,
            EntryKind::Answer { .. } | EntryKind::Remembered { .. } => false,
        });
        if uses_the_stack {
            Self::UsesTheStack
        } else {
            Self::ManaAbilitiesOnly
        }
    }
}

/// CR 118.3 + CR 701.17a: the first play of `body` whose activation cost, read on `frame`, moves
/// a card to or from a library; a spell has no such cost.
fn library_cost_play(body: &[TraceEntry], frame: &GameState) -> Option<PlayLocus> {
    body.iter().find_map(|entry| {
        let EntryKind::Play { locus, .. } = &entry.kind else {
            return None;
        };
        let (PlayLocus::Activate(id, index) | PlayLocus::Mana(id, Some(index))) = *locus else {
            return None;
        };
        frame
            .objects
            .get(&id)?
            .abilities
            .get(index)?
            .cost
            .as_ref()
            .is_some_and(|cost| cost.moves_card_to_or_from_library())
            .then_some(*locus)
    })
}

/// CR 732.2a: which authority states the count of a confirmed period's offer; its take answers to
/// the same one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CountedBy {
    /// The threshold authority, by arithmetic over one certified period; a period that repeats
    /// without bound has no count.
    #[default]
    Threshold,
    /// The counting replay, by performing the period: the seats that left the game in each
    /// repetition it performed, the one that ended the count included.
    Replay { departures: Vec<PredictedDeparture> },
}

impl CountedBy {
    fn is_threshold(&self) -> bool {
        *self == CountedBy::Threshold
    }
}

// `GameAction` derives no `Eq`, and no value an action carries is floating-point.
impl Eq for ConfirmedPeriod {}

impl ConfirmedPeriod {
    /// Whether no period was confirmed; a confirmed period of passes alone has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.performance.is_none()
    }

    pub(crate) fn growth(&self) -> CoveredGrowth {
        self.growth
    }

    pub fn choices(&self) -> &[PinnedDecision] {
        &self.choices
    }

    pub fn performance(&self) -> Option<PeriodPerformance> {
        self.performance
    }

    /// CR 732.2a: the answers a take of this period performs, which a declaration must agree
    /// with; `None` when no period was confirmed.
    pub fn recorded_answers(&self) -> Option<&[PinnedDecision]> {
        (!self.is_empty()).then_some(self.choices.as_slice())
    }

    /// CR 732.2a: the departures the counting replay saw, for a period it counted; `None` for a
    /// period it did not count.
    pub(crate) fn replayed_departures(&self) -> Option<&[PredictedDeparture]> {
        match &self.counted_by {
            CountedBy::Threshold => None,
            CountedBy::Replay { departures } => Some(departures),
        }
    }
}

/// A confirmed period: what it replays, the replay's last frame pair, and the sign check's
/// delta.
#[derive(Debug, Clone)]
pub(crate) struct Confirmation {
    pub(crate) period: ConfirmedPeriod,
    pub(crate) frames: Box<[GameState; 2]>,
    pub(crate) delta: ResourceVector,
    pub(crate) departure: Option<CertifiedInstructedDeparture>,
    /// The printed identity of each triggered ability the first replayed cycle resolved, in order.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) performed: Vec<String>,
    /// CR 732.2a: the offer at its stated count — a loss period's as the threshold authority
    /// measured it on the replayed frames (CR 704.5a), or a spend period's as the counting replay
    /// performed it; `None` on the unbounded road.
    pub(crate) bounded: Option<BoundedOfferParts>,
}

/// The certificate, schema and declaration a bounded offer publishes, and the winner its
/// published declaration's performance ended the game with.
#[derive(Debug, Clone)]
pub(crate) struct BoundedOfferParts {
    pub(crate) certificate: LoopCertificate,
    pub(crate) schema: ShortcutDecisionSchema,
    pub(crate) declaration: Option<DecisionTemplate>,
    pub(crate) predicted_winner: Option<PlayerId>,
    /// The witness built with the other parking, performed beside the suggestion.
    pub(crate) other_suggestion: Option<DecisionTemplate>,
}

/// The refusal of a span whose optional choices are not `holder`'s alone: more than one seat's
/// is a fragmented loop (CR 732.3), and one other seat's is that seat's period.
fn seat_refusal(body: &[TraceEntry], holder: PlayerId) -> Option<OfferRefusal> {
    let mut seats: Vec<PlayerId> = body
        .iter()
        .filter(|entry| optional_choice(entry))
        .map(|entry| entry.seat)
        .collect();
    seats.sort_unstable();
    seats.dedup();
    match seats[..] {
        [] => None,
        [seat] if seat == holder => None,
        [seat] => Some(OfferRefusal::ForeignSeat { seat }),
        _ => Some(OfferRefusal::Fragmented { seats }),
    }
}

/// Whether `entry` is its seat's optional choice.
fn optional_choice(entry: &TraceEntry) -> bool {
    match &entry.kind {
        // CR 117.1a + CR 117.1b: casting a spell or activating an ability is optional.
        EntryKind::Play { .. } => true,
        EntryKind::Answer { optional, .. } => *optional == AnswerOptionality::Optional,
        // CR 603.5: a stored preference's answer is still its seat's optional choice.
        EntryKind::Remembered { .. } => true,
        EntryKind::Resolution { .. } => false,
    }
}

/// The span's plays and answers as `frame` replays them, rotated to begin at the trace's current
/// position; priority passes are left to the replay (CR 117.4).
fn period_items(
    entries: &im::Vector<TraceEntry>,
    span: NamedSpan,
    frame: &GameState,
) -> Vec<PeriodItem> {
    let len = span.end - span.start;
    let rotation = if span.end < entries.len() {
        (entries.len() - span.start) % len
    } else {
        0
    };
    (span.start + rotation..span.end)
        .chain(span.start..span.start + rotation)
        .filter_map(|at| {
            let entry = &entries[at];
            // CR 111.7 + CR 111.1: a play whose token source the period made again is the play
            // one period later, made from that token, which the period minted before it.
            if let (
                EntryKind::Play {
                    action,
                    locus: PlayLocus::Mana(spent, _),
                    ..
                },
                Some(source_id),
            ) = (&entry.kind, play_trace::remade_play_source(frame, span, at))
            {
                let minted = frame.next_object_id - entries[entries.len() - len].next_object_id;
                let remade = |id: ObjectId| if id == *spent { source_id } else { id };
                return Some(PeriodItem {
                    seat: entry.seat,
                    action: renamed(action, remade, frame),
                    play: Some((entry.depth, entry.prompt)),
                    mandatory_answer: false,
                    next_object_id: entry.next_object_id + minted,
                    minted_since: entry.next_object_id,
                    cost_move: None,
                    point: None,
                });
            }
            let minted_since = entries[at.saturating_sub(len)].next_object_id;
            let (action, play, cost_move, mandatory_answer, point) = match &entry.kind {
                EntryKind::Play { action, .. } => {
                    (action, Some((entry.depth, entry.prompt)), None, false, None)
                }
                EntryKind::Answer {
                    action: GameAction::PassPriority,
                    ..
                } if entry.prompt == PromptClass::Priority => return None,
                EntryKind::Answer {
                    action,
                    cost_move,
                    optional,
                    point,
                    ..
                } => (
                    action,
                    None,
                    cost_move.clone(),
                    *optional == AnswerOptionality::Mandatory,
                    *point,
                ),
                // The replay keeps the stored preferences, which answer it again.
                EntryKind::Resolution { .. } | EntryKind::Remembered { .. } => return None,
            };
            Some(PeriodItem {
                seat: entry.seat,
                action: action.clone(),
                play,
                mandatory_answer,
                next_object_id: entry.next_object_id,
                minted_since,
                cost_move,
                point,
            })
        })
        .collect()
}

/// CR 400.7: the recorded action with each object the period minted before it replaced by the
/// replay's object minted at the same point; every other object is the one recorded.
fn rebound(item: &PeriodItem, replay: &GameState) -> GameAction {
    let shift = replay.next_object_id.wrapping_sub(item.next_object_id);
    let map = |id: ObjectId| {
        if (item.minted_since..item.next_object_id).contains(&id.0) {
            ObjectId(id.0.wrapping_add(shift))
        } else {
            id
        }
    };
    renamed(&item.action, map, replay)
}

/// `action` with each object it names replaced by the one `map` gives for it. CR 400.7: a mana
/// source selection whose source is replaced names that object as `state` holds it.
fn renamed(
    action: &GameAction,
    map: impl Fn(ObjectId) -> ObjectId,
    state: &GameState,
) -> GameAction {
    let target = |t: &TargetRef| match t {
        TargetRef::Object(id) => TargetRef::Object(map(*id)),
        TargetRef::Player(p) => TargetRef::Player(*p),
    };
    let selected = |selection: &ManaSourceSelection| {
        let id = map(selection.source.object_id);
        ManaSourceSelection {
            source: state
                .objects
                .get(&id)
                .filter(|_| id != selection.source.object_id)
                .map_or(selection.source, ObjectIncarnationRef::from_object),
            ..selection.clone()
        }
    };
    match action {
        GameAction::ChooseTarget { target: chosen } => GameAction::ChooseTarget {
            target: chosen.as_ref().map(target),
        },
        GameAction::SelectTargets { targets } => GameAction::SelectTargets {
            targets: targets.iter().map(target).collect(),
        },
        GameAction::SelectCards { cards } => GameAction::SelectCards {
            cards: cards.iter().copied().map(map).collect(),
        },
        GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode,
        } => GameAction::ActivateAbility {
            source_id: map(*source_id),
            ability_index: *ability_index,
            payment_mode: *payment_mode,
        },
        GameAction::TapLandForMana { selection } => GameAction::TapLandForMana {
            selection: selected(selection),
        },
        GameAction::ActivateManaSource { selection } => GameAction::ActivateManaSource {
            selection: selected(selection),
        },
        GameAction::CastSpell {
            object_id,
            card_id,
            targets,
            payment_mode,
        } => GameAction::CastSpell {
            object_id: map(*object_id),
            card_id: *card_id,
            targets: targets.iter().copied().map(map).collect(),
            payment_mode: *payment_mode,
        },
        // An action naming no object, or one whose objects the period does not mint.
        other => other.clone(),
    }
}

/// CR 603.2c: one occurrence of a triggered ability, by its source and text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Occurrence {
    source_name: String,
    description: Option<String>,
}

impl Occurrence {
    fn of(source_name: &str, description: Option<&String>) -> Self {
        Self {
            source_name: source_name.to_owned(),
            description: description.cloned(),
        }
    }
}

/// What a replayed cycle ends on: the trigger occurrence on top, or the kind of entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TopOfStack {
    Trigger(Occurrence),
    Other(std::mem::Discriminant<StackEntryKind>),
}

fn top_of_stack(state: &GameState) -> Option<TopOfStack> {
    state.stack.back().map(|entry| match &entry.kind {
        StackEntryKind::TriggeredAbility {
            source_name,
            description,
            ..
        } => TopOfStack::Trigger(Occurrence::of(source_name, description.as_ref())),
        other => TopOfStack::Other(std::mem::discriminant(other)),
    })
}

/// The one owner of cycle end and seat liveness for the six replay entry points: `replay_period`'s
/// second cycle, `ReplayedPeriod::floors`, `mandatory_reading`, the counting replay's `Count`,
/// `perform_declared` and the take's `perform_cycle`.
/// Where a cycle must end: priority back with `holder`, with the frame's top of stack and at least
/// its depth (accumulation beneath is admitted), at a window the period's reach admits.
struct FrameEnd {
    reach: PeriodReach,
    depth: usize,
    holder: PlayerId,
    top: Option<TopOfStack>,
    /// CR 800.4a: the seats in the game where the run of repetitions began.
    seats: BTreeSet<PlayerId>,
    /// CR 603.3b: each order the record made, by the triggers it ordered, as the run applied it.
    related: std::cell::RefCell<Vec<Vec<OrderKey>>>,
}

/// CR 603.3b: a trigger an ordering prompt asks about, by its source and its ability with the
/// trigger's own identity cleared, ranked among equal ones.
type OrderKey = (
    (ObjectId, Box<crate::types::ability::ResolvedAbility>),
    usize,
);

/// CR 732.2a + CR 800.4a + CR 104.2a: how one replayed repetition ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CycleEnd {
    /// It came round at its frame end.
    CameRound,
    /// CR 800.4a: a seat other than its holder left in it and it did not come round.
    Departed(OfferRefusal),
    /// CR 104.2a + CR 704.3: the game ended in it with its holder the winner.
    HolderWon(OfferRefusal),
    /// CR 800.4a: its holder left the game in it.
    HolderLeft(OfferRefusal),
    /// CR 514.1: it stopped where its holder awaits its own cleanup discard.
    HolderCleanup(OfferRefusal),
    Refused(OfferRefusal),
}

impl CycleEnd {
    /// Whether the repetition was performed, so it is counted.
    pub(crate) fn performed(&self) -> bool {
        matches!(
            self,
            Self::CameRound | Self::Departed(_) | Self::HolderWon(_) | Self::HolderLeft(_)
        )
    }

    /// Why a cycle that did not come round stopped.
    pub(crate) fn refusal(&self) -> Option<&OfferRefusal> {
        match self {
            Self::CameRound => None,
            Self::Departed(refusal)
            | Self::HolderWon(refusal)
            | Self::HolderLeft(refusal)
            | Self::HolderCleanup(refusal)
            | Self::Refused(refusal) => Some(refusal),
        }
    }
}

/// CR 800.4a: whether a seat of `seats` is no longer in the game at `state`.
fn left_since(seats: &BTreeSet<PlayerId>, state: &GameState) -> bool {
    seats
        .iter()
        .any(|&seat| !crate::game::players::is_alive(state, seat))
}

/// How a cycle `holder` began with `started` in the game, refused with `refusal` at `replay`,
/// ended.
fn stopped(
    replay: &GameState,
    holder: PlayerId,
    started: &BTreeSet<PlayerId>,
    refusal: OfferRefusal,
) -> CycleEnd {
    if replay
        .game_end
        .as_ref()
        .is_some_and(|game_end| game_end.winner == Some(holder))
    {
        // CR 104.2a + CR 704.3: the holder won in it.
        CycleEnd::HolderWon(refusal)
    } else if !crate::game::players::is_alive(replay, holder) {
        // CR 800.4a: the holder left the game in it.
        CycleEnd::HolderLeft(refusal)
    } else if matches!(replay.waiting_for, WaitingFor::DiscardToHandSize { player, .. } if player == holder)
    {
        // CR 514.1: the holder's own cleanup discard is no choice the period contains.
        CycleEnd::HolderCleanup(refusal)
    } else if replay.game_end.is_none() && left_since(started, replay) {
        // CR 800.4a: another seat left the game in it.
        CycleEnd::Departed(refusal)
    } else {
        CycleEnd::Refused(refusal)
    }
}

impl FrameEnd {
    fn of(frame: &GameState, holder: PlayerId, reach: PeriodReach) -> Self {
        Self {
            reach,
            depth: frame.stack.len(),
            holder,
            top: top_of_stack(frame),
            seats: seats(frame),
            related: Default::default(),
        }
    }

    /// CR 800.4a: whether a seat living where the run began has left the game at `state`.
    fn departed(&self, state: &GameState) -> bool {
        left_since(&self.seats, state)
    }

    /// CR 732.2a: performs one repetition of `items` from `replay` and classifies how it ended.
    fn repeat(
        &self,
        replay: &mut GameState,
        items: &[PeriodItem],
        choices: &mut Vec<PinnedDecision>,
        frames: Option<&mut Vec<GameState>>,
        events: &mut Vec<GameEvent>,
        declared: Declared<'_>,
    ) -> CycleEnd {
        let started = seats(replay);
        match replay_cycle_ordered(replay, items, self, choices, frames, events, None, declared) {
            Ok(_) => CycleEnd::CameRound,
            Err(refusal) => stopped(replay, self.holder, &started, refusal),
        }
    }

    /// Whether the cycle begun at window `start` may still run at `state`: the same step
    /// in-step, the same turn for a combat period (CR 500.8), and the holder's turns for an
    /// extra-turn period (CR 500.7). Nothing once the holder has left the game, whose turn goes on
    /// without it (CR 800.4a + CR 800.4j).
    fn admits(&self, start: play_trace::WindowKey, state: &GameState) -> bool {
        if !crate::game::players::is_alive(state, self.holder) {
            return false;
        }
        let now = play_trace::WindowKey::of(state);
        match self.reach {
            PeriodReach::InStep => now == start,
            PeriodReach::Combat => now.turn() == start.turn(),
            PeriodReach::ExtraTurn => state.active_player == self.holder,
        }
    }

    /// Whether the cycle begun at window `start` has come round at `state`; a carried cycle only
    /// at a later window of its start's step.
    fn reached(&self, start: play_trace::WindowKey, state: &GameState) -> bool {
        let now = play_trace::WindowKey::of(state);
        let window = match self.reach {
            PeriodReach::InStep => true,
            PeriodReach::Combat | PeriodReach::ExtraTurn => {
                now.phase() == start.phase() && now != start
            }
        };
        window
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == self.holder)
            && state.stack.len() >= self.depth
            && top_of_stack(state) == self.top
    }
}

/// CR 705.1 + CR 706.1: what random outcomes wrote: the objects an outcome's step created, and
/// the counters it changed on the others.
#[derive(Debug, Clone, Default)]
pub(crate) struct Written {
    created: BTreeSet<ObjectId>,
    countered: BTreeSet<(ObjectId, crate::types::counter::CounterType)>,
}

impl Written {
    const NONE: Written = Written {
        created: BTreeSet::new(),
        countered: BTreeSet::new(),
    };

    /// What the step from `before`'s objects to `after` wrote.
    fn of_step(before: &crate::types::object_map::ObjectMap, after: &GameState) -> Written {
        let mut written = Written::default();
        for (id, object) in after.objects.iter() {
            let Some(prior) = before.get(id) else {
                written.created.insert(*id);
                continue;
            };
            for counter in object.counters.keys().chain(prior.counters.keys()) {
                if object.counters.get(counter) != prior.counters.get(counter) {
                    written.countered.insert((*id, counter.clone()));
                }
            }
        }
        written
    }

    fn extend(&mut self, other: &Written) {
        self.created.extend(other.created.iter().copied());
        self.countered.extend(other.countered.iter().cloned());
    }

    fn objects(&self) -> BTreeSet<ObjectId> {
        let countered = self.countered.iter().map(|(id, _)| *id);
        self.created.iter().copied().chain(countered).collect()
    }
}

/// CR 705.1 + CR 706.1 + CR 400.7 + CR 111.7: every object a period's random outcomes wrote
/// leaves the zone it was written in within the cycle, feeding no choice, no player and no
/// reader on the way, so no outcome is a way to stop the period; `written` is what the reading's
/// outcomes wrote.
#[derive(Debug, Clone)]
pub(crate) struct OutcomeCertificate {
    written: Written,
}

/// A reading's first-cycle writes, followed until each leaves the zone it was written in.
#[derive(Default)]
struct Lifetime {
    second_cycle: bool,
    /// Each object the first cycle's outcomes wrote or could have written, and its zone then.
    first: Vec<(ObjectId, crate::types::zones::Zone)>,
    /// Those that have since left that zone.
    left: BTreeSet<ObjectId>,
    /// Each priority frame from the write while one is still in its zone.
    frames: Vec<GameState>,
    /// The triggered abilities that wrote, and those that moved a written object out of its zone.
    authors: Vec<crate::types::ability::TriggerDefinitionRef>,
    /// An outcome fed a player or a prompt, or a confining move changed a player.
    escaped: bool,
}

impl Lifetime {
    /// Whether a written object is still in the zone it was written in at `state`.
    fn holds(&self, state: &GameState) -> bool {
        self.first.iter().any(|(id, zone)| {
            !self.left.contains(id) && state.objects.get(id).is_some_and(|o| o.zone == *zone)
        })
    }
}

/// While outcomes are admitted: how many outcome draws the replays kept and what they wrote.
#[derive(Default)]
struct OutcomeRecord {
    kept: u64,
    written: Written,
    lifetime: Option<Lifetime>,
}

/// The state a step's record compares against, read before the step.
struct BeforeStep {
    objects: crate::types::object_map::ObjectMap,
    players: Vec<PlayerMarks>,
    /// The triggered ability resolving, or on top of the stack.
    resolving: Option<crate::types::ability::TriggerDefinitionRef>,
    /// What any outcome of the ability resolving, or on top of the stack, can write.
    can_write: Option<OutcomeWrites>,
}

/// CR 705.1 + CR 706.1: what any outcome of a coin flip or die roll can write, read from its
/// definition: counters on its source, and whether an outcome creates objects.
#[derive(Default)]
struct OutcomeWrites {
    objects: BTreeSet<ObjectId>,
    creates: bool,
}

impl OutcomeWrites {
    /// Every outcome branch of `effect`, whose source is `source`; `None` when `effect` draws no
    /// branched outcome or a branch does anything else.
    fn of(effect: &crate::types::ability::Effect, source: ObjectId) -> Option<Self> {
        use crate::types::ability::{Effect, TargetFilter};
        let branches: Vec<&crate::types::ability::AbilityDefinition> = match effect {
            Effect::FlipCoins {
                win_effect,
                lose_effect,
                ..
            } => win_effect.iter().chain(lose_effect).map(|b| &**b).collect(),
            Effect::RollDie { results, .. } => results.iter().map(|b| &*b.effect).collect(),
            _ => return None,
        };
        if branches.is_empty() {
            return None;
        }
        let mut writes = OutcomeWrites::default();
        for branch in branches {
            let mut next = Some(branch);
            while let Some(def) = next {
                match &*def.effect {
                    Effect::PutCounter {
                        target: TargetFilter::SelfRef,
                        ..
                    } => {
                        writes.objects.insert(source);
                    }
                    Effect::Token { .. } => writes.creates = true,
                    _ => return None,
                }
                next = def.sub_ability.as_deref();
            }
        }
        Some(writes)
    }
}

/// CR 119 + CR 122.1 + CR 400.1: what of a player a random outcome could feed.
type PlayerMarks = (
    i32,
    im::Vector<ObjectId>,
    im::Vector<ObjectId>,
    im::Vector<ObjectId>,
    u32,
);

fn player_marks(state: &GameState) -> Vec<PlayerMarks> {
    state
        .players
        .iter()
        .map(|p| {
            (
                p.life,
                p.hand.clone(),
                p.library.clone(),
                p.graveyard.clone(),
                p.poison_counters,
            )
        })
        .collect()
}

impl BeforeStep {
    fn of(state: &GameState) -> Self {
        let ability = state
            .resolving_stack_entry
            .as_ref()
            .or_else(|| state.stack.back())
            .and_then(|entry| entry.ability());
        BeforeStep {
            objects: state.objects.clone(),
            players: player_marks(state),
            resolving: ability.and_then(|ability| ability.trigger_definition_ref.clone()),
            can_write: ability
                .and_then(|ability| OutcomeWrites::of(&ability.effect, ability.source_id)),
        }
    }
}

impl OutcomeRecord {
    /// Notes the step from `before` to `after`, which drew `drawn` outcomes and emitted `events`.
    fn note(&mut self, before: BeforeStep, after: &GameState, drawn: u64, events: &[GameEvent]) {
        let fed_a_player = before.players != player_marks(after);
        if drawn > 0 {
            self.kept += drawn;
            let wrote = Written::of_step(&before.objects, after);
            if let Some(lifetime) = &mut self.lifetime {
                // A sampled outcome is one of many: the subject is what any outcome can write,
                // and a trigger keyed on the outcome itself reads every one (CR 603.2).
                lifetime.escaped |= fed_a_player
                    || !matches!(after.waiting_for, WaitingFor::Priority { .. })
                    || before
                        .can_write
                        .as_ref()
                        .is_none_or(|can| can.creates && wrote.created.is_empty())
                    || outcome_keyed(after);
                lifetime.authors.extend(before.resolving.clone());
                if !lifetime.second_cycle {
                    let derived = before.can_write.iter().flat_map(|can| can.objects.iter());
                    let ids: BTreeSet<ObjectId> = wrote
                        .objects()
                        .into_iter()
                        .chain(derived.copied())
                        .collect();
                    for id in ids {
                        if let Some(object) = after.objects.get(&id) {
                            if !lifetime.first.contains(&(id, object.zone)) {
                                lifetime.first.push((id, object.zone));
                            }
                        }
                    }
                }
            }
            self.written.extend(&wrote);
        } else if let Some(lifetime) = &mut self.lifetime {
            let moved: Vec<ObjectId> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(from),
                        ..
                    } if lifetime.first.contains(&(*object_id, *from))
                        && !lifetime.left.contains(object_id) =>
                    {
                        Some(*object_id)
                    }
                    _ => None,
                })
                .collect();
            if !moved.is_empty() {
                lifetime.escaped |= fed_a_player;
                lifetime.authors.extend(before.resolving);
                lifetime.left.extend(moved);
            }
        }
        if let Some(lifetime) = &mut self.lifetime {
            if matches!(after.waiting_for, WaitingFor::Priority { .. }) && lifetime.holds(after) {
                lifetime.frames.push(after.clone());
            }
        }
    }

    /// CR 400.7 + CR 111.7 + CR 704.5d + CR 603.2: the certificate, when every object the first
    /// cycle wrote leaves the zone it was written in again in the second cycle, whose `events`
    /// end at `end`, and no surface but its authors' reads one while it is there.
    fn certificate(&self, end: &GameState, events: &[GameEvent]) -> Option<OutcomeCertificate> {
        let lifetime = self.lifetime.as_ref()?;
        let leaves = lifetime.first.iter().all(|&(id, zone)| {
            !end.objects.contains_key(&id)
                || events.iter().any(|event| {
                    matches!(event, GameEvent::ZoneChanged { object_id, from: Some(from), .. }
                        if *object_id == id && *from == zone)
                })
        });
        let unread = || {
            lifetime.frames.iter().all(|frame| {
                lifetime
                    .first
                    .iter()
                    .filter(|(id, zone)| frame.objects.get(id).is_some_and(|o| o.zone == *zone))
                    .all(|(id, _)| {
                        !crate::analysis::resource::written_member_read(
                            frame,
                            *id,
                            &lifetime.authors,
                        )
                    })
            })
        };
        (!lifetime.first.is_empty() && !lifetime.escaped && leaves && unread()).then(|| {
            OutcomeCertificate {
                written: self.written.clone(),
            }
        })
    }
}

/// CR 603.2 + CR 705.1 + CR 706.1: whether a trigger functioning at `state` is keyed on a coin
/// flip or die roll itself.
fn outcome_keyed(state: &GameState) -> bool {
    use crate::types::TriggerMode;
    state.objects.values().any(|obj| {
        crate::game::functioning_abilities::active_trigger_definitions(state, obj).any(|active| {
            matches!(
                active.definition.mode,
                TriggerMode::RolledDie | TriggerMode::RolledDieOnce | TriggerMode::FlippedCoin
            ) && crate::game::triggers::trigger_definition_functions_in_zone(
                active.definition,
                obj.zone,
            )
        })
    })
}

thread_local! {
    /// While outcomes are admitted: what the replays' outcomes kept and wrote.
    static OUTCOMES: std::cell::RefCell<Option<OutcomeRecord>> =
        const { std::cell::RefCell::new(None) };
}

/// CR 705.1 + CR 706.1 + CR 104.4b: while held, a replayed coin flip or die roll is kept as the
/// period's randomness instead of refused, and what it writes is recorded.
#[must_use]
struct OutcomesAdmitted(Option<OutcomeRecord>);

impl OutcomesAdmitted {
    fn enter() -> Self {
        OutcomesAdmitted(OUTCOMES.with(|record| record.replace(Some(OutcomeRecord::default()))))
    }

    /// Enters a guard only where outcomes are already admitted, to read its replays' own writes.
    fn nested() -> Option<Self> {
        outcomes_admitted().then(Self::enter)
    }

    fn with<T>(&self, read: impl FnOnce(&mut OutcomeRecord) -> T) -> T {
        OUTCOMES.with(|record| {
            read(
                record
                    .borrow_mut()
                    .as_mut()
                    .expect("a held guard keeps a record"),
            )
        })
    }

    /// Whether a replay kept an outcome since this guard was entered.
    fn kept(&self) -> bool {
        self.with(|record| record.kept > 0)
    }

    /// What the replays wrote since this guard was entered.
    fn written(&self) -> Written {
        self.with(|record| record.written.clone())
    }

    /// Follows the first cycle's writes, for [`OutcomeRecord::certificate`].
    fn follow_writes(&self) {
        self.with(|record| record.lifetime = Some(Lifetime::default()));
    }

    fn second_cycle(&self) {
        self.with(|record| {
            if let Some(lifetime) = &mut record.lifetime {
                lifetime.second_cycle = true;
            }
        });
    }

    fn certificate(&self, end: &GameState, events: &[GameEvent]) -> Option<OutcomeCertificate> {
        self.with(|record| record.certificate(end, events))
    }
}

impl Drop for OutcomesAdmitted {
    fn drop(&mut self) {
        OUTCOMES.with(|record| {
            let inner = record.replace(self.0.take());
            if let (Some(inner), Some(outer)) = (inner, record.borrow_mut().as_mut()) {
                outer.kept += inner.kept;
                outer.written.extend(&inner.written);
            }
        });
    }
}

fn outcomes_admitted() -> bool {
    OUTCOMES.with(|record| record.borrow().is_some())
}

/// Upper bound on the actions one replayed cycle may take.
const CYCLE_BEATS: usize = 512;

/// Applies one action, refusing a random outcome drawn by it (CR 732.2a) unless outcomes are
/// admitted; its events are appended to `events`.
fn step(
    replay: &mut GameState,
    seat: PlayerId,
    action: GameAction,
    rejected: OfferRefusal,
    events: &mut Vec<GameEvent>,
) -> Result<(), OfferRefusal> {
    let outcomes = replay.rng.outcome_draws();
    let before = outcomes_admitted().then(|| BeforeStep::of(replay));
    let from = events.len();
    events.extend(
        apply(replay, seat, action)
            .map_err(|error| match error {
                crate::game::engine::EngineError::LifeChangeUnrepresentable { player } => {
                    OfferRefusal::LifeChangeUnrepresentable { player }
                }
                _ => rejected,
            })?
            .events,
    );
    let drawn = replay.rng.outcome_draws() - outcomes;
    let Some(before) = before else {
        return if drawn > 0 {
            Err(OfferRefusal::Randomness)
        } else {
            Ok(())
        };
    };
    OUTCOMES.with(|record| {
        if let Some(record) = record.borrow_mut().as_mut() {
            record.note(before, replay, drawn, &events[from..]);
        }
    });
    Ok(())
}

/// CR 601.2h + CR 702.51a: pays a cast's mana payment with the convoke tap set the live board
/// offers, in the detection replay's fodder-first order; how many creatures it tapped, or `None`
/// when this prompt is not such a payment.
fn rebind_convoke(
    replay: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<Option<usize>, OfferRefusal> {
    let (WaitingFor::ManaPayment { player, .. }, Some(pending)) =
        (&replay.waiting_for, replay.pending_cast.as_ref())
    else {
        return Ok(None);
    };
    let player = *player;
    let Some(taps) = select_convoke_taps(
        replay,
        player,
        &pending.cost,
        ConvokeTapOrder::DetectionFodderFirst,
    ) else {
        return Ok(None);
    };
    let before = replay.clone();
    let recorded = events.len();
    let tapped = taps.len();
    for (object_id, mana_type) in taps {
        let tapped = step(
            replay,
            player,
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
            OfferRefusal::UnanswerablePrompt,
            events,
        );
        match tapped {
            Ok(()) => {}
            Err(OfferRefusal::UnanswerablePrompt) => {
                *replay = before;
                events.truncate(recorded);
                return Ok(None);
            }
            Err(refusal) => return Err(refusal),
        }
    }
    Ok(Some(tapped))
}

/// CR 400.7 + CR 601.2h + CR 514.1: a recorded cost or cleanup-discard selection whose objects
/// the prompt does not offer, with each such object replaced by an unused offered object of the
/// same name; `rebound` is the recorded selection with the objects minted in the period already
/// replaced, and `chosen` the name each recorded object had when it was chosen.
fn rebind_cost_selection(
    recorded: &GameAction,
    rebound: GameAction,
    chosen: &[String],
    replay: &GameState,
) -> GameAction {
    let (
        GameAction::SelectCards { cards: original },
        GameAction::SelectCards { cards },
        WaitingFor::PayCost { choices, .. } | WaitingFor::DiscardToHandSize { cards: choices, .. },
    ) = (recorded, &rebound, &replay.waiting_for)
    else {
        return rebound;
    };
    let name = |id: &ObjectId| replay.objects.get(id).map(|object| object.name.as_str());
    let mut unused: Vec<ObjectId> = choices
        .iter()
        .filter(|id| !cards.contains(id))
        .copied()
        .collect();
    let mut rebound_cards = Vec::with_capacity(cards.len());
    for (index, (id, original)) in cards.iter().zip(original).enumerate() {
        if choices.contains(id) {
            rebound_cards.push(*id);
            continue;
        }
        let Some(printed) = name(id)
            .or_else(|| name(original))
            .or_else(|| chosen.get(index).map(String::as_str))
        else {
            return rebound;
        };
        let Some(at) = unused.iter().position(|other| name(other) == Some(printed)) else {
            return rebound;
        };
        rebound_cards.push(unused.swap_remove(at));
    }
    GameAction::SelectCards {
        cards: rebound_cards,
    }
}

/// One replayed cycle from `replay`'s current state; the source of each triggered ability it
/// resolved, in order. `frames`, when given, collects every priority frame the cycle passes, and
/// `events` collects every event its actions emit.
fn replay_cycle(
    replay: &mut GameState,
    items: &[PeriodItem],
    end: &FrameEnd,
    choices: &mut Vec<PinnedDecision>,
    frames: Option<&mut Vec<GameState>>,
    events: &mut Vec<GameEvent>,
) -> Result<Vec<String>, OfferRefusal> {
    replay_cycle_ordered(replay, items, end, choices, frames, events, None, None)
}

/// A declaration's pins and the repetition index they answer at.
type Declared<'a> = Option<(&'a DecisionTemplate, u32)>;

/// An occurrence and its rank among the equal occurrences at one ordering point, in prompt order.
type Label = (Occurrence, usize);

/// CR 603.2c: labels `occurrences` by their rank among equal ones, so equal occurrences stay
/// distinct triggered abilities to order.
fn ranked(occurrences: Vec<Occurrence>) -> Vec<Label> {
    let mut labels: Vec<Label> = Vec::with_capacity(occurrences.len());
    for occurrence in occurrences {
        let rank = labels
            .iter()
            .filter(|(seen, _)| *seen == occurrence)
            .count();
        labels.push((occurrence, rank));
    }
    labels
}

/// CR 603.3b: the occurrences the ordering prompt at `replay` asks about, in prompt order.
fn point_labels(replay: &GameState) -> Option<Vec<Label>> {
    let WaitingFor::OrderTriggers { triggers, .. } = &replay.waiting_for else {
        return None;
    };
    let group = replay
        .pending_trigger_order
        .as_ref()?
        .groups
        .iter()
        .find(|group| !group.ordered)?;
    let occurrences = triggers
        .iter()
        .zip(&group.triggers)
        .map(|(summary, context)| {
            Occurrence::of(&summary.source_name, context.pending.description.as_ref())
        })
        .collect();
    Some(ranked(occurrences))
}

/// The prompt index of each label of `order`, an order of the prompt's `labels`.
fn order_indices(labels: &[Label], order: &[Label]) -> Option<Vec<usize>> {
    let mut used = vec![false; labels.len()];
    order
        .iter()
        .map(|label| {
            let at = (0..labels.len()).find(|&i| !used[i] && labels[i] == *label)?;
            used[at] = true;
            Some(at)
        })
        .collect()
}

/// CR 603.3b: the triggers the ordering prompt at `replay` asks about, in prompt order, keyed by
/// the engine's trigger identity (`triggers::normalize_ability_identity`).
fn order_keys(replay: &GameState) -> Option<Vec<OrderKey>> {
    let group = replay
        .pending_trigger_order
        .as_ref()?
        .groups
        .iter()
        .find(|group| !group.ordered)?;
    let mut keys: Vec<OrderKey> = Vec::with_capacity(group.triggers.len());
    for context in &group.triggers {
        let mut ability = context.pending.ability.clone();
        crate::game::triggers::normalize_ability_identity(&mut ability);
        let identity = (context.pending.source_id, ability);
        let rank = keys.iter().filter(|(seen, _)| *seen == identity).count();
        keys.push((identity, rank));
    }
    Some(keys)
}

/// CR 603.3b + CR 732.2a: after a departure, the ordering prompt at `replay` answered with the
/// record's order of the triggers that remain, consuming the recorded order it stands for; printed
/// order where the record orders none of them.
fn related_order(
    replay: &GameState,
    end: &FrameEnd,
    items: &[PeriodItem],
    done: &mut [bool],
) -> Option<GameAction> {
    if !matches!(replay.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return None;
    }
    let keys = order_keys(replay)?;
    let related = end.related.borrow();
    let order = match related
        .iter()
        .find(|order| order.iter().any(|key| keys.contains(key)))
    {
        Some(recorded) => recorded
            .iter()
            .filter(|key| keys.contains(key))
            .chain(keys.iter().filter(|key| !recorded.contains(key)))
            .filter_map(|key| keys.iter().position(|prompted| prompted == key))
            .collect(),
        None => (0..keys.len()).collect(),
    };
    if let Some(at) = pending_answers(items, done).and_then(|mut candidates| {
        candidates
            .find(|&at| !done[at] && matches!(items[at].action, GameAction::OrderTriggers { .. }))
    }) {
        done[at] = true;
    }
    Some(GameAction::OrderTriggers { order })
}

/// How an ordered replay answers the decision prompts it meets before it stops at one.
enum Answering<'a> {
    /// These answers in turn; the replay stops at the first prompt past them.
    Path(std::collections::VecDeque<(PromptKey, PointAnswer)>),
    /// The road's answer at each prompt of the first run of prompts, or the prompt's first
    /// answer where the road has none; the replay stops at the first prompt after priority.
    Road(&'a Road),
}

/// Where an ordered replay stops: the prompts it answered, and, once it stops at a decision
/// prompt, the priority frame before that prompt, `None` when the prompt follows an answer with
/// no priority between.
struct PointStop<'a> {
    answering: Answering<'a>,
    taken: KeyedPath,
    reached: Option<Option<GameState>>,
}

impl PointStop<'_> {
    /// The answer at the decision prompt at `replay`, or `None` where the replay stops.
    fn answer(
        &mut self,
        replay: &GameState,
        after_priority: bool,
    ) -> Result<Option<(PromptKey, PointAnswer)>, OfferRefusal> {
        match &mut self.answering {
            Answering::Path(answers) => Ok(answers.pop_front()),
            Answering::Road(_) if after_priority && !self.taken.is_empty() => Ok(None),
            Answering::Road(road) => {
                let (key, answers) =
                    decision_prompt(replay, ORDER_EDGES).ok_or(OfferRefusal::UnanswerablePrompt)?;
                let answer = road
                    .iter()
                    .find(|(taken, _)| *taken == key)
                    .map(|(_, answer)| answer.clone())
                    .or_else(|| answers.into_iter().next())
                    .ok_or(OfferRefusal::UnanswerablePrompt)?;
                Ok(Some((key, answer)))
            }
        }
    }
}

/// One answer at a decision prompt: an order of its triggers, one of its options, or the
/// replacement effect applied next (CR 616.1f re-prompts for the rest).
#[derive(Debug, Clone, PartialEq)]
enum PointAnswer {
    Order(Vec<Label>),
    Option(GameAction),
    Replacement(Label),
}

/// What makes two decision prompts the same point: an ordering point's occurrences, or the
/// choice, the triggered ability asking it, and how many options it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptKey {
    Order {
        point: ChoicePoint,
        labels: Vec<Label>,
    },
    Choice {
        point: ChoicePoint,
        asker: Occurrence,
        // CR 104.4b: a covered decision point offers exactly the start's options.
        options: usize,
    },
}

/// One answer at each decision prompt of a run of prompts with no priority between.
type KeyedPath = Vec<(PromptKey, PointAnswer)>;

/// CR 601.2c + CR 608.2d: each option of the target or card choice at `replay`, as its answer.
fn choice_options(replay: &GameState) -> Vec<GameAction> {
    match &replay.waiting_for {
        WaitingFor::TriggerTargetSelection { selection, .. } => selection
            .current_legal_targets
            .iter()
            .map(|target| GameAction::ChooseTarget {
                target: Some(target.clone()),
            })
            .collect(),
        WaitingFor::RevealChoice { cards, .. } => cards
            .iter()
            .map(|&card| GameAction::SelectCards { cards: vec![card] })
            .collect(),
        _ => Vec::new(),
    }
}

/// CR 400.7: the triggered ability asking the choice at `replay`, by its source and text: the one
/// being put on the stack for a target, the one resolving for a card.
fn choice_asker(replay: &GameState) -> Option<Occurrence> {
    match &replay.waiting_for {
        WaitingFor::TriggerTargetSelection {
            source_id,
            description,
            ..
        } => {
            let source = replay.objects.get(&(*source_id)?)?;
            Some(Occurrence::of(&source.name, description.as_ref()))
        }
        _ => match &replay.resolving_stack_entry.as_ref()?.kind {
            StackEntryKind::TriggeredAbility {
                source_name,
                description,
                ..
            } => Some(Occurrence::of(source_name, description.as_ref())),
            _ => None,
        },
    }
}

/// The decision prompt at `replay`: its key and every answer; `None` past `limit` answers or
/// where the prompt cannot be keyed.
fn decision_prompt(replay: &GameState, limit: usize) -> Option<(PromptKey, Vec<PointAnswer>)> {
    let point = replay.waiting_for.option_point()?;
    match point {
        ChoicePoint::TriggerOrder => {
            let labels = point_labels(replay)?;
            if order_count(labels.len()) > limit {
                return None;
            }
            let answers = class_orders(&labels)
                .into_iter()
                .map(PointAnswer::Order)
                .collect();
            let key = PromptKey::Order {
                point,
                labels: point_key(&labels),
            };
            Some((key, answers))
        }
        ChoicePoint::AnnouncedTarget | ChoicePoint::ResolutionSet => {
            let options = choice_options(replay);
            if options.len() > limit {
                return None;
            }
            let key = PromptKey::Choice {
                point,
                asker: choice_asker(replay)?,
                options: options.len(),
            };
            Some((key, options.into_iter().map(PointAnswer::Option).collect()))
        }
        ChoicePoint::ReplacementOrder => {
            let labels = replacement_labels(replay)?;
            if labels.len() > limit {
                return None;
            }
            let answers = labels
                .iter()
                .cloned()
                .map(PointAnswer::Replacement)
                .collect();
            let key = PromptKey::Order {
                point,
                labels: point_key(&labels),
            };
            Some((key, answers))
        }
        ChoicePoint::MayGate
        | ChoicePoint::TapCost
        | ChoicePoint::ProliferateSet
        | ChoicePoint::ManaColor
        | ChoicePoint::ConvokeTaps
        | ChoicePoint::Mode
        | ChoicePoint::UnlessBreak => None,
    }
}

/// CR 616.1 + CR 400.7: the effects the replacement prompt at `replay` asks to choose among, by
/// source and description, in prompt order.
fn replacement_labels(replay: &GameState) -> Option<Vec<Label>> {
    let WaitingFor::ReplacementChoice { candidates, .. } = &replay.waiting_for else {
        return None;
    };
    Some(ranked(
        candidates
            .iter()
            .map(|candidate| Occurrence::of(&candidate.source_name, Some(&candidate.description)))
            .collect(),
    ))
}

/// The action that gives `answer` at the prompt at `replay`; an option or a replacement effect is
/// answered only by the same option or effect among the prompt's, never by its position.
fn answer_action(replay: &GameState, answer: &PointAnswer) -> Option<GameAction> {
    match answer {
        PointAnswer::Order(order) => Some(GameAction::OrderTriggers {
            order: order_indices(&point_labels(replay)?, order)?,
        }),
        PointAnswer::Option(chosen) => choice_options(replay)
            .contains(chosen)
            .then(|| chosen.clone()),
        PointAnswer::Replacement(label) => Some(GameAction::ChooseReplacement {
            index: replacement_labels(replay)?
                .iter()
                .position(|candidate| candidate == label)?,
        }),
    }
}

/// CR 603.3b + CR 601.2c + CR 608.2d: every answer sequence from the decision prompt at `prompt`
/// through the decision prompts that follow it with no priority between, each later prompt read
/// in its own branch; `None` past `limit` sequences or at a prompt that cannot be answered.
fn decision_paths(prompt: &GameState, limit: usize) -> Option<(PromptKey, Vec<KeyedPath>)> {
    let (key, answers) = decision_prompt(prompt, limit)?;
    let seat = prompt.waiting_for.acting_player()?;
    let mut paths: Vec<KeyedPath> = Vec::new();
    for answer in answers {
        let mut next = prompt.clone();
        let action = answer_action(&next, &answer)?;
        apply(&mut next, seat, action).ok()?;
        let tails = if next.waiting_for.option_point().is_some() {
            decision_paths(&next, limit)?.1
        } else {
            vec![Vec::new()]
        };
        for mut tail in tails {
            tail.insert(0, (key.clone(), answer.clone()));
            paths.push(tail);
            if paths.len() > limit {
                return None;
            }
        }
    }
    Some((key, paths))
}

/// [`replay_cycle`]; with `stop`, priority never ends the cycle and the replay stops at a
/// decision point, answering any other prompt no recorded answer takes with its forced answer.
#[allow(clippy::too_many_arguments)]
fn replay_cycle_ordered(
    replay: &mut GameState,
    items: &[PeriodItem],
    end: &FrameEnd,
    choices: &mut Vec<PinnedDecision>,
    mut frames: Option<&mut Vec<GameState>>,
    events: &mut Vec<GameEvent>,
    mut stop: Option<&mut PointStop<'_>>,
    declared: Declared<'_>,
) -> Result<Vec<String>, OfferRefusal> {
    let mut done = vec![false; items.len()];
    let mut performed = Vec::new();
    let mut last_priority: Option<GameState> = None;
    let start = play_trace::WindowKey::of(replay);
    // The stack entries no target credit may come from: those standing at the last priority
    // beat, and each entry whose targets a prompt asked for.
    let mut seen: Vec<ObjectId> = replay.stack.iter().map(|entry| entry.id).collect();
    for beat in 0..CYCLE_BEATS {
        if replay.game_end.is_some() || !end.admits(start, replay) {
            return Err(OfferRefusal::NoRecurrence);
        }
        // A trigger's targets are set once it is on the stack, so they are read at priority.
        match replay.waiting_for {
            WaitingFor::Priority { .. } => {
                if let Some(candidates) = pending_answers(items, &done) {
                    announced_declared(replay, end, &seen, items, &mut done, candidates, declared)?;
                }
                seen = replay.stack.iter().map(|entry| entry.id).collect();
            }
            // CR 603.3d: the prompted trigger is already on the stack, under construction.
            WaitingFor::TriggerTargetSelection { .. } => seen.extend(replay.pending_trigger_entry),
            _ => {}
        }
        let pending = pending_answers(items, &done);
        let next = pending.as_ref().map(|candidates| candidates.start);
        if let WaitingFor::Priority { player } = replay.waiting_for {
            if let Some(frames) = frames.as_deref_mut() {
                frames.push(replay.clone());
            }
            if stop.is_some() {
                last_priority = Some(replay.clone());
            } else if beat > 0
                && end.reached(start, replay)
                && (cycle_complete(items, &done)
                    || (end.departed(replay) && only_answers_unmade(items, &done)))
            {
                return Ok(performed);
            }
            let Some(at) = next else {
                pass(replay, player, &mut performed, events)?;
                continue;
            };
            let item = &items[at];
            if item.seat == player && item.play == Some((replay.stack.len(), PromptClass::Priority))
            {
                let action = rebound(item, replay);
                step(
                    replay,
                    player,
                    action,
                    OfferRefusal::IllegalReplayedPlay,
                    events,
                )?;
                done[at] = true;
            } else {
                pass(replay, player, &mut performed, events)?;
            }
            continue;
        }
        if let (Some(stop), Some(_)) = (stop.as_deref_mut(), replay.waiting_for.option_point()) {
            let player = replay
                .waiting_for
                .acting_player()
                .ok_or(OfferRefusal::UnanswerablePrompt)?;
            let Some((key, answer)) = stop.answer(replay, last_priority.is_some())? else {
                stop.reached = Some(last_priority.take());
                return Ok(performed);
            };
            let action = answer_action(replay, &answer).ok_or(OfferRefusal::UnanswerablePrompt)?;
            step(
                replay,
                player,
                action,
                OfferRefusal::UnanswerablePrompt,
                events,
            )?;
            stop.taken.push((key, answer));
            last_priority = None;
            continue;
        }
        let answered = match pending {
            None => false,
            Some(candidates) => {
                let at = candidates.start;
                if answer_declared(
                    replay,
                    items,
                    &mut done,
                    candidates.clone(),
                    declared,
                    events,
                )? {
                    continue;
                }
                let item = &items[at];
                if item.play == Some((replay.stack.len(), PromptClass::Other)) {
                    let action = rebound(item, replay);
                    step(
                        replay,
                        item.seat,
                        action,
                        OfferRefusal::IllegalReplayedPlay,
                        events,
                    )?;
                    done[at] = true;
                    continue;
                }
                answer(replay, end, items, &mut done, candidates, choices, events)?
            }
        };
        if !answered {
            // CR 603.3b + CR 732.2a: after a departure an order the record made answers the
            // triggers that remain.
            if let Some((order, seat)) = end
                .departed(replay)
                .then(|| related_order(replay, end, items, &mut done))
                .flatten()
                .zip(replay.waiting_for.acting_player())
            {
                step(
                    replay,
                    seat,
                    order,
                    OfferRefusal::UnanswerablePrompt,
                    events,
                )?;
                continue;
            }
            // CR 608.2d: an order the period did not record meets a choice with one option.
            let forced = stop
                .is_some()
                .then(|| replay.waiting_for.forced_one_option_answer())
                .flatten()
                .zip(replay.waiting_for.acting_player());
            let Some((action, seat)) = forced else {
                return Err(OfferRefusal::UnanswerablePrompt);
            };
            step(
                replay,
                seat,
                action,
                OfferRefusal::UnanswerablePrompt,
                events,
            )?;
        }
    }
    Err(OfferRefusal::NoRecurrence)
}

/// The unmade items from the first one up to the next unmade play: the answers the current
/// prompt may take.
fn pending_answers(items: &[PeriodItem], done: &[bool]) -> Option<std::ops::Range<usize>> {
    let from = done.iter().position(|made| !made)?;
    let to = (from..items.len())
        .find(|&at| !done[at] && items[at].play.is_some())
        .unwrap_or(items.len());
    Some(from..to)
}

/// CR 800.4a: every unmade item is an answer the remaining seats may not ask.
fn only_answers_unmade(items: &[PeriodItem], done: &[bool]) -> bool {
    items
        .iter()
        .zip(done)
        .all(|(item, made)| *made || item.play.is_none())
}

/// Every item is made, except mandatory answers the replay went past without being asked them.
fn cycle_complete(items: &[PeriodItem], done: &[bool]) -> bool {
    let last_done = done.iter().rposition(|made| *made);
    items
        .iter()
        .zip(done)
        .enumerate()
        .all(|(at, (item, made))| {
            *made || (item.mandatory_answer && last_done.is_some_and(|last| at < last))
        })
}

/// CR 117.4: `player` passes, noting the triggered ability the pass resolves.
fn pass(
    replay: &mut GameState,
    player: PlayerId,
    performed: &mut Vec<String>,
    events: &mut Vec<GameEvent>,
) -> Result<(), OfferRefusal> {
    let top = replay.stack.back().and_then(|entry| {
        play_trace::resolution_identity(replay, entry).map(|identity| (entry.id, identity))
    });
    step(
        replay,
        player,
        GameAction::PassPriority,
        OfferRefusal::UnanswerablePrompt,
        events,
    )?;
    if let Some((id, identity)) = top {
        if replay.stack.iter().all(|entry| entry.id != id) {
            performed.push(identity);
        }
    }
    Ok(())
}

/// CR 603.3d + CR 601.2c: answers a trigger's target prompt whose slot `declared` pins with the
/// declaration's targets at its repetition, in place of the recorded answer among `candidates`;
/// `false` when the declaration pins no slot of the prompt. CR 608.2b + CR 400.7: a pinned
/// target that is no longer there stops the repetition.
fn answer_declared(
    replay: &mut GameState,
    items: &[PeriodItem],
    done: &mut [bool],
    candidates: std::ops::Range<usize>,
    declared: Declared<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<bool, OfferRefusal> {
    use crate::analysis::decision_template::take_answer;
    let Some((template, iteration)) = declared else {
        return Ok(false);
    };
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id: Some(source_id),
        ..
    } = replay.waiting_for
    else {
        return Ok(false);
    };
    let Some(decision) = take_answer(
        template,
        iteration,
        replay,
        ChoicePoint::AnnouncedTarget,
        |source| crate::game::engine::slot_source_prompted(replay, source, source_id),
    ) else {
        return Ok(false);
    };
    let Some(recorded) = candidates.clone().find(|&at| {
        !done[at]
            && matches!(
                items[at].action,
                GameAction::SelectTargets { .. } | GameAction::ChooseTarget { .. }
            )
    }) else {
        return Ok(false);
    };
    step(
        replay,
        player,
        GameAction::SelectTargets {
            targets: pinned_targets(decision)?,
        },
        OfferRefusal::IllegalReplayedPlay,
        events,
    )?;
    done[recorded] = true;
    Ok(true)
}

/// CR 603.3d + CR 601.2c: each trigger put on the stack since the last priority beat with its
/// only legal target, asking no one, makes the next recorded mandatory target answer among
/// `candidates` when `declared` pins its slot and the target it got is the declaration's at this
/// repetition. CR 603.3b + CR 405.2: its controller may have stacked others of the batch above it,
/// so every entry outside `seen` is read in the order it was put on the stack.
fn announced_declared(
    replay: &GameState,
    end: &FrameEnd,
    seen: &[ObjectId],
    items: &[PeriodItem],
    done: &mut [bool],
    candidates: std::ops::Range<usize>,
    declared: Declared<'_>,
) -> Result<(), OfferRefusal> {
    use crate::analysis::decision_template::take_answer;
    let Some((template, iteration)) = declared else {
        return Ok(());
    };
    for entry in replay
        .stack
        .iter()
        .filter(|entry| !seen.contains(&entry.id))
    {
        let StackEntryKind::TriggeredAbility {
            source_id, ability, ..
        } = &entry.kind
        else {
            continue;
        };
        let Some(decision) = take_answer(
            template,
            iteration,
            replay,
            ChoicePoint::AnnouncedTarget,
            |source| crate::game::engine::slot_source_prompted(replay, source, *source_id),
        ) else {
            continue;
        };
        let Some(recorded) = candidates.clone().find(|&at| {
            !done[at]
                && items[at].mandatory_answer
                && matches!(
                    items[at].action,
                    GameAction::SelectTargets { .. } | GameAction::ChooseTarget { .. }
                )
        }) else {
            return Ok(());
        };
        // CR 603.3d + CR 601.2c: once any seat has left, a trigger's announced targets are not
        // compared with its pin, whichever seat the pin names.
        if !end.departed(replay)
            && crate::game::ability_utils::declared_targets_in_chain(ability)
                != pinned_targets(decision)?
        {
            return Err(OfferRefusal::IllegalReplayedPlay);
        }
        done[recorded] = true;
    }
    Ok(())
}

/// The targets a declared target pin names at this repetition; a pin that does not resolve here
/// refuses the replayed play.
fn pinned_targets(
    decision: Result<
        crate::analysis::decision_template::ConcreteDecision,
        crate::analysis::decision_template::ReplayFailure,
    >,
) -> Result<Vec<TargetRef>, OfferRefusal> {
    use crate::analysis::decision_template::{ConcreteDecision, ConcreteTarget};
    let ConcreteDecision::Targets { targets, .. } =
        decision.map_err(|_| OfferRefusal::IllegalReplayedPlay)?
    else {
        return Err(OfferRefusal::UnanswerablePrompt);
    };
    Ok(targets
        .into_iter()
        .map(|target| match target {
            ConcreteTarget::Object(id) => TargetRef::Object(id),
            ConcreteTarget::Player(seat) => TargetRef::Player(seat),
        })
        .collect())
}

/// CR 508.1b + CR 800.4a: `action` with each attack the record aimed at a player who has left
/// declared at the one attack target its attacker has left; with more left, which one it attacks
/// is a choice the record does not hold, so the attack stays as recorded.
fn reaimed_attack(action: GameAction, replay: &GameState) -> GameAction {
    let (
        GameAction::DeclareAttackers { attacks, bands },
        WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker: Some(by_attacker),
            ..
        },
    ) = (&action, &replay.waiting_for)
    else {
        return action;
    };
    let attacks = attacks
        .iter()
        .map(
            |&(attacker, target)| match (target, by_attacker.get(&attacker).map(Vec::as_slice)) {
                (crate::game::combat::AttackTarget::Player(seat), Some(&[only]))
                    if !crate::game::players::is_alive(replay, seat) =>
                {
                    (attacker, only)
                }
                _ => (attacker, target),
            },
        )
        .collect();
    GameAction::DeclareAttackers {
        attacks,
        bands: bands.clone(),
    }
}

/// Answers the current prompt with the first unconsumed answer among `candidates` the reducer
/// accepts; `false` when none does.
fn answer(
    replay: &mut GameState,
    end: &FrameEnd,
    items: &[PeriodItem],
    done: &mut [bool],
    candidates: std::ops::Range<usize>,
    choices: &mut Vec<PinnedDecision>,
    events: &mut Vec<GameEvent>,
) -> Result<bool, OfferRefusal> {
    for at in candidates.clone() {
        if done[at] {
            continue;
        }
        let item = &items[at];
        if matches!(item.action, GameAction::TapForConvoke { .. }) {
            let convoked = convoke_choice(replay);
            if rebind_convoke(replay, events)?.is_some() {
                record_choice(choices, convoked)?;
                for convoke in candidates.clone() {
                    if matches!(items[convoke].action, GameAction::TapForConvoke { .. }) {
                        done[convoke] = true;
                    }
                }
                return Ok(true);
            }
            continue;
        }
        let chosen = item
            .cost_move
            .as_ref()
            .map_or(&[][..], |recorded| recorded.chosen.as_slice());
        let action = rebind_cost_selection(&item.action, rebound(item, replay), chosen, replay);
        let action = if end.departed(replay) {
            reaimed_attack(action, replay)
        } else {
            action
        };
        let offered = item
            .cost_move
            .as_ref()
            .and_then(|_| CostChoices::at(replay, &action));
        let choice = answered_choice(replay, &action);
        // CR 603.3b: the run keeps the record's order by the triggers it ordered.
        let ordered: Option<Vec<OrderKey>> = match &action {
            GameAction::OrderTriggers { order } => order_keys(replay)
                .map(|keys| order.iter().filter_map(|&i| keys.get(i).cloned()).collect()),
            _ => None,
        };
        match step(
            replay,
            item.seat,
            action,
            OfferRefusal::UnanswerablePrompt,
            events,
        ) {
            Ok(()) => {}
            Err(OfferRefusal::UnanswerablePrompt) => continue,
            Err(refusal) => return Err(refusal),
        }
        done[at] = true;
        if let Some(ordered) = ordered {
            let mut related = end.related.borrow_mut();
            if !related.contains(&ordered) {
                related.push(ordered);
            }
        }
        record_choice(choices, choice)?;
        if let (Some(recorded), Some(offered)) = (&item.cost_move, offered) {
            if offered.moved(replay).arrivals != recorded.arrivals {
                return Err(OfferRefusal::ArrivalDiverged);
            }
        }
        return Ok(true);
    }
    // CR 601.2h + CR 702.51a: a recorded payment the live board cannot repeat is made with the
    // creatures it can tap instead, as a recorded convoke is.
    let convoked = convoke_choice(replay);
    if rebind_convoke(replay, events)?.is_some_and(|tapped| tapped > 0) {
        record_choice(choices, convoked)?;
        return Ok(true);
    }
    Ok(false)
}

/// CR 732.2a: records `choice` as the next occurrence of its source and point, so each time a
/// cycle asks one source the same choice is a slot a declaration can name.
fn record_choice(
    choices: &mut Vec<PinnedDecision>,
    choice: Option<PinnedDecision>,
) -> Result<(), OfferRefusal> {
    if let Some(choice) = choice {
        let choice = choice
            .at_occurrence_among(choices)
            .ok_or(OfferRefusal::UnanswerablePrompt)?;
        choices.push(choice);
    }
    Ok(())
}

/// CR 702.51a: the convoke a replayed cast is about to pay, by the card cast.
fn convoke_choice(replay: &GameState) -> Option<PinnedDecision> {
    let pending = replay.pending_cast.as_ref()?;
    let card_id = replay.objects.get(&pending.object_id)?.card_id;
    Some(PinnedDecision::ConvokeTaps {
        slot: DecisionSlot::first(
            YieldTarget::AllCopies {
                card_id,
                trigger_description: None,
            },
            ChoicePoint::ConvokeTaps,
        ),
    })
}

/// CR 732.2a: the game choice `action` makes at the prompt in hand, as a declaration names it: a
/// target (CR 601.2c, CR 603.3d), a cost's tapped creatures (CR 601.2h), a mana color (CR 605.3a),
/// a "may" (CR 603.5), a proliferate set (CR 701.34a), or a set chosen at resolution (CR 608.2d);
/// `None` for any other answer.
fn answered_choice(replay: &GameState, action: &GameAction) -> Option<PinnedDecision> {
    let source = |id: ObjectId| object_decision_source(replay, id);
    let identities = |ids: &[ObjectId]| {
        ids.iter()
            .map(|id| source(*id).map(TargetPin::ByIdentity))
            .collect::<Option<Vec<_>>>()
            .filter(|pins| !pins.is_empty())
    };
    let set = |slot_source: ObjectId, point: ChoicePoint, targets: Option<Vec<TargetPin>>| {
        Some(PinnedDecision::Targets {
            slot: DecisionSlot::first(source(slot_source)?, point),
            targets: targets?,
        })
    };
    let announced = |slot_source: ObjectId, chosen: &[TargetRef]| {
        let targets = announced_target_pins(replay, chosen).filter(|pins| !pins.is_empty());
        set(slot_source, ChoicePoint::AnnouncedTarget, targets)
    };
    match (&replay.waiting_for, action) {
        (
            WaitingFor::TriggerTargetSelection {
                source_id: Some(slot_source),
                ..
            },
            GameAction::ChooseTarget { target },
        ) => announced(*slot_source, target.as_slice()),
        (
            WaitingFor::TriggerTargetSelection {
                source_id: Some(slot_source),
                ..
            },
            GameAction::SelectTargets { targets },
        ) => announced(*slot_source, targets),
        (WaitingFor::TargetSelection { pending_cast, .. }, GameAction::ChooseTarget { target }) => {
            announced(pending_cast.object_id, target.as_slice())
        }
        (
            WaitingFor::TargetSelection { pending_cast, .. },
            GameAction::SelectTargets { targets },
        ) => announced(pending_cast.object_id, targets),
        (
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                resume: CostResume::ManaAbility { mana_ability },
                ..
            },
            GameAction::SelectCards { cards },
        ) => set(
            mana_ability.source_id,
            ChoicePoint::TapCost,
            identities(cards),
        ),
        (
            WaitingFor::EffectZoneChoice {
                source_id,
                is_cost_payment: false,
                ..
            },
            GameAction::SelectCards { cards },
        ) => set(*source_id, ChoicePoint::ResolutionSet, identities(cards)),
        (WaitingFor::ProliferateChoice { .. }, GameAction::SelectTargets { targets }) => set(
            replay.active_proliferate_frame()?.source_id,
            ChoicePoint::ProliferateSet,
            Some(proliferate_pins(replay, targets)).filter(|pins| !pins.is_empty()),
        ),
        (
            WaitingFor::OptionalEffectChoice { source_id, .. },
            GameAction::DecideOptionalEffect { accept },
        ) => Some(PinnedDecision::MayChoice {
            slot: DecisionSlot::first(source(*source_id)?, ChoicePoint::MayGate),
            take: if *accept {
                MayChoiceOption::Take
            } else {
                MayChoiceOption::Decline
            },
        }),
        (
            WaitingFor::ChooseManaColor {
                context: ManaChoiceContext::ManaAbility(pending),
                ..
            },
            GameAction::ChooseManaColor { choice, .. },
        ) => Some(PinnedDecision::ManaColor {
            slot: DecisionSlot::first(source(pending.source_id)?, ChoicePoint::ManaColor),
            color: pinnable_mana_color(choice)?,
        }),
        _ => None,
    }
}

/// The objects of `state` the period's plays cast: each cast card, and for a prepared copy cast,
/// the copy linked to its prepared permanent and the copy it cast, if that is still on the stack.
///
/// CR 722.3c + CR 400.7: each time the permanent becomes prepared its controller creates a new
/// copy of its prepare spell in exile, so a period that casts one and re-prepares the permanent
/// holds a different copy in each frame. The exiled copy is stripped only while its permanent is
/// prepared on the battlefield, since only then does CR 722.3c make it a function of that
/// permanent, which the frames still compare. CR 601.2a: the cast moves the copy to the stack,
/// where a frame taken before it resolves still holds it.
fn period_casts(state: &GameState, items: &[PeriodItem]) -> Vec<ObjectId> {
    use crate::game::effects::prepare::{
        is_retained_linked_prepared_copy, linked_prepared_copy_source,
    };
    let mut casts = Vec::new();
    for item in items {
        match item.action {
            GameAction::CastSpell { object_id, .. } => casts.push(object_id),
            GameAction::CastPreparedCopy { source } => casts.extend(
                state
                    .objects
                    .values()
                    .filter(|object| {
                        let linked = linked_prepared_copy_source(object) == Some(source)
                            && is_retained_linked_prepared_copy(state, object);
                        let cast = object.zone == crate::types::zones::Zone::Stack
                            && object.prepared_copy_source == Some(source);
                        linked || cast
                    })
                    .map(|object| object.id),
            ),
            _ => {}
        }
    }
    casts
}

/// The frame with every object the period cast removed, since a cast card returns where it
/// came from (CR 400.7), and its churning bookkeeping cleared.
pub(crate) fn normalize_cast_frame(state: &GameState, casts: &[ObjectId]) -> GameState {
    let mut normalized = frame_without(state, casts);
    clear_frame_bookkeeping(&mut normalized);
    normalized
}

/// [`normalize_cast_frame`] with the casts read from `state` itself, since a prepared copy cast
/// strips a different copy in each frame, and what `written` names stripped too: an object a
/// random outcome created, and a counter it changed (CR 705.1, CR 706.1).
fn normalize_period_frame(state: &GameState, items: &[PeriodItem], written: &Written) -> GameState {
    let mut stripped = period_casts(state, items);
    stripped.extend(written.created.iter().copied());
    let mut normalized = normalize_cast_frame(state, &stripped);
    for (id, counter) in &written.countered {
        if let Some(object) = normalized.objects.get_mut(id) {
            object.counters.remove(counter);
        }
    }
    normalized
}

/// A span replayed twice from a priority frame: the stage the confirmer and the
/// mandatory-recurrence witness share.
struct ReplayedPeriod {
    holder: PlayerId,
    items: Vec<PeriodItem>,
    performance: Option<PeriodPerformance>,
    library_cost: Option<PlayLocus>,
    end: FrameEnd,
    choices: Vec<PinnedDecision>,
    #[cfg(any(test, feature = "test-support"))]
    performed: Vec<String>,
    s_n: GameState,
    s_n1: GameState,
    /// The frame the second cycle reached, or where it was refused.
    replay: GameState,
    second: CycleEnd,
    /// The outcome position each compared cycle starts from, when the replay drew an outcome.
    pinned: Option<u128>,
    /// Every frame above is read inside the probe.
    _probe: SimulationProbeGuard,
}

/// CR 117.3d + CR 732.2a: the frame a replay starts from, `holder`'s view with the standing-pass
/// overlays cleared, since the replay takes only the period's own passes.
fn replay_base(frame: &GameState, holder: PlayerId) -> GameState {
    without_standing_passes(crate::game::visibility::proposer_hidden_view(frame, holder))
}

/// CR 117.3d: `state` with its standing-pass overlays cleared.
fn without_standing_passes(mut state: GameState) -> GameState {
    state.auto_pass.clear();
    state.stack_resolution_session = None;
    state
}

/// CR 405.5 + CR 608.1 + CR 400.7: the bottom of `frame`'s stack that `next` keeps unchanged,
/// where an entry is its id and the incarnation of the object at it.
fn stack_floor(frame: &GameState, next: &GameState) -> usize {
    let key = |state: &GameState, entry: &crate::types::game_state::StackEntry| {
        let incarnation = state
            .objects
            .get(&entry.id)
            .map(|object| object.incarnation);
        (entry.id, incarnation)
    };
    frame
        .stack
        .iter()
        .zip(next.stack.iter())
        .take_while(|(a, b)| key(frame, a) == key(next, b))
        .count()
}

/// CR 405.5 + CR 608.1: floors are measured on the frames the cover compares, and a refused cycle
/// keeps nothing.
fn period_floors(
    frames: [&GameState; 3],
    next: Option<&GameState>,
    normalize: impl Fn(&GameState) -> GameState,
) -> [usize; 3] {
    let [a, b, c] = frames.map(&normalize);
    let closing = next.map_or(0, |next| stack_floor(&c, &normalize(next)));
    [stack_floor(&a, &b), stack_floor(&b, &c), closing]
}

impl ReplayedPeriod {
    /// Each replayed frame's floor; the closing frame's needs a third cycle.
    fn floors(&self) -> [usize; 3] {
        let mut next = self.replay.clone();
        if let Some(at) = self.pinned {
            next.rng.set_word_pos(at);
        }
        let third = self.end.repeat(
            &mut next,
            &self.items,
            &mut Vec::new(),
            None,
            &mut Vec::new(),
            None,
        );
        // CR 704.3 + CR 104.2a: a third cycle its holder won in closes the floors as one that came
        // round does.
        let closes = matches!(third, CycleEnd::CameRound | CycleEnd::HolderWon(_));
        period_floors(
            [&self.s_n, &self.s_n1, &self.replay],
            closes.then_some(&next),
            |state| normalize_period_frame(state, &self.items, &Written::NONE),
        )
    }
}

/// CR 732.2a: replays the span `span` of the current trace twice from `frame`, a priority frame;
/// the first cycle's refusal refuses, and the second's is kept for the reader.
fn replay_period(frame: &GameState, span: NamedSpan) -> Result<ReplayedPeriod, OfferRefusal> {
    let WaitingFor::Priority { player: holder } = frame.waiting_for else {
        return Err(OfferRefusal::UnanswerablePrompt);
    };
    let Some(entries) = play_trace::span_entries(frame, span) else {
        return Err(OfferRefusal::NoRecurrence);
    };
    let body: Vec<TraceEntry> = entries
        .iter()
        .skip(span.start)
        .take(span.end - span.start)
        .cloned()
        .collect();
    if let Some(refusal) = seat_refusal(&body, holder) {
        return Err(refusal);
    }
    let items = period_items(entries, span, frame);
    let performance = Some(PeriodPerformance::of(&body));
    let library_cost = library_cost_play(&body, frame);
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_play_trace(|counters| counters.confirm_drives += 1);
    let probe = SimulationProbeGuard::enter();
    let mut s_n = replay_base(frame, holder);
    let end = FrameEnd::of(&s_n, holder, span.reach);
    let mut replay = s_n.clone();
    let mut choices = Vec::new();
    let lead_in = replay_cycle_ordered(
        &mut replay,
        &items,
        &end,
        &mut choices,
        None,
        &mut Vec::new(),
        None,
        None,
    )?;
    // CR 705.1 + CR 706.1 + CR 400.7: cycles that drew different outcomes differ in what those
    // outcomes made, so the compared cycles start after a lead-in, under one outcome sequence.
    let pinned = (outcomes_admitted() && replay.rng.outcome_draws() != s_n.rng.outcome_draws())
        .then(|| s_n.rng.get_word_pos());
    #[cfg_attr(not(any(test, feature = "test-support")), allow(unused_variables))]
    let performed = match pinned {
        None => lead_in,
        Some(at) => {
            s_n = replay;
            s_n.rng.set_word_pos(at);
            replay = s_n.clone();
            choices.clear();
            replay_cycle_ordered(
                &mut replay,
                &items,
                &end,
                &mut choices,
                None,
                &mut Vec::new(),
                None,
                None,
            )?
        }
    };
    let s_n1 = replay.clone();
    // CR 800.4a: a seat that left in the frame's first repetition leaves a board whose
    // repetitions are not the period the compared cycles repeat.
    if end.departed(&s_n1) {
        return Err(OfferRefusal::NoRecurrence);
    }
    if let Some(at) = pinned {
        replay.rng.set_word_pos(at);
    }
    let second = end.repeat(
        &mut replay,
        &items,
        &mut Vec::new(),
        None,
        &mut Vec::new(),
        None,
    );
    Ok(ReplayedPeriod {
        holder,
        items,
        performance,
        library_cost,
        end,
        choices,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        s_n,
        s_n1,
        replay,
        second,
        pinned,
        _probe: probe,
    })
}

/// CR 732.2a: confirms the span `span` of the current trace as a period `frame`'s priority
/// holder may propose to repeat.
pub(crate) fn confirm(frame: &GameState, span: NamedSpan) -> Result<Confirmation, OfferRefusal> {
    let replayed = replay_period(frame, span)?;
    // CR 405.5 + CR 608.1: the cover reads the floors the replayed frames keep.
    let floors = replayed.floors();
    let ReplayedPeriod {
        holder,
        items,
        performance,
        library_cost,
        end,
        choices,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        s_n,
        s_n1,
        mut replay,
        second,
        pinned: _,
        _probe,
    } = replayed;
    // A period whose second cycle did not come round has one pair of frames, read as any pair is;
    // CR 800.4a: so has one a seat left in, whose pair spans a board change.
    let (prior, current) = if second == CycleEnd::CameRound && !end.departed(&replay) {
        (&s_n1, &replay)
    } else {
        (&s_n, &s_n1)
    };
    let (verdict, growth) = certify_object_growth_frames(
        [&s_n, &s_n1, current],
        |state| normalize_period_frame(state, &items, &Written::NONE),
        holder,
        Some(floors),
    );
    // CR 732.2a: one count per offer. Something of its proposer's own falling makes the period
    // the counting replay's; any other period with a loss axis is the threshold authority's.
    // A spend verdict asks the sign check only for its refusals.
    let sign = match (&verdict, verdict.certifies()) {
        (ObjectGrowthVerdict::Spend, _) => {
            period_sign_check(prior, current, holder, library_cost).map(|_| PeriodSign::Spend)
        }
        (
            ObjectGrowthVerdict::FodderGrowth(_) | ObjectGrowthVerdict::ResourceRecurrence(_),
            true,
        ) => period_sign_check(prior, current, holder, library_cost),
        (
            ObjectGrowthVerdict::FodderGrowth(_) | ObjectGrowthVerdict::ResourceRecurrence(_),
            false,
        ) => Err(OfferRefusal::Cover(verdict.clone())),
    };
    let sign = match sign {
        // CR 701.17b: the first cycle milled what the library held; the second cycle's empty mill
        // is the library running out, not the period.
        Err(OfferRefusal::NoAxis)
            if second == CycleEnd::CameRound
                && certify_instructed_library_departure(&s_n, &s_n1, [&s_n, &s_n1])
                    .is_some_and(|certified| {
                        certified.per_victim.keys().any(|&seat| {
                            s_n1.players
                                .iter()
                                .any(|player| player.id == seat && player.library.is_empty())
                        })
                    }) =>
        {
            period_sign_check(&s_n, &s_n1, holder, library_cost)
        }
        // CR 603.5 + CR 104.4b: a period of answers alone, one of them optional, is no mandatory
        // loop, so it is a finite proposal with its answers pinned (CR 732.2a) that each other
        // player may shorten (CR 732.2b).
        Err(OfferRefusal::NoAxis)
            if play_trace::span_entries(frame, span).is_some_and(|entries| {
                let body = || entries.iter().skip(span.start).take(span.end - span.start);
                !body().any(|entry| matches!(entry.kind, EntryKind::Play { .. }))
                    && body().any(optional_choice)
            }) =>
        {
            Ok(PeriodSign::Unbounded(Box::new((
                ResourceVector::period(prior, current),
                None,
            ))))
        }
        sign => sign,
    };
    // CR 800.4a + CR 732.2a: a loss period a seat left in is the threshold authority's, read on its
    // first cycle alone; its second cycle stopped at a board the record does not answer, such as a
    // target announced among the remaining seats (CR 603.3d + CR 601.2c).
    let loss_departed = matches!(
        (&sign, &second),
        (Ok(PeriodSign::LossAxis(_)), CycleEnd::Departed(_))
    );
    // CR 104.2a + CR 732.2a: a second cycle the game ended in with its holder the winner was
    // performed, so the counting replay counts the period.
    let sign = match (sign, &second) {
        (Ok(PeriodSign::Unbounded(_) | PeriodSign::LossAxis(_)), CycleEnd::HolderWon(_)) => {
            Ok(PeriodSign::Spend)
        }
        // CR 732.2a: so was a second cycle stopped where the count ends, a repetition the counting
        // replay counts but does not offer; that cycle's pair compares a frame with itself, so the
        // cover is read on the first pair alone.
        (
            Err(OfferRefusal::Cover(_)) | Ok(PeriodSign::Unbounded(_) | PeriodSign::LossAxis(_)),
            CycleEnd::HolderLeft(_) | CycleEnd::HolderCleanup(_) | CycleEnd::Departed(_),
        ) if first_pair_certifies(&verdict) && !loss_departed => {
            period_sign_check(&s_n, &s_n1, holder, library_cost).map(|_| PeriodSign::Spend)
        }
        (sign, _) => sign,
    };
    // Only a spend, or a loss period a seat left in, reads past a period that stopped after one
    // repetition.
    if let (Ok(PeriodSign::Unbounded(_) | PeriodSign::LossAxis(_)) | Err(_), Some(stopped)) =
        (&sign, second.refusal().filter(|_| !loss_departed))
    {
        return Err(stopped.clone());
    }
    let mut performed_reaches = Vec::new();
    let mut recorded = None;
    let (delta, departure, bounded, growth, counted_by) = match (sign, second) {
        (Err(refusal), _) => return Err(refusal),
        (Ok(PeriodSign::Unbounded(_)), _)
            if verdict
                == ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::GrowthRead)) =>
        {
            return Err(OfferRefusal::GrowingLoss);
        }
        (Ok(PeriodSign::Unbounded(unbounded)), _) => {
            let (delta, departure) = *unbounded;
            (delta, departure, None, growth, CountedBy::Threshold)
        }
        // CR 704.5a + CR 732.2a: a loss period may be repeated only up to the threshold crossings
        // the authority measures on its replayed frames, and is offered at the count its
        // published declaration reaches when performed.
        (Ok(PeriodSign::LossAxis(unbounded)), _) => {
            let cycle_frames = |from: &GameState| {
                let mut frames = Vec::new();
                replay_cycle(
                    &mut from.clone(),
                    &items,
                    &end,
                    &mut Vec::new(),
                    Some(&mut frames),
                    &mut Vec::new(),
                )
                .map(|_| frames)
            };
            let second_frames = if loss_departed {
                None
            } else {
                Some(cycle_frames(&s_n1)?)
            };
            let first_frames = cycle_frames(&s_n)?;
            let repetition_growth = match &second_frames {
                Some(second) => crate::analysis::resource::repetition_growth(
                    &first_frames.iter().collect::<Vec<_>>(),
                    &second.iter().collect::<Vec<_>>(),
                ),
                None => Default::default(),
            };
            let grows = !repetition_growth.0.is_zero() || !repetition_growth.1.is_empty();
            // CR 732.2a: the two replayed repetitions charge differently, and only a cover that
            // certified the growth (CR 613.4c) makes that difference repeat.
            if grows
                && verdict
                    != ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::GrowthRead))
            {
                return Err(OfferRefusal::GrowingLoss);
            }
            let frames = match second_frames {
                Some(second_frames) if !grows => second_frames,
                _ => first_frames,
            };
            // CR 514.1: a turn-cycle period's count is bounded by its cleanup discard.
            let cleanup = (verdict
                == ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::TurnCycle)))
            .then(|| CleanupPair::measure(&frames, holder))
            .flatten();
            let takes = if grows {
                Vec::new()
            } else {
                measure_seat_takes(frame, holder, &s_n, &items, &end, &frames, &choices)
            };
            match replay_bounded_offer(
                frame,
                holder,
                &frames,
                &choices,
                cleanup,
                repetition_growth,
                takes,
            )? {
                Some(mut parts) => {
                    (performed_reaches, recorded) =
                        perform_published(&mut parts, &s_n, &items, span.reach, holder);
                    let delta = ResourceVector::period(prior, current);
                    (delta, None, Some(parts), growth, CountedBy::Threshold)
                }
                // CR 732.2a: the unbounded road states no growth.
                None if grows => return Err(OfferRefusal::GrowingLoss),
                // No threshold: the unbounded road's.
                None => {
                    let (delta, departure) = *unbounded;
                    (delta, departure, None, growth, CountedBy::Threshold)
                }
            }
        }
        // CR 732.2a + CR 118.3: the count is the repetitions the reducer performs, and the offer
        // states no other.
        (Ok(PeriodSign::Spend), second) => {
            let points = pinned_decisions_to_points(&choices, frame, holder)
                .ok_or(OfferRefusal::UnanswerablePrompt)?;
            let mut frames = Vec::new();
            replay_cycle(
                &mut prior.clone(),
                &items,
                &end,
                &mut Vec::new(),
                Some(&mut frames),
                &mut Vec::new(),
            )?;
            let delta = ResourceVector::period(prior, current);
            let pair = Box::new([prior.clone(), current.clone()]);
            let mut count = Count::default();
            count.note(seats(&s_n), &s_n1, &CycleEnd::CameRound);
            let mut last = second;
            let mut repeats = count.note(seats(&s_n1), &replay, &last);
            while repeats && count.repetitions < COUNT_CAP {
                let before = seats(&replay);
                last = end.repeat(
                    &mut replay,
                    &items,
                    &mut Vec::new(),
                    None,
                    &mut Vec::new(),
                    None,
                );
                repeats = count.note(before, &replay, &last);
            }
            let parts = counted_offer(
                &frames,
                points,
                count.survived(holder),
                count.repetitions,
                counted_ending(holder, &s_n, &replay, &last).map(|kind| (kind, holder)),
            );
            let departures = count.departures;
            return Ok(Confirmation {
                period: ConfirmedPeriod {
                    items,
                    growth: CoveredGrowth::PerformedOnly,
                    reach: span.reach,
                    choices,
                    counted_by: CountedBy::Replay { departures },
                    performance,
                    performed: Vec::new(),
                    recorded: None,
                    outcomes: Outcomes::Refused,
                },
                frames: pair,
                delta,
                departure: None,
                #[cfg(any(test, feature = "test-support"))]
                performed,
                bounded: Some(parts),
            });
        }
    };
    Ok(Confirmation {
        period: ConfirmedPeriod {
            items,
            growth,
            reach: span.reach,
            choices,
            counted_by,
            performance,
            performed: performed_reaches,
            recorded,
            outcomes: Outcomes::Refused,
        },
        frames: Box::new([s_n1, replay]),
        delta,
        departure,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        bounded,
    })
}

/// CR 732.2a: whether the cover certified the first of a verdict's two frame pairs; only a
/// fodder-growth verdict reports its pairs apart.
fn first_pair_certifies(verdict: &ObjectGrowthVerdict) -> bool {
    match verdict {
        ObjectGrowthVerdict::FodderGrowth([first, _]) => first.is_empty(),
        verdict => verdict.certifies(),
    }
}

/// CR 104.4b + CR 732.4: whether the mandatory span `span`, replayed twice from `frame`, recurs
/// for every living player, with net progress or none, and nothing moving toward a state-based
/// action: no loss axis (CR 704.5c), no driving resource falling on a carried permanent and no
/// marked damage growing (CR 704.5g, CR 704.5i, CR 704.5q), and no -1/-1 counter growth
/// (CR 704.5f). A player counter nothing in the loop reads is no way to stop it (CR 122.1), so
/// no unmeasured axis bars the period. A period that holds is returned as its take performs it
/// (CR 732.2a), keeping the outcomes its replay kept.
pub(crate) fn mandatory_period_holds(
    frame: &GameState,
    span: NamedSpan,
) -> Option<ConfirmedPeriod> {
    let _outcomes = OutcomesAdmitted::enter();
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_play_trace(|counters| counters.mandatory_replays += 1);
    let Ok(
        period @ ReplayedPeriod {
            second: CycleEnd::CameRound,
            ..
        },
    ) = replay_period(frame, span)
    else {
        return None;
    };
    let floors = period.floors();
    let ReplayedPeriod {
        items,
        performance,
        choices,
        s_n,
        s_n1,
        replay: s_n2,
        _probe,
        ..
    } = period;
    let living = seats(frame);
    let mut buried_growth = false;
    let covered = living.iter().all(|&seat| {
        let verdict = certify_object_growth_frames(
            [&s_n, &s_n1, &s_n2],
            |state| normalize_period_frame(state, &items, &Written::NONE),
            seat,
            Some(floors),
        )
        .0;
        buried_growth |=
            verdict == ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::StackGrowth));
        verdict.certifies()
    });
    if !covered {
        return None;
    }
    let delta = period_delta(&s_n1, &s_n2);
    // CR 104.4b + CR 405.5: entries the period buries are its unbounded axis.
    let buried_rises = buried_growth && floors[2] > floors[1];
    // CR 104.4b: a period that recurs exactly is a sequence of events with no way to stop.
    let holds = (period_recurs_without_end(&delta, buried_rises)
        || delta.axis_components().is_empty())
        && living
            .iter()
            .all(|&seat| driving_resources_non_decreasing_carried(&s_n1, &s_n2, seat, Some(&s_n)))
        && delta
            .counters
            .get(&(CounterClass::Minus1Minus1, ObjectClass::Creature))
            .is_none_or(|&grew| grew <= 0);
    holds.then(|| ConfirmedPeriod {
        items,
        growth: CoveredGrowth::PerformedOnly,
        reach: span.reach,
        choices,
        counted_by: CountedBy::Threshold,
        performance,
        performed: Vec::new(),
        recorded: None,
        outcomes: Outcomes::Admitted,
    })
}

/// What a mandatory span's authoritative replay shows: its decision profile, the resources and
/// board one cycle moves, and the single player it drives every other living player's loss for.
pub(crate) struct MandatoryReading {
    pub(crate) profile: crate::types::game_state::LoopDecisionProfile,
    pub(crate) delta: ResourceVector,
    /// The board one cycle moves; empty when the replay kept a random outcome (CR 705.1, CR 706.1)
    /// no certificate covers.
    pub(crate) residual: crate::analysis::resource::BoardDelta,
    pub(crate) winner: Option<PlayerId>,
    pub(crate) certificate: Option<OutcomeCertificate>,
}

/// CR 104.4b + CR 732.5: the mandatory span `span` replayed for two cycles from `frame` itself,
/// not from its holder's view: the engine, not a player, declares the draw or the win, and an out
/// in any player's hand that is legal only mid-period is a way to stop the loop.
pub(crate) fn mandatory_reading(frame: &GameState, span: NamedSpan) -> Option<MandatoryReading> {
    use crate::types::game_state::{LoopDecision, LoopDecisionProfile, PriorityOut};
    let WaitingFor::Priority { player: holder } = frame.waiting_for else {
        return None;
    };
    let items = period_items(play_trace::span_entries(frame, span)?, span, frame);
    let out = PriorityOut::at(frame);
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_play_trace(|counters| counters.confirm_drives += 1);
    let _probe = SimulationProbeGuard::enter();
    let outcomes = OutcomesAdmitted::enter();
    outcomes.follow_writes();
    let mut replay = without_standing_passes(frame.clone());
    let end = FrameEnd::of(&replay, holder, span.reach);
    replay_cycle(
        &mut replay,
        &items,
        &end,
        &mut Vec::new(),
        None,
        &mut Vec::new(),
    )
    .ok()?;
    let s1 = replay.clone();
    let mut frames = Vec::new();
    let mut events = Vec::new();
    outcomes.second_cycle();
    let second = end.repeat(
        &mut replay,
        &items,
        &mut Vec::new(),
        Some(&mut frames),
        &mut events,
        None,
    );
    // CR 104.2a + CR 704.5c + CR 800.4a: a second cycle performed without coming round (its
    // holder won or left in it, or another seat left) is read as far as it went.
    if !second.performed() {
        return None;
    }
    let s2 = replay;
    let certificate = outcomes.certificate(&s2, &events);
    let mut profile = LoopDecisionProfile::of_window(
        [&s1.loop_decision_marks_with(out)],
        &s2.loop_decision_marks_with(out),
    );
    // CR 732.5: an out held at any frame of the period, read as `of_window` reads one.
    if !profile.members.contains(&LoopDecision::Optional)
        && frames
            .iter()
            .any(|frame| PriorityOut::at(frame) == PriorityOut::Held)
    {
        profile.members.insert(LoopDecision::Outs);
    }
    // CR 705.1 + CR 706.1: a replay that kept an outcome saw one outcome of many, so nothing it
    // read is forced or published, unless the certificate shows no outcome can stop the period.
    let kept = outcomes.kept() && certificate.is_none();
    if kept {
        profile.members.insert(LoopDecision::Random);
    } else if certificate.is_some() {
        profile.members.remove(&LoopDecision::Random);
    }
    let delta = period_delta(&s1, &s2);
    let fallers: Vec<PlayerId> = seats(&s2)
        .into_iter()
        .filter(|seat| delta.life.get(seat).copied().unwrap_or(0) < 0)
        .collect();
    let read: Vec<&GameState> = frames.iter().collect();
    // CR 704.5a + CR 104.2a: the winner never dips, and two or more fallers cross lethal together.
    let winner = crate::analysis::loop_check::live_mandatory_loop_winner(&s1, &s2, &delta).filter(
        |&winner| {
            crate::analysis::loop_check::winner_life_never_dips(&read, winner)
                && (fallers.len() < 2
                    || crate::analysis::loop_check::fallers_lives_pairwise_equal(&read, &fallers))
        },
    );
    let residual = match &certificate {
        Some(certificate) => crate::analysis::resource::board_delta(
            &normalize_period_frame(&s1, &items, &certificate.written),
            &normalize_period_frame(&s2, &items, &certificate.written),
        ),
        None if kept => crate::analysis::resource::BoardDelta::default(),
        None => crate::analysis::resource::board_delta(&s1, &s2),
    };
    Some(MandatoryReading {
        profile,
        residual,
        delta,
        winner,
        certificate,
    })
}

/// Whether a replay admitted random outcomes, which keys an every-option proof's memo slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Outcomes {
    #[default]
    Refused,
    Admitted,
}

impl Outcomes {
    fn is_refused(&self) -> bool {
        *self == Outcomes::Refused
    }
}

/// CR 603.3b + CR 732.2a: the every-option proof of the mandatory span `span` for the period read
/// as `profile`, read from the trace when kept. A random profile is never forced, so only its
/// proof, or one under `certificate`, may keep an outcome a replay draws.
pub(crate) fn span_option_proof(
    state: &GameState,
    span: NamedSpan,
    profile: &crate::types::game_state::LoopDecisionProfile,
    certificate: Option<&OutcomeCertificate>,
) -> (Outcomes, OptionProof) {
    let outcomes = if certificate.is_some()
        || profile
            .members
            .contains(&crate::types::game_state::LoopDecision::Random)
    {
        Outcomes::Admitted
    } else {
        Outcomes::Refused
    };
    let proof = play_trace::order_proof(state, span.start, outcomes).unwrap_or_else(|| {
        let _admitted = (outcomes == Outcomes::Admitted).then(OutcomesAdmitted::enter);
        decision_points(state, span, certificate)
    });
    (outcomes, proof)
}

/// The resources a period from `before` to `after` moved, counting each token it mints.
pub(crate) fn period_delta(before: &GameState, after: &GameState) -> ResourceVector {
    let mut delta = ResourceVector::delta(
        &ResourceVector::snapshot(before),
        &ResourceVector::snapshot(after),
    );
    delta.tokens_created += minted_battlefield_ids(before, after).len() as i64;
    delta
}

/// CR 104.4b + CR 704.5a + CR 704.5b + CR 704.5c: a period recurs without end only with net
/// progress or buried entries, and no loss axis.
pub(crate) fn period_recurs_without_end(delta: &ResourceVector, buried_rises: bool) -> bool {
    (delta.is_net_progress() || buried_rises) && has_no_loss_axis(delta)
}

/// CR 603.3b: what the every-option proof found of a period's decision points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionProof {
    /// Under every sequence of answers the loop comes round to a frame the closure covers.
    Continues,
    /// Under some sequence of answers the loop ends: the game ends or priority settles on an
    /// empty stack.
    Ends,
    /// Some sequence neither comes round nor ends within the budget.
    Undecided,
}

/// Why a closure edge or cover check has no frame: the loop ended, or the replay could not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Ends,
    Undecided,
}

/// CR 104.4b: a refused ordered replay ended the loop only when the game ended or priority
/// settled on an empty stack; any other refusal leaves the point a decision.
fn halt(replay: &GameState, refusal: &OfferRefusal) -> Halt {
    if replay.game_end.is_some()
        || (matches!(refusal, OfferRefusal::NoRecurrence) && replay.stack.is_empty())
    {
        Halt::Ends
    } else {
        Halt::Undecided
    }
}

/// The most closure edges the proof makes, an edge being one answer path replayed at one point;
/// with at most `1 + 2 * ONWARD_POINTS` replays per edge, the replays are linear in the budget.
const ORDER_EDGES: usize = 25;

/// The most ordering points a cover check replays, per onward frame, to come back to a node's
/// point; an edge costs at most `1 + 2 * ONWARD_POINTS` replays.
const ONWARD_POINTS: usize = 8;

/// The number of orders of `labels` labels, saturating rather than overflowing.
fn order_count(labels: usize) -> usize {
    (1..=labels)
        .try_fold(1usize, usize::checked_mul)
        .unwrap_or(usize::MAX)
}

/// Every order of the labels `labels`.
fn class_orders(labels: &[Label]) -> Vec<Vec<Label>> {
    if labels.is_empty() {
        return vec![Vec::new()];
    }
    let mut out: Vec<Vec<Label>> = Vec::new();
    for at in 0..labels.len() {
        let mut rest = labels.to_vec();
        let first = rest.remove(at);
        for mut tail in class_orders(&rest) {
            tail.insert(0, first.clone());
            out.push(tail);
        }
    }
    out
}

/// CR 603.3b + CR 104.4b: closes the frames reachable from `root` under every answer at every
/// decision point: each node is expanded once under each of its answers, and a successor no seen
/// node with its key covers becomes a node. Ends when an edge or a cover's replay ends the loop;
/// Undecided on a refusal or when the edges made and pending exceed `budget`.
fn frame_closure<N, K: PartialEq>(
    root: N,
    budget: usize,
    key: impl Fn(&N) -> K,
    orders: impl Fn(&N) -> usize,
    mut successor: impl FnMut(&N, usize) -> Result<N, Halt>,
    mut covers: impl FnMut(&N, &N) -> Result<bool, Halt>,
) -> OptionProof {
    let mut pending = orders(&root);
    if pending > budget {
        return OptionProof::Undecided;
    }
    let mut nodes = vec![root];
    let mut work = vec![0];
    let (mut edges, mut undecided) = (0usize, false);
    while let Some(at) = work.pop() {
        let mut reached = Vec::new();
        for order in 0..orders(&nodes[at]) {
            pending -= 1;
            edges += 1;
            match successor(&nodes[at], order) {
                Err(Halt::Ends) => return OptionProof::Ends,
                next => reached.push(next),
            }
        }
        for next in reached {
            let Ok(next) = next else {
                undecided = true;
                continue;
            };
            let mut covered = false;
            for seen in nodes.iter().filter(|seen| key(seen) == key(&next)) {
                match covers(seen, &next) {
                    Ok(true) => {
                        covered = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(Halt::Ends) => return OptionProof::Ends,
                    Err(Halt::Undecided) => {
                        undecided = true;
                        covered = true;
                        break;
                    }
                }
            }
            if !covered {
                pending = pending.saturating_add(orders(&next));
                if edges.saturating_add(pending) > budget {
                    return OptionProof::Undecided;
                }
                nodes.push(next);
                work.push(nodes.len() - 1);
            }
        }
    }
    if undecided {
        OptionProof::Undecided
    } else {
        OptionProof::Continues
    }
}

/// The answer a road took at each prompt key; a later prompt with that key reuses it.
type Road = Vec<(PromptKey, PointAnswer)>;

/// A node of the proof: the priority frame before a decision point, the state at its prompt, the
/// point's key, and the road to it.
struct PointNode {
    frame: GameState,
    prompt: GameState,
    key: PromptKey,
    road: Road,
    /// What the outcomes of the replays that reached this node wrote.
    written: Written,
    /// The point's answer paths, found when the closure first counts them; `None` when they
    /// cannot all be listed.
    edges: std::cell::OnceCell<Option<Vec<Edge>>>,
    /// The frames before the next two points with this node's key, along `road`, and what the
    /// replays to the second wrote.
    onward: std::cell::OnceCell<Result<([GameState; 2], Written), Halt>>,
}

impl PointNode {
    fn edges(&self, find: impl FnOnce() -> Option<Vec<Edge>>) -> Option<&[Edge]> {
        self.edges.get_or_init(find).as_deref()
    }
}

/// One answer path at a node, with the node its replay reached when finding the path replayed it.
struct Edge {
    path: KeyedPath,
    reached: std::cell::RefCell<Option<Result<PointNode, Halt>>>,
}

fn point_key(labels: &[Label]) -> Vec<Label> {
    let mut key = labels.to_vec();
    key.sort();
    key
}

/// CR 603.3b + CR 104.4b: replays the span `span` from `frame` under every answer at every
/// decision point it meets, with saved orders asked rather than applied, closing over the frames
/// before each point; a frame is covered when the recorded road's certification, with each
/// frame's floor and, under `certificate`, what the outcomes wrote stripped, equates it with a
/// node at the same point.
pub(crate) fn decision_points(
    frame: &GameState,
    span: NamedSpan,
    certificate: Option<&OutcomeCertificate>,
) -> OptionProof {
    let WaitingFor::Priority { player: holder } = frame.waiting_for else {
        return OptionProof::Undecided;
    };
    let Some(entries) = play_trace::span_entries(frame, span) else {
        return OptionProof::Undecided;
    };
    let items: Vec<PeriodItem> = period_items(entries, span, frame)
        .into_iter()
        .filter(|item| item.point.is_none())
        .collect();
    let _probe = SimulationProbeGuard::enter();
    let _bypass = crate::game::triggers::SavedOrdersBypass::enter();
    let s0 = replay_base(frame, holder);
    let end = FrameEnd::of(&s0, holder, span.reach);
    let living = seats(frame);
    // The replay from `from` to the decision prompt it stops at: the frame before the prompt, the
    // state at it, the answers given on the way, and what its outcomes wrote.
    let replay_from = |from: &GameState, answering: Answering| {
        #[cfg(feature = "test-support")]
        crate::game::perf_counters::record_play_trace(|counters| counters.confirm_drives += 1);
        let own = OutcomesAdmitted::nested();
        let mut replay = from.clone();
        let mut stop = PointStop {
            answering,
            taken: Vec::new(),
            reached: None,
        };
        let out = replay_cycle_ordered(
            &mut replay,
            &items,
            &end,
            &mut Vec::new(),
            None,
            &mut Vec::new(),
            Some(&mut stop),
            None,
        );
        let written = own
            .as_ref()
            .map(OutcomesAdmitted::written)
            .unwrap_or_default();
        match (out, stop.reached) {
            (Ok(_), Some(frame)) => Ok((frame, replay, stop.taken, written)),
            (Ok(_), None) => Err(Halt::Undecided),
            (Err(refusal), _) => Err(halt(&replay, &refusal)),
        }
    };
    let road_with = |road: &Road, taken: &[(PromptKey, PointAnswer)]| {
        let mut road = road.clone();
        for (key, answer) in taken {
            road.retain(|(k, _)| k != key);
            road.push((key.clone(), answer.clone()));
        }
        road
    };
    let node_at = |frame: Option<GameState>, prompt: GameState, road: Road, written: Written| {
        // A prompt following another with no priority between is no node frame.
        let frame = frame.ok_or(Halt::Undecided)?;
        let (key, _) = decision_prompt(&prompt, ORDER_EDGES).ok_or(Halt::Undecided)?;
        Ok(PointNode {
            frame,
            prompt,
            key,
            road,
            written,
            edges: std::cell::OnceCell::new(),
            onward: std::cell::OnceCell::new(),
        })
    };
    let lineage = |node: &PointNode, mut written: Written| {
        written.extend(&node.written);
        written
    };
    let step = |node: &PointNode, path: &[(PromptKey, PointAnswer)]| {
        let (frame, prompt, taken, written) =
            replay_from(&node.frame, Answering::Path(path.iter().cloned().collect()))?;
        node_at(
            frame,
            prompt,
            road_with(&node.road, &taken),
            lineage(node, written),
        )
    };
    // CR 603.3b + CR 601.2c + CR 608.2d: `node`'s answer paths, found by replaying each answer at
    // its prompt and keeping that replay as the edge when priority comes before the next decision
    // prompt.
    let answer_edges = |node: &PointNode| {
        let (key, answers) = decision_prompt(&node.prompt, ORDER_EDGES)?;
        let mut edges = Vec::new();
        for answer in answers {
            let path = vec![(key.clone(), answer)];
            match replay_from(&node.frame, Answering::Path(path.iter().cloned().collect())) {
                Ok((None, prompt, _, _)) => {
                    let (_, tails) = decision_paths(&prompt, ORDER_EDGES)?;
                    edges.extend(tails.into_iter().map(|tail| Edge {
                        path: [path.as_slice(), &tail].concat(),
                        reached: std::cell::RefCell::new(None),
                    }));
                }
                reached => {
                    let reached = reached.and_then(|(frame, prompt, taken, written)| {
                        node_at(
                            frame,
                            prompt,
                            road_with(&node.road, &taken),
                            lineage(node, written),
                        )
                    });
                    edges.push(Edge {
                        path,
                        reached: std::cell::RefCell::new(Some(reached)),
                    });
                }
            }
            if edges.len() > ORDER_EDGES {
                return None;
            }
        }
        Some(edges)
    };
    // The node at the next point with `node`'s key, each point answered along the road.
    let onward = |node: &PointNode| -> Result<PointNode, Halt> {
        let mut held: Option<PointNode> = None;
        for _ in 0..ONWARD_POINTS {
            let at = held.as_ref().unwrap_or(node);
            let (frame, prompt, taken, written) =
                replay_from(&at.frame, Answering::Road(&at.road))?;
            let next = node_at(
                frame,
                prompt,
                road_with(&at.road, &taken),
                lineage(at, written),
            )?;
            if next.key == node.key {
                return Ok(next);
            }
            held = Some(next);
        }
        Err(Halt::Undecided)
    };
    let Ok(root) = replay_from(&s0, Answering::Path(Default::default()))
        .and_then(|(frame, prompt, _, written)| node_at(frame, prompt, Vec::new(), written))
    else {
        return OptionProof::Undecided;
    };
    frame_closure(
        root,
        ORDER_EDGES,
        |node| node.key.clone(),
        // A point whose paths cannot all be listed counts past the budget.
        |node| {
            node.edges(|| answer_edges(node))
                .map_or(ORDER_EDGES + 1, <[Edge]>::len)
        },
        |node, at| {
            let edge = node
                .edges(|| answer_edges(node))
                .and_then(|edges| edges.get(at))
                .ok_or(Halt::Undecided)?;
            let reached = edge.reached.take();
            reached.unwrap_or_else(|| step(node, &edge.path))
        },
        |seen, next| {
            let chain = next.onward.get_or_init(|| {
                let first = onward(next)?;
                let second = onward(&first)?;
                Ok(([first.frame, second.frame], second.written))
            });
            let ([y1, y2], onward_written) = chain.as_ref().map_err(|halt| *halt)?;
            let written = match certificate {
                Some(_) => lineage(seen, onward_written.clone()),
                None => Written::NONE,
            };
            let normalize = |state: &GameState| normalize_period_frame(state, &items, &written);
            let floors = period_floors([&seen.frame, &next.frame, y1], Some(y2), normalize);
            Ok(living.iter().all(|&seat| {
                certify_object_growth_frames(
                    [&seen.frame, &next.frame, y1],
                    normalize,
                    seat,
                    Some(floors),
                )
                .0
                .certifies()
            }))
        },
    )
}

/// CR 732.2a: the most repetitions the counting replay performs for one offer. An offer at the cap
/// states the cap, which the replay performed; what remains is the next offer's.
pub(crate) const COUNT_CAP: u32 = 25;

/// The players still in the game (CR 800.4a).
fn seats(state: &GameState) -> BTreeSet<PlayerId> {
    state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .map(|player| player.id)
        .collect()
}

/// CR 732.2a: the repetitions the reducer performed, with the seats that left the game in each.
#[derive(Default)]
struct Count {
    repetitions: u32,
    departures: Vec<PredictedDeparture>,
}

impl Count {
    /// Notes one replayed repetition from a frame holding `before` to `after` that ended as
    /// `ended`: counted when it was performed; whether another may follow.
    fn note(&mut self, before: BTreeSet<PlayerId>, after: &GameState, ended: &CycleEnd) -> bool {
        if !ended.performed() {
            return false;
        }
        self.repetitions += 1;
        let left = before.difference(&seats(after)).copied().collect();
        self.departures
            .extend(PredictedDeparture::new(self.repetitions, left));
        *ended == CycleEnd::CameRound
    }

    /// The repetitions `holder` is still in the game after: every one, or all but the last when
    /// `holder` leaves in it.
    fn survived(&self, holder: PlayerId) -> u32 {
        let leaves = self.departures.last().is_some_and(|last| {
            last.repetition == self.repetitions && last.seats.contains(&holder)
        });
        self.repetitions - u32::from(leaves)
    }
}

/// CR 104.2a + CR 732.2a: how the counted repetitions from `start` to `end` end the game, when
/// the last of them `ended` with `holder` the winner.
fn counted_ending(
    holder: PlayerId,
    start: &GameState,
    end: &GameState,
    ended: &CycleEnd,
) -> Option<WinKind> {
    matches!(ended, CycleEnd::HolderWon(_)).then(|| {
        match classify_win_kind(holder, &ResourceVector::period(start, end), Some(end)) {
            kind @ (WinKind::LethalDamage
            | WinKind::PoisonLoss
            | WinKind::Decking
            | WinKind::ImmediateWin) => kind,
            // CR 104.2b / CR 104.3e: no opponent's life, poison or library moved, so an effect won it.
            WinKind::ExtraTurns | WinKind::Advantage => WinKind::ImmediateWin,
        }
    })
}

/// CR 732.2c: performs one cycle of `period` from `state`, a frame where `run`'s holder has
/// priority, ending where the cycle comes round again; the events it emits are appended to `events`. A
/// `declared` declaration answers the target prompts it pins at its repetition index.
pub(crate) fn perform_cycle(
    state: &mut GameState,
    period: &ConfirmedPeriod,
    run: &TakeRun,
    declared: Declared<'_>,
    events: &mut Vec<GameEvent>,
) -> CycleEnd {
    let _admitted = (period.outcomes == Outcomes::Admitted).then(OutcomesAdmitted::enter);
    run.0.repeat(
        state,
        &period.items,
        &mut Vec::new(),
        None,
        events,
        declared,
    )
}

/// CR 732.2c: the run a take performs a period in, from the frame where its holder has priority.
pub(crate) struct TakeRun(FrameEnd);

impl TakeRun {
    pub(crate) fn of(state: &GameState, period: &ConfirmedPeriod, holder: PlayerId) -> Self {
        Self(FrameEnd::of(state, holder, period.reach))
    }
}

/// CR 732.2a: performs `declared` (the recorded answers when `None`) forward from `start`, a frame
/// where `holder` has priority, for at most `n` repetitions of the period `items` make, and states
/// how far it reached.
pub(crate) fn perform_declared(
    start: &GameState,
    items: &[PeriodItem],
    reach: PeriodReach,
    holder: PlayerId,
    declared: Option<&DecisionTemplate>,
    n: u32,
) -> PerformedReach {
    let _probe = SimulationProbeGuard::enter();
    let mut replay = start.clone();
    let end = FrameEnd::of(&replay, holder, reach);
    let mut count = Count::default();
    let mut won = false;
    // CR 800.4a: `n` counts the period as its frame charges it; once a seat has left, the count
    // goes on while the period the remaining seats ask comes round.
    for k in 0..crate::game::engine::MAX_SHORTCUT_CYCLES {
        if k >= n && count.departures.is_empty() {
            break;
        }
        let before = seats(&replay);
        let ended = end.repeat(
            &mut replay,
            items,
            &mut Vec::new(),
            None,
            &mut Vec::new(),
            declared.map(|template| (template, k)),
        );
        won = matches!(ended, CycleEnd::HolderWon(_));
        if !count.note(before, &replay, &ended) {
            break;
        }
    }
    PerformedReach {
        decisions: declared.map_or_else(Vec::new, |template| template.decisions.clone()),
        reach: count.survived(holder),
        winner: won.then_some(holder),
        departures: count.departures,
    }
}

/// [`perform_declared`] of `period` from `state`'s view for `holder`, as its offer frame was.
pub(crate) fn perform_declared_on(
    state: &GameState,
    period: &ConfirmedPeriod,
    holder: PlayerId,
    declared: &DecisionTemplate,
    n: u32,
) -> PerformedReach {
    let mut start = replay_base(state, holder);
    start.waiting_for = WaitingFor::Priority { player: holder };
    perform_declared(
        &start,
        &period.items,
        period.reach,
        holder,
        Some(declared),
        n,
    )
}

/// CR 119.3 + CR 601.2c: per charged slot, the life one resolution takes at its recorded aim and
/// at each other living opponent, replayed from the offer frame with the slot declared there.
fn measure_seat_takes(
    frame: &GameState,
    holder: PlayerId,
    s_n: &GameState,
    items: &[PeriodItem],
    end: &FrameEnd,
    frames: &[GameState],
    choices: &[PinnedDecision],
) -> Vec<(DecisionSlot, PlayerId, i64)> {
    use crate::analysis::decision_template::{
        AnnouncementSubject, DecisionGroupKey, DecisionKind, IterationCount, Ranking, ReplayMode,
        TargetSchedule,
    };
    let charges = |frames: &[GameState]| {
        let refs: Vec<&GameState> = frames.iter().collect();
        let (&last, window) = refs.split_last()?;
        let touch = crate::analysis::resource::certified_period_touch(
            window,
            last,
            crate::analysis::resource::PeriodCertification::BoardCovered,
        );
        Some(
            crate::game::engine::bounded_cycle_charged_targets_for_window(
                &touch,
                holder,
                i64::MAX,
                crate::analysis::loop_check::OfferRoad::RecordedPeriod,
            ),
        )
    };
    let Some(recorded) = charges(frames) else {
        return Vec::new();
    };
    let record: Vec<(DecisionSlot, PlayerId, i64)> = recorded
        .iter()
        .filter_map(|charge| {
            let aim = charge.aimed_at.filter(|seat| *seat != holder)?;
            Some((charge.slot.clone(), aim, charge.magnitude))
        })
        .collect();
    let mut takes = record.clone();
    for seat in frame
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.id != holder)
        .map(|p| p.id)
    {
        let probes: Vec<&(DecisionSlot, PlayerId, i64)> = record
            .iter()
            .filter(|(slot, aim, took)| {
                *took > 0
                    && *aim != seat
                    && recorded
                        .iter()
                        .any(|charge| charge.slot == *slot && charge.reaches.contains(&seat))
            })
            .collect();
        if probes.is_empty() {
            continue;
        }
        let template = DecisionTemplate {
            owner: holder,
            decisions: choices
                .iter()
                .cloned()
                .map(|decision| match decision {
                    PinnedDecision::Targets { slot, .. }
                        if probes.iter().any(|(probe, ..)| *probe == slot) =>
                    {
                        PinnedDecision::Targets {
                            slot,
                            targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                                Ranking::one(AnnouncementSubject::Seat(seat)),
                            ))],
                        }
                    }
                    other => other,
                })
                .collect(),
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(2),
            },
            key: DecisionGroupKey::from_sources(&[], DecisionKind::LoopChoice),
        };
        let mut replay = s_n.clone();
        let mut measured = Vec::new();
        let came_round = end.repeat(
            &mut replay,
            items,
            &mut Vec::new(),
            None,
            &mut Vec::new(),
            Some((&template, 0)),
        ) == CycleEnd::CameRound
            && end.repeat(
                &mut replay,
                items,
                &mut Vec::new(),
                Some(&mut measured),
                &mut Vec::new(),
                Some((&template, 1)),
            ) == CycleEnd::CameRound;
        let Some(at) = charges(&measured).filter(|_| came_round) else {
            continue;
        };
        takes.extend(at.into_iter().filter_map(|charge| {
            let (_, _, record_take) = probes.iter().find(|(probe, ..)| *probe == charge.slot)?;
            // CR 800.4a: a take above the record's may come from a permanent whose controller
            // leaves first, so it is not published.
            (charge.aimed_at == Some(seat) && charge.magnitude <= *record_take).then_some((
                charge.slot,
                seat,
                charge.magnitude,
            ))
        }));
    }
    takes
}

/// CR 732.2a: performs the offer's own declaration and its suggestion — of two witnesses, the one
/// that reaches further — from `start` to the count the threshold authority published, publishes
/// the suggestion's reach (the own's without one; the recorded answers' without either), withdraws
/// a suggestion that reaches no further than the own, and returns the declarations' records.
fn perform_published(
    parts: &mut BoundedOfferParts,
    start: &GameState,
    items: &[PeriodItem],
    reach: PeriodReach,
    holder: PlayerId,
) -> (Vec<PerformedReach>, Option<PerformedReach>) {
    use crate::analysis::decision_template::{IterationCount, ReplayMode};
    let IterationCount::Fixed(n) = parts.schema.iteration_count else {
        return (Vec::new(), None);
    };
    let mut recorded = None;
    let mut performed = Vec::new();
    let mut published = None;
    if parts.declaration.is_none() && parts.schema.suggested_declaration.is_none() {
        // Kept out of `performed`, where a record makes a take conform to a declaration, which this
        // offer has none of; the take reads it as `recorded`.
        let record = perform_declared(start, items, reach, holder, None, n);
        recorded = Some(record.clone());
        published = Some(record);
    }
    for declaration in [
        parts.declaration.as_mut(),
        parts.schema.suggested_declaration.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        let record = perform_declared(start, items, reach, holder, Some(&*declaration), n);
        declaration.replay = ReplayMode::Scheduled {
            count: IterationCount::Fixed(record.reach),
        };
        published = Some(record.clone());
        performed.push(record);
    }
    if let (Some(mut other), [_, suggestion]) =
        (parts.other_suggestion.take(), performed.as_mut_slice())
    {
        let record = perform_declared(start, items, reach, holder, Some(&other), n);
        if record.reach > suggestion.reach {
            other.replay = ReplayMode::Scheduled {
                count: IterationCount::Fixed(record.reach),
            };
            parts.schema.suggested_declaration = Some(other);
            published = Some(record.clone());
            *suggestion = record;
        }
    }
    if let ([own, suggestion], Some(_)) = (performed.as_slice(), &parts.declaration) {
        if suggestion.reach <= own.reach {
            parts.schema.suggested_declaration = None;
            published = Some(own.clone());
        }
    }
    if let Some(record) = published {
        parts.schema.iteration_count = IterationCount::Fixed(record.reach);
        parts.schema.deliverable_capacity = record.reach;
        // CR 800.4a: a performance that went on past a departure measured the bound itself.
        if record.reach > n {
            parts.schema.measured_repetition_bound = Some(record.reach);
        }
        parts.predicted_winner = record.winner;
    }
    (performed, recorded)
}

/// Every span the current trace names, with the confirmer's verdict on each at `state`, which
/// must be a priority frame: the triggered abilities its first cycle resolved, or the refusal.
#[cfg(any(test, feature = "test-support"))]
pub fn confirm_for_tests(state: &GameState) -> Vec<(NamedSpan, Result<Vec<String>, OfferRefusal>)> {
    play_trace::current_named(state)
        .into_iter()
        .map(|span| {
            (
                span,
                confirm(state, span).map(|confirmation| confirmation.performed),
            )
        })
        .collect()
}

/// The authoritative reading of the mandatory span `span` at `state`: whether the outcome
/// certificate covers it, and its decision profile.
#[cfg(any(test, feature = "test-support"))]
pub fn mandatory_reading_for_tests(
    state: &GameState,
    span: NamedSpan,
) -> Option<(bool, crate::types::game_state::LoopDecisionProfile)> {
    mandatory_reading(state, span).map(|reading| (reading.certificate.is_some(), reading.profile))
}

/// CR 608.1: one cycle of the span `state`'s trace last offered, replayed from `state` as its
/// acting player's priority frame; the printed identity of each triggered ability it resolved, in
/// order. `None` when nothing was offered.
#[cfg(any(test, feature = "test-support"))]
pub fn performed_for_tests(state: &GameState) -> Option<Result<Vec<String>, OfferRefusal>> {
    let span = play_trace::play_trace_view(state)?.offered?;
    let holder = state.waiting_for.acting_player()?;
    let items = period_items(play_trace::span_entries(state, span)?, span, state);
    let mut frame = state.clone();
    frame.waiting_for = WaitingFor::Priority { player: holder };
    frame.auto_pass.clear();
    let _probe = SimulationProbeGuard::enter();
    let frame = crate::game::visibility::proposer_hidden_view(&frame, holder);
    let end = FrameEnd::of(&frame, holder, span.reach);
    let mut replay = frame.clone();
    Some(replay_cycle(
        &mut replay,
        &items,
        &end,
        &mut Vec::new(),
        None,
        &mut Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::actions::DebugAction;
    use crate::types::game_state::{
        AutoPassMode, StackResolutionAutoPassOverlay, StackResolutionBudget, StackResolutionPolicy,
        StackResolutionSession, TurnBoundary,
    };
    use crate::types::player::PlayerId;

    fn with_stack(ids: &[u64]) -> GameState {
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
        use crate::types::game_state::{StackEntry, StackEntryKind};
        let mut state = GameState::new_two_player(7);
        state.stack.extend(ids.iter().map(|&id| StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(500),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(500),
                ability: Box::new(ResolvedAbility::new(
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                    vec![],
                    ObjectId(500),
                    PlayerId(0),
                )),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }));
        state
    }

    /// CR 603.2c + CR 603.3b: two occurrences equal in source and text are still two triggered
    /// abilities, so the orders tried at a point apply every order of the three.
    #[test]
    fn every_order_of_equal_occurrences_is_applied() {
        let occurrence = |name: &str| Occurrence {
            source_name: name.to_string(),
            description: None,
        };
        let point = ranked(vec![occurrence("T"), occurrence("T"), occurrence("X")]);
        let applied: std::collections::BTreeSet<Vec<usize>> = class_orders(&point)
            .iter()
            .filter_map(|order| order_indices(&point, order))
            .collect();
        assert_eq!(applied.len(), 6, "{applied:?}");
    }

    /// CR 603.3b: an order sequence that ends the loop is found though every repeated order
    /// continues; with that sequence returning instead, the closure continues.
    #[test]
    fn a_mixed_order_that_ends_the_loop_is_found_by_the_frame_closure() {
        type Table = [[Option<usize>; 2]; 2];
        let closure = |table: &Table| {
            frame_closure(
                0usize,
                ORDER_EDGES,
                |_| (),
                |_| 2,
                |&node, order| table[node][order].ok_or(Halt::Ends),
                |seen, next| Ok(seen == next),
            )
        };
        let repeated_orders_continue = |table: &Table| {
            (0..2).all(|order| {
                let (mut node, mut seen) = (0, Vec::new());
                while !seen.contains(&node) {
                    seen.push(node);
                    let Some(next) = table[node][order] else {
                        return false;
                    };
                    node = next;
                }
                true
            })
        };
        let ends: Table = [[Some(0), Some(1)], [None, Some(1)]];
        let returns: Table = [[Some(0), Some(1)], [Some(0), Some(1)]];
        assert!(repeated_orders_continue(&ends));
        assert_eq!(
            (closure(&ends), closure(&returns)),
            (OptionProof::Ends, OptionProof::Continues)
        );
        let halted = |halt: Halt| {
            frame_closure(
                0usize,
                ORDER_EDGES,
                |_| (),
                |_| 2,
                |&node, order| returns[node][order].ok_or(Halt::Ends),
                |_, _| Err(halt),
            )
        };
        assert_eq!(
            (halted(Halt::Ends), halted(Halt::Undecided)),
            (OptionProof::Ends, OptionProof::Undecided)
        );
        let keyed = frame_closure(
            0usize,
            ORDER_EDGES,
            |&node| node,
            |_| 2,
            |&node, order| ends[node][order].ok_or(Halt::Ends),
            |_, _| Ok(true),
        );
        assert_eq!(
            keyed,
            OptionProof::Ends,
            "a cover is sought only at the same point"
        );
        assert!(
            [21, 34, 66]
                .into_iter()
                .all(|labels| order_count(labels) > ORDER_EDGES),
            "an order count past usize saturates past the budget"
        );
        let saturated = |root: usize| {
            frame_closure(
                root,
                ORDER_EDGES,
                |_| (),
                |&node| if node == 0 { usize::MAX } else { 1 },
                |_, _| Ok(0),
                |_, _| Ok(false),
            )
        };
        assert_eq!(
            (saturated(0), saturated(1)),
            (OptionProof::Undecided, OptionProof::Undecided)
        );
        let pending_saturates = frame_closure(
            2usize,
            ORDER_EDGES,
            |_| (),
            |&node| match node {
                2 => 2,
                1 => 1,
                _ => usize::MAX,
            },
            |_, order| Ok(1 - order),
            |_, _| Ok(false),
        );
        assert_eq!(pending_saturates, OptionProof::Undecided);
    }

    /// CR 104.4b: a refused ordered replay ends the loop only when the game ended or priority
    /// settled on an empty stack; any other refusal there stays undecided.
    #[test]
    fn a_refused_replay_ends_the_loop_only_when_it_settled_or_the_game_ended() {
        let empty = GameState::new_two_player(7);
        let mut ended = with_stack(&[1]);
        ended.game_end = Some(crate::types::game_state::GameEnd { winner: None });
        assert_eq!(
            (
                halt(&empty, &OfferRefusal::NoRecurrence),
                halt(&empty, &OfferRefusal::UnanswerablePrompt),
                halt(&empty, &OfferRefusal::Randomness),
                halt(&with_stack(&[1]), &OfferRefusal::NoRecurrence),
                halt(&ended, &OfferRefusal::UnanswerablePrompt),
            ),
            (
                Halt::Ends,
                Halt::Undecided,
                Halt::Undecided,
                Halt::Undecided,
                Halt::Ends
            )
        );
    }

    /// CR 732.2a: a departure's second cycle excuses only the refusal its self-compared pair
    /// makes, never one the first pair makes.
    #[test]
    fn only_the_first_pairs_cover_is_read_for_a_departed_second_cycle() {
        use crate::analysis::resource::FodderCoverRefusal::BoardCover;
        assert_eq!(
            [vec![], vec![BoardCover]].map(|second| first_pair_certifies(
                &ObjectGrowthVerdict::FodderGrowth([vec![], second])
            )),
            [true, true]
        );
        assert!(!first_pair_certifies(&ObjectGrowthVerdict::FodderGrowth([
            vec![BoardCover],
            vec![]
        ])));
        assert!(!first_pair_certifies(
            &ObjectGrowthVerdict::ResourceRecurrence(None)
        ));
        assert!(first_pair_certifies(
            &ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::GrowthRead))
        ));
    }

    /// CR 603.3b + CR 603.3d: of one batch, a trigger targeted unasked below one whose target was
    /// prompted makes a recorded answer, and the prompted one makes none.
    #[test]
    fn an_unasked_target_below_a_prompted_one_is_credited_and_the_prompted_one_is_not() {
        use crate::analysis::decision_template::{
            DecisionGroupKey, DecisionKind, IterationCount, ReplayMode,
        };
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;
        let mut state = with_stack(&[]);
        let source = |state: &mut GameState| {
            crate::game::zones::create_object(
                state,
                CardId(0),
                PlayerId(0),
                String::new(),
                Zone::Battlefield,
            )
        };
        let (unasked, prompted) = (source(&mut state), source(&mut state));
        for (id, trigger_source) in [(20, unasked), (21, prompted)] {
            let mut entry = with_stack(&[id]).stack[0].clone();
            entry.source_id = trigger_source;
            let StackEntryKind::TriggeredAbility { source_id, .. } = &mut entry.kind else {
                unreachable!("with_stack builds triggered abilities")
            };
            *source_id = trigger_source;
            state.stack.push_back(entry);
        }
        let pin = |source_id| PinnedDecision::Targets {
            slot: DecisionSlot::first(
                YieldTarget::ThisObject {
                    source_id,
                    incarnation: None,
                    trigger_description: None,
                },
                ChoicePoint::AnnouncedTarget,
            ),
            targets: Vec::new(),
        };
        let template = DecisionTemplate {
            owner: PlayerId(0),
            decisions: vec![pin(unasked), pin(prompted)],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(1),
            },
            key: DecisionGroupKey::from_sources(&[], DecisionKind::LoopChoice),
        };
        let answer = PeriodItem {
            seat: PlayerId(0),
            action: GameAction::ChooseTarget { target: None },
            play: None,
            mandatory_answer: true,
            next_object_id: 0,
            minted_since: 0,
            cost_move: None,
            point: None,
        };
        let items = [answer.clone(), answer];
        let mut done = [false; 2];
        announced_declared(
            &state,
            &FrameEnd::of(&state, PlayerId(0), PeriodReach::InStep),
            &[ObjectId(21)],
            &items,
            &mut done,
            0..2,
            Some((&template, 0)),
        )
        .expect("both pins resolve");
        assert_eq!(done, [true, false]);
    }

    /// CR 603.3d + CR 732.2a: a mandatory answer is excused only when the replay went past it to
    /// a later made item, so a two-occurrence period whose second target answer is still owed has
    /// not come round at its half-period frame.
    #[test]
    fn a_trailing_mandatory_answer_is_owed_while_one_the_replay_went_past_is_not() {
        let item = |play: bool, mandatory_answer: bool| PeriodItem {
            seat: PlayerId(0),
            action: GameAction::PassPriority,
            play: play.then_some((0, PromptClass::Priority)),
            mandatory_answer,
            next_object_id: 0,
            minted_since: 0,
            cost_move: None,
            point: None,
        };
        let alternating = [item(false, true), item(false, true)];
        assert!(!cycle_complete(&alternating, &[true, false]));
        assert!(cycle_complete(&alternating, &[true, true]));
        let went_past = [item(false, true), item(true, false)];
        assert!(cycle_complete(&went_past, &[false, true]));
        assert!(!cycle_complete(&went_past, &[true, false]));
    }

    /// CR 405.5 + CR 400.7: a floor keeps the entries the next frame holds unchanged, so a
    /// resolved trigger and a spell cast again at its old position are above it.
    #[test]
    fn a_floor_is_the_stack_the_next_frame_keeps_unchanged() {
        let spell_frame = |ids: &[u64], incarnation: u64| {
            let mut state = with_stack(ids);
            let mut spell = crate::game::game_object::GameObject::new(
                ObjectId(2),
                crate::types::identifiers::CardId(2),
                PlayerId(0),
                "Spell".to_string(),
                crate::types::zones::Zone::Stack,
            );
            spell.incarnation = incarnation;
            state.objects.insert(ObjectId(2), spell);
            state
        };
        let frame = spell_frame(&[1, 2, 3], 0);
        assert_eq!(
            (
                stack_floor(&frame, &spell_frame(&[1, 2, 3, 4], 0)),
                stack_floor(&frame, &spell_frame(&[1, 2, 5], 0)),
                stack_floor(&frame, &spell_frame(&[1, 2, 3], 1)),
            ),
            (3, 2, 1)
        );
    }

    /// CR 405.5 + CR 608.1: floors are measured on the frames the cover compares, which hold no
    /// spell the period cast, and a refused cycle keeps nothing for the frame it began at.
    #[test]
    fn floors_are_measured_without_the_periods_casts_and_need_a_completed_cycle() {
        let cast = ObjectId(9);
        let frame = |ids: &[u64]| {
            let mut state = with_stack(ids);
            state.objects.insert(
                cast,
                crate::game::game_object::GameObject::new(
                    cast,
                    crate::types::identifiers::CardId(9),
                    PlayerId(0),
                    "Cast".to_string(),
                    crate::types::zones::Zone::Stack,
                ),
            );
            state
        };
        let frames = [
            frame(&[9, 1]),
            frame(&[9, 1, 2]),
            frame(&[9, 1, 2, 3]),
            frame(&[9, 1, 2, 3, 4]),
        ];
        let [a, b, c, d] = &frames;
        assert_eq!(
            (
                period_floors([a, b, c], Some(d), |f| normalize_cast_frame(f, &[cast])),
                period_floors([a, b, c], None, |f| normalize_cast_frame(f, &[cast])),
            ),
            ([1, 2, 3], [1, 2, 0])
        );
    }

    /// CR 117.3d: a replay takes only the period's own passes, so neither standing pass survives
    /// into its base.
    #[test]
    fn the_replay_base_drops_standing_pass_overlays() {
        let mut frame = GameState::new_two_player(7);
        let standing = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        };
        frame.auto_pass.insert(PlayerId(1), standing);
        frame.stack_resolution_session = Some(StackResolutionSession {
            entries: Vec::new(),
            cursor: 0,
            representatives: [PlayerId(0)].into_iter().collect(),
            verified_pass_representatives: BTreeSet::new(),
            budget: StackResolutionBudget::Unlimited,
            policy: StackResolutionPolicy::Committed,
            auto_pass_overlay: StackResolutionAutoPassOverlay {
                baseline: [(PlayerId(1), standing)].into_iter().collect(),
            },
        });
        let base = replay_base(&frame, PlayerId(0));
        assert!(base.auto_pass.is_empty());
        assert!(base.stack_resolution_session.is_none());
    }

    /// P0 casts Banisher Priest beside Soul Warden with `bears` creatures on P1's battlefield,
    /// stopping at the order of the two triggers.
    fn exiler_order_prompt(bears: usize) -> GameState {
        use crate::game::scenario::GameScenario;
        use crate::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(
            PlayerId(0),
            "Soul Warden",
            1,
            1,
            "Whenever another creature enters, you gain 1 life.",
        );
        let priest = scenario
            .add_creature_to_hand_from_oracle(
                PlayerId(0),
                "Banisher Priest",
                2,
                2,
                "When this creature enters, exile target creature an opponent controls until this creature leaves the battlefield.",
            )
            .id();
        for _ in 0..bears {
            scenario.add_creature(PlayerId(1), "Grizzly Bears", 2, 2);
        }
        let mut runner = scenario.build();
        drop(runner.cast(priest).commit());
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
                return runner.state().clone();
            }
            runner
                .act(GameAction::PassPriority)
                .expect("each player may pass priority");
        }
        panic!("no trigger order: {:?}", runner.state().waiting_for);
    }

    /// CR 603.3b + CR 601.2c: a trigger order followed, with no priority between, by a target
    /// choice is one path per order and target, the target choice read in each order's branch.
    #[test]
    fn decision_paths_run_through_consecutive_decision_prompts() {
        let prompt = exiler_order_prompt(2);
        let (key, paths) = decision_paths(&prompt, ORDER_EDGES)
            .expect("two orders and two targets fit the budget");
        assert!(matches!(key, PromptKey::Order { .. }), "{key:?}");
        assert_eq!(paths.len(), 4, "{paths:?}");
        for path in &paths {
            assert!(
                matches!(
                    path.as_slice(),
                    [
                        (PromptKey::Order { .. }, PointAnswer::Order(_)),
                        (
                            PromptKey::Choice {
                                point: ChoicePoint::AnnouncedTarget,
                                options: 2,
                                ..
                            },
                            PointAnswer::Option(GameAction::ChooseTarget { .. })
                        )
                    ]
                ),
                "{path:?}"
            );
        }
        let orders = class_orders(&point_labels(&prompt).expect("an ordering point"));
        assert_eq!(orders.len(), 2);
        for order in orders {
            let targets: Vec<&PointAnswer> = paths
                .iter()
                .filter(|path| path[0].1 == PointAnswer::Order(order.clone()))
                .map(|path| &path[1].1)
                .collect();
            assert_eq!(targets.len(), 2, "{order:?}");
            assert_ne!(targets[0], targets[1], "{order:?}");
        }
        assert_eq!(decision_paths(&prompt, 3), None);
    }

    /// CR 104.4b: a target choice offering one more creature than the start's is another point,
    /// so no cover is sought between them; the same options key the same point.
    #[test]
    fn a_target_choice_offering_another_option_is_another_point() {
        let target_key = |bears| {
            let mut prompt = exiler_order_prompt(bears);
            apply(
                &mut prompt,
                PlayerId(0),
                GameAction::OrderTriggers { order: vec![0, 1] },
            )
            .expect("P0 orders its triggers");
            decision_prompt(&prompt, ORDER_EDGES)
                .expect("the target choice is a decision point")
                .0
        };
        let two = target_key(2);
        assert!(
            matches!(
                two,
                PromptKey::Choice {
                    point: ChoicePoint::AnnouncedTarget,
                    options: 2,
                    ..
                }
            ),
            "{two:?}"
        );
        assert_eq!(target_key(2), two);
        assert_ne!(target_key(3), two);
    }

    /// A replayed play whose final life edit is unrepresentable carries its own
    /// refusal, so the public shortcut entry can reject the take rather than
    /// treating it as an illegal play or silently stopping the repetition.
    #[test]
    fn step_carries_an_unrepresentable_life_change_as_its_own_refusal() {
        let mut state = GameState::new_two_player(7);
        state.debug_mode = true;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.players[0].life = i32::MAX - 1;
        let mut events = Vec::new();
        let refusal = step(
            &mut state,
            PlayerId(0),
            GameAction::Debug(DebugAction::GainLife {
                player_id: PlayerId(0),
                amount: 2,
            }),
            OfferRefusal::IllegalReplayedPlay,
            &mut events,
        )
        .expect_err("the impossible gain must refuse the replay");
        assert_eq!(
            refusal,
            OfferRefusal::LifeChangeUnrepresentable {
                player: PlayerId(0)
            }
        );
    }

    /// CR 603.5 + CR 732.3: a remembered answer is its seat's optional choice; resolutions and
    /// mandatory answers attribute no seat.
    #[test]
    fn a_remembered_answer_attributes_the_span_to_its_seat() {
        let entry = |seat: PlayerId, kind: EntryKind| TraceEntry {
            seat,
            kind,
            depth: 0,
            prompt: PromptClass::Other,
            next_object_id: 0,
        };
        let remembered = |seat| entry(seat, EntryKind::Remembered { asked_by: None });
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        assert_eq!(
            seat_refusal(&[remembered(p1)], p0),
            Some(OfferRefusal::ForeignSeat { seat: p1 })
        );
        assert_eq!(seat_refusal(&[remembered(p0)], p0), None);
        let mandatory = entry(
            p1,
            EntryKind::Answer {
                action: GameAction::PassPriority,
                optional: AnswerOptionality::Mandatory,
                asked_by: Some(0),
                cost_move: None,
                point: None,
            },
        );
        assert_eq!(
            seat_refusal(
                &[entry(p1, EntryKind::Resolution { node: 0 }), mandatory],
                p0
            ),
            None
        );
        let play = entry(
            p0,
            EntryKind::Play {
                action: GameAction::PassPriority,
                node: 1,
                locus: PlayLocus::Unread,
            },
        );
        assert_eq!(
            seat_refusal(&[remembered(p1), play], p0),
            Some(OfferRefusal::Fragmented {
                seats: vec![p0, p1]
            })
        );
    }

    /// CR 400.7 + CR 111.7 + CR 603.2: at each priority frame of Swarming Goblins, Worldgorger
    /// Dragon and Animate Dead while a Goblin token the d20 made is on the battlefield, the
    /// Dragon's "exile all other permanents you control" reads the token; with it excluded as the
    /// token's confiner and the d20's trigger as its writer, nothing reads the token, the Dragon's
    /// "return the exiled cards to the battlefield under their owner's control" included.
    #[test]
    fn the_confiner_is_the_only_reader_of_a_goblin_token_beside_worldgorger() {
        use crate::analysis::resource::written_member_read;
        use crate::game::functioning_abilities::active_trigger_definitions;
        use crate::game::scenario::GameScenario;
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::game_state::LoopDetectionMode;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use crate::types::zones::Zone;
        let db = crate::test_support::shared_card_db();
        let p0 = PlayerId(0);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dragon = scenario.add_real_card(p0, "Worldgorger Dragon", Zone::Graveyard, db);
        let animate = scenario.add_real_card(p0, "Animate Dead", Zone::Hand, db);
        let goblins = scenario.add_real_card(p0, "Swarming Goblins", Zone::Battlefield, db);
        scenario.with_mana_pool(
            p0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]); 2],
        );
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Off;
        drop(runner.cast(animate).target_object(dragon).commit());
        let triggers = |state: &GameState, id: ObjectId| {
            active_trigger_definitions(state, &state.objects[&id])
                .map(|active| (active.definition_ref, active.definition.destination))
                .collect::<Vec<_>>()
        };
        let mut frames = Vec::new();
        for _ in 0..200 {
            let state = runner.state();
            let tokens: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| state.objects[id].is_token)
                .collect();
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) && !tokens.is_empty() {
                frames.push((state.clone(), tokens));
            }
            if frames
                .iter()
                .any(|(frame, _)| frame.objects[&dragon].zone == Zone::Battlefield)
            {
                break;
            }
            // Animate Dead's trigger goes on the stack first and resolves last.
            let action = match &state.waiting_for {
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let mut order: Vec<usize> = (0..triggers.len()).collect();
                    order.sort_by_key(|&index| triggers[index].source_id != animate);
                    GameAction::OrderTriggers { order }
                }
                _ => GameAction::PassPriority,
            };
            runner
                .act(action)
                .expect("the loop's every action is legal");
        }
        let (on_battlefield, _) = frames
            .iter()
            .find(|(frame, _)| frame.objects[&dragon].zone == Zone::Battlefield)
            .expect("reach: the Dragon returns while the tokens are on the battlefield");
        let dragon_triggers = triggers(on_battlefield, dragon);
        let confiner = dragon_triggers
            .iter()
            .find(|(_, destination)| *destination == Some(Zone::Battlefield))
            .map(|(id, _)| id.clone())
            .expect("reach: the Dragon's enters trigger functions");
        assert_eq!(
            dragon_triggers.len(),
            2,
            "reach: its leaves trigger functions too"
        );
        let writer = triggers(on_battlefield, goblins)
            .first()
            .map(|(id, _)| id.clone())
            .expect("reach: the d20's trigger functions");
        for (frame, tokens) in &frames {
            for &token in tokens {
                assert!(!written_member_read(
                    frame,
                    token,
                    &[writer.clone(), confiner.clone()]
                ));
                assert_eq!(
                    written_member_read(frame, token, std::slice::from_ref(&writer)),
                    frame.objects[&dragon].zone == Zone::Battlefield
                );
            }
        }
    }

    /// CR 514.1 + CR 800.4a: P0 holding eight Islands at its cleanup must discard one it has
    /// drawn, so a cycle stopped there is not performed; the same cycle with P0 gone from the game
    /// was. Neither is P1's end, and neither is P0's own main phase before it.
    #[test]
    fn a_cycle_stopped_at_its_holders_cleanup_is_unperformed_and_one_its_holder_left_is_performed()
    {
        use crate::game::scenario::GameScenario;
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::phase::Phase;
        use crate::types::zones::Zone;
        let db = crate::test_support::shared_card_db();
        let (p0, p1) = (PlayerId(0), PlayerId(1));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..8 {
            scenario.add_real_card(p0, "Island", Zone::Hand, db);
        }
        let mut runner = scenario.build();
        let main = runner.state();
        assert_eq!(
            stopped(main, p0, &seats(main), OfferRefusal::NoRecurrence),
            CycleEnd::Refused(OfferRefusal::NoRecurrence)
        );
        for _ in 0..40 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardToHandSize { .. }
            ) {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("each player may pass priority");
        }
        let cleanup = runner.state().clone();
        assert!(
            matches!(cleanup.waiting_for, WaitingFor::DiscardToHandSize { player, count: 1, .. } if player == p0),
            "reach: {:?}",
            cleanup.waiting_for
        );
        let started = seats(&cleanup);
        let at_cleanup = stopped(&cleanup, p0, &started, OfferRefusal::NoRecurrence);
        assert_eq!(
            at_cleanup,
            CycleEnd::HolderCleanup(OfferRefusal::NoRecurrence)
        );
        assert!(!at_cleanup.performed());
        assert_eq!(
            stopped(&cleanup, p1, &started, OfferRefusal::NoRecurrence),
            CycleEnd::Refused(OfferRefusal::NoRecurrence)
        );
        let mut left = cleanup.clone();
        crate::game::elimination::eliminate_player(&mut left, p0, &mut Vec::new());
        let holder_left = stopped(&left, p0, &started, OfferRefusal::NoRecurrence);
        assert_eq!(
            holder_left,
            CycleEnd::HolderLeft(OfferRefusal::NoRecurrence)
        );
        assert!(holder_left.performed());
    }
}
