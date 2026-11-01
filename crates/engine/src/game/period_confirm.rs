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
    driving_resources_non_decreasing_carried, frame_without, CertifiedInstructedDeparture,
    CleanupPair, CounterClass, CoveredGrowth, ObjectClass, ObjectGrowthVerdict, PredictedDeparture,
    RecurrenceCover, ResourceVector,
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
    /// CR 704.5a: the period moves some player toward losing the game, and the threshold
    /// authority refused its replayed frames.
    LossAxis,
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
            EntryKind::Answer { .. } => false,
        });
        if uses_the_stack {
            Self::UsesTheStack
        } else {
            Self::ManaAbilitiesOnly
        }
    }
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
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
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

/// The certificate, schema and declaration a bounded offer publishes.
pub(crate) type BoundedOfferParts = (
    LoopCertificate,
    ShortcutDecisionSchema,
    Option<DecisionTemplate>,
);

/// The refusal of a span whose optional choices are not `holder`'s alone: more than one seat's
/// is a fragmented loop (CR 732.3), and one other seat's is that seat's period.
fn seat_refusal(body: &[TraceEntry], holder: PlayerId) -> Option<OfferRefusal> {
    let mut seats: Vec<PlayerId> = body
        .iter()
        .filter(|entry| match &entry.kind {
            // CR 117.1a + CR 117.1b: casting a spell or activating an ability is optional.
            EntryKind::Play { .. } => true,
            EntryKind::Answer { optional, .. } => *optional == AnswerOptionality::Optional,
            EntryKind::Resolution { .. } => false,
        })
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
                EntryKind::Resolution { .. } => return None,
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

/// Where a cycle must end: priority back with `holder`, with the frame's top of stack and at least
/// its depth (accumulation beneath is admitted), at a window the period's reach admits.
struct FrameEnd {
    reach: PeriodReach,
    depth: usize,
    holder: PlayerId,
    top: Option<TopOfStack>,
}

impl FrameEnd {
    fn of(frame: &GameState, holder: PlayerId, reach: PeriodReach) -> Self {
        Self {
            reach,
            depth: frame.stack.len(),
            holder,
            top: top_of_stack(frame),
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

/// Upper bound on the actions one replayed cycle may take.
const CYCLE_BEATS: usize = 512;

/// Applies one action, refusing a random outcome drawn by it (CR 732.2a); its events are appended
/// to `events`.
fn step(
    replay: &mut GameState,
    seat: PlayerId,
    action: GameAction,
    rejected: OfferRefusal,
    events: &mut Vec<GameEvent>,
) -> Result<(), OfferRefusal> {
    let outcomes = replay.rng.outcome_draws();
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
    if replay.rng.outcome_draws() != outcomes {
        return Err(OfferRefusal::Randomness);
    }
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
    replay_cycle_ordered(replay, items, end, choices, frames, None, events, None)
}

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

/// One answer at a decision prompt: an order of its triggers, or one of its options.
#[derive(Debug, Clone, PartialEq)]
enum PointAnswer {
    Order(Vec<Label>),
    Option(GameAction),
}

/// What makes two decision prompts the same point: an ordering point's occurrences, or the
/// choice, the triggered ability asking it, and how many options it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptKey {
    Order(Vec<Label>),
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
            Some((PromptKey::Order(point_key(&labels)), answers))
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
        ChoicePoint::MayGate
        | ChoicePoint::TapCost
        | ChoicePoint::ProliferateSet
        | ChoicePoint::ManaColor
        | ChoicePoint::ConvokeTaps
        | ChoicePoint::Mode
        | ChoicePoint::UnlessBreak => None,
    }
}

/// The action that gives `answer` at the prompt at `replay`; an option is answered only by the
/// same action among the prompt's options, never by its position.
fn answer_action(replay: &GameState, answer: &PointAnswer) -> Option<GameAction> {
    match answer {
        PointAnswer::Order(order) => Some(GameAction::OrderTriggers {
            order: order_indices(&point_labels(replay)?, order)?,
        }),
        PointAnswer::Option(chosen) => choice_options(replay)
            .contains(chosen)
            .then(|| chosen.clone()),
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
/// `settles`, when given, collects each frame a pass leaves at the active player's priority.
#[allow(clippy::too_many_arguments)]
fn replay_cycle_ordered(
    replay: &mut GameState,
    items: &[PeriodItem],
    end: &FrameEnd,
    choices: &mut Vec<PinnedDecision>,
    mut frames: Option<&mut Vec<GameState>>,
    mut settles: Option<&mut Vec<GameState>>,
    events: &mut Vec<GameEvent>,
    mut stop: Option<&mut PointStop<'_>>,
) -> Result<Vec<String>, OfferRefusal> {
    let mut done = vec![false; items.len()];
    let mut performed = Vec::new();
    let mut last_priority: Option<GameState> = None;
    let start = play_trace::WindowKey::of(replay);
    for beat in 0..CYCLE_BEATS {
        if replay.game_end.is_some() || !end.admits(start, replay) {
            return Err(OfferRefusal::NoRecurrence);
        }
        let next = done.iter().position(|d| !d);
        let next_play = next.map(|from| {
            (from..items.len())
                .find(|&at| !done[at] && items[at].play.is_some())
                .unwrap_or(items.len())
        });
        if let WaitingFor::Priority { player } = replay.waiting_for {
            if let Some(frames) = frames.as_deref_mut() {
                frames.push(replay.clone());
            }
            if stop.is_some() {
                last_priority = Some(replay.clone());
            } else if beat > 0 && end.reached(start, replay) && cycle_complete(items, &done) {
                return Ok(performed);
            }
            let Some(at) = next else {
                pass(
                    replay,
                    player,
                    &mut performed,
                    events,
                    settles.as_deref_mut(),
                )?;
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
                pass(
                    replay,
                    player,
                    &mut performed,
                    events,
                    settles.as_deref_mut(),
                )?;
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
        let answered = match next {
            None => false,
            Some(at) => {
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
                let candidates = at..next_play.unwrap_or(items.len());
                answer(replay, items, &mut done, candidates, choices, events)?
            }
        };
        if !answered {
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

/// CR 117.4: `player` passes, noting the triggered ability the pass resolves; a frame the pass
/// leaves at the active player's priority is pushed to `settles`.
fn pass(
    replay: &mut GameState,
    player: PlayerId,
    performed: &mut Vec<String>,
    events: &mut Vec<GameEvent>,
    settles: Option<&mut Vec<GameState>>,
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
    if let Some(settles) = settles.filter(|_| {
        matches!(replay.waiting_for, WaitingFor::Priority { player } if player == replay.active_player)
    }) {
        settles.push(replay.clone());
    }
    if let Some((id, identity)) = top {
        if replay.stack.iter().all(|entry| entry.id != id) {
            performed.push(identity);
        }
    }
    Ok(())
}

/// Answers the current prompt with the first unconsumed answer among `candidates` the reducer
/// accepts; `false` when none does.
fn answer(
    replay: &mut GameState,
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
        let offered = item
            .cost_move
            .as_ref()
            .and_then(|_| CostChoices::at(replay));
        let action = rebound(item, replay);
        let choice = answered_choice(replay, &action);
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
/// strips a different copy in each frame.
fn normalize_period_frame(state: &GameState, items: &[PeriodItem]) -> GameState {
    normalize_cast_frame(state, &period_casts(state, items))
}

/// A span replayed twice from a priority frame: the stage the confirmer and the
/// mandatory-recurrence witness share.
struct ReplayedPeriod {
    holder: PlayerId,
    items: Vec<PeriodItem>,
    performance: Option<PeriodPerformance>,
    end: FrameEnd,
    choices: Vec<PinnedDecision>,
    #[cfg(any(test, feature = "test-support"))]
    performed: Vec<String>,
    s_n: GameState,
    s_n1: GameState,
    /// The frame the second cycle reached, or where it was refused.
    replay: GameState,
    second: Result<Vec<String>, OfferRefusal>,
    /// Every frame above is read inside the probe.
    _probe: SimulationProbeGuard,
}

/// CR 117.3d + CR 732.2a: the frame a replay starts from, `holder`'s view with the standing-pass
/// overlays cleared, since the replay takes only the period's own passes.
fn replay_base(frame: &GameState, holder: PlayerId) -> GameState {
    let mut base = crate::game::visibility::proposer_hidden_view(frame, holder);
    base.auto_pass.clear();
    base.stack_resolution_session = None;
    base
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
        let third = replay_cycle(
            &mut next,
            &self.items,
            &self.end,
            &mut Vec::new(),
            None,
            &mut Vec::new(),
        );
        period_floors(
            [&self.s_n, &self.s_n1, &self.replay],
            third.is_ok().then_some(&next),
            |state| normalize_period_frame(state, &self.items),
        )
    }
}

/// CR 732.2a: replays the span `span` of the current trace twice from `frame`, a priority frame;
/// the first cycle's refusal refuses, and the second's is kept for the reader. `settles`, when
/// given, collects both cycles' settle frames.
fn replay_period(
    frame: &GameState,
    span: NamedSpan,
    mut settles: Option<&mut Vec<GameState>>,
) -> Result<ReplayedPeriod, OfferRefusal> {
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
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_play_trace(|counters| counters.confirm_drives += 1);
    let probe = SimulationProbeGuard::enter();
    let s_n = replay_base(frame, holder);
    let end = FrameEnd::of(&s_n, holder, span.reach);
    let mut replay = s_n.clone();
    let mut choices = Vec::new();
    #[cfg_attr(not(any(test, feature = "test-support")), allow(unused_variables))]
    let performed = replay_cycle_ordered(
        &mut replay,
        &items,
        &end,
        &mut choices,
        None,
        settles.as_deref_mut(),
        &mut Vec::new(),
        None,
    )?;
    let s_n1 = replay.clone();
    let second = replay_cycle_ordered(
        &mut replay,
        &items,
        &end,
        &mut Vec::new(),
        None,
        settles,
        &mut Vec::new(),
        None,
    );
    Ok(ReplayedPeriod {
        holder,
        items,
        performance,
        end,
        choices,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        s_n,
        s_n1,
        replay,
        second,
        _probe: probe,
    })
}

/// CR 732.2a: confirms the span `span` of the current trace as a period `frame`'s priority
/// holder may propose to repeat.
pub(crate) fn confirm(frame: &GameState, span: NamedSpan) -> Result<Confirmation, OfferRefusal> {
    let ReplayedPeriod {
        holder,
        items,
        performance,
        end,
        choices,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        s_n,
        s_n1,
        mut replay,
        second,
        _probe,
    } = replay_period(frame, span, None)?;
    // A period whose second cycle is refused has one pair of frames, read as any pair is.
    let (prior, current) = match second {
        Ok(_) => (&s_n1, &replay),
        Err(_) => (&s_n, &s_n1),
    };
    let (verdict, growth) = certify_object_growth_frames(
        [&s_n, &s_n1, current],
        |state| normalize_period_frame(state, &items),
        holder,
        None,
    );
    // CR 732.2a: one count per offer. Something of its proposer's own falling makes the period
    // the counting replay's; any other period with a loss axis is the threshold authority's.
    // A spend verdict asks the sign check only for its refusals.
    let sign = match (&verdict, verdict.certifies()) {
        (ObjectGrowthVerdict::Spend, _) => {
            period_sign_check(prior, current, holder).map(|_| PeriodSign::Spend)
        }
        (
            ObjectGrowthVerdict::FodderGrowth(_) | ObjectGrowthVerdict::ResourceRecurrence(_),
            true,
        ) => period_sign_check(prior, current, holder),
        (
            ObjectGrowthVerdict::FodderGrowth(_) | ObjectGrowthVerdict::ResourceRecurrence(_),
            false,
        ) => Err(OfferRefusal::Cover(verdict.clone())),
    };
    let (delta, departure, bounded, growth, counted_by) = match (sign, second) {
        // Only a spend says why a period stopped after one repetition.
        (Ok(PeriodSign::Unbounded(_) | PeriodSign::LossAxis) | Err(_), Err(stopped)) => {
            return Err(stopped);
        }
        (Err(refusal), Ok(_)) => return Err(refusal),
        (Ok(PeriodSign::Unbounded(unbounded)), Ok(_)) => {
            let (delta, departure) = *unbounded;
            (delta, departure, None, growth, CountedBy::Threshold)
        }
        // CR 704.5a + CR 732.2a: a loss period may be repeated only up to the threshold crossings
        // the authority measures on its replayed frames.
        (Ok(PeriodSign::LossAxis), Ok(_)) => {
            let mut frames = Vec::new();
            replay_cycle(
                &mut s_n1.clone(),
                &items,
                &end,
                &mut Vec::new(),
                Some(&mut frames),
                &mut Vec::new(),
            )?;
            // CR 514.1: a turn-cycle period's count is bounded by its cleanup discard.
            let cleanup = (verdict
                == ObjectGrowthVerdict::ResourceRecurrence(Some(RecurrenceCover::TurnCycle)))
            .then(|| CleanupPair::measure(&frames, holder))
            .flatten();
            let parts = replay_bounded_offer(frame, holder, &frames, &choices, cleanup)
                .map_err(|_| OfferRefusal::LossAxis)?;
            let delta = ResourceVector::period(prior, current);
            (delta, None, Some(parts), growth, CountedBy::Threshold)
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
            count.note(seats(&s_n), &s_n1, true, holder);
            let mut repeats = count.note(seats(&s_n1), &replay, second.is_ok(), holder);
            while repeats && count.repetitions < COUNT_CAP {
                let before = seats(&replay);
                let completed = replay_cycle(
                    &mut replay,
                    &items,
                    &end,
                    &mut Vec::new(),
                    None,
                    &mut Vec::new(),
                )
                .is_ok();
                repeats = count.note(before, &replay, completed, holder);
            }
            let parts = counted_offer(
                &frames,
                points,
                count.survived(holder),
                count.repetitions,
                counted_ending(holder, &s_n, &replay),
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
        },
        frames: Box::new([s_n1, replay]),
        delta,
        departure,
        #[cfg(any(test, feature = "test-support"))]
        performed,
        bounded,
    })
}

/// What a held mandatory period's replay shows besides its recurrence.
pub(crate) struct HeldPeriod {
    /// Performs only when a ring drain's delimiter fires at a settle frame the replay reaches.
    pub(crate) counted_take: crate::analysis::loop_check::CountedTake,
    /// The decision members the replayed period itself drew, such as a shuffle's randomness
    /// (CR 701.24a).
    pub(crate) replayed: crate::types::game_state::LoopDecisionProfile,
}

/// CR 104.4b + CR 732.4: whether the mandatory span `span`, replayed twice from `frame`, recurs
/// for every living player with net progress and nothing moving toward a state-based action:
/// no loss axis (CR 704.5c), no driving resource falling on a carried permanent (CR 704.5g,
/// CR 704.5i, CR 704.5q), and no -1/-1 counter growth (CR 704.5f).
pub(crate) fn mandatory_period_holds(frame: &GameState, span: NamedSpan) -> Option<HeldPeriod> {
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_play_trace(|counters| counters.mandatory_replays += 1);
    let mut settles = Vec::new();
    let Ok(period @ ReplayedPeriod { second: Ok(_), .. }) =
        replay_period(frame, span, Some(&mut settles))
    else {
        return None;
    };
    let floors = period.floors();
    let ReplayedPeriod {
        items,
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
            |state| normalize_period_frame(state, &items),
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
    let holds = period_recurs_without_end(&delta, buried_rises)
        && living
            .iter()
            .all(|&seat| driving_resources_non_decreasing_carried(&s_n1, &s_n2, seat, Some(&s_n)))
        && delta
            .counters
            .get(&(CounterClass::Minus1Minus1, ObjectClass::Creature))
            .is_none_or(|&grew| grew <= 0);
    if !holds {
        return None;
    }
    // The boundary is seeded from the replay's own base, so it shares the frames' view.
    let boundary = crate::game::engine::take_boundary(&s_n);
    let counted_take = if settles
        .iter()
        .any(|settled| crate::game::engine::settle_recurs(&boundary, &settled.normalize_for_loop()))
    {
        crate::analysis::loop_check::CountedTake::Performs
    } else {
        crate::analysis::loop_check::CountedTake::Unperformable(
            crate::analysis::loop_check::UnperformableCause::UndelimitedCover,
        )
    };
    let mut replayed = crate::types::game_state::LoopDecisionProfile::default();
    // CR 701.24a: a shuffle the replay performs draws randomness the recorded span may not hold.
    if s_n.rng.get_word_pos() != s_n1.rng.get_word_pos() {
        replayed
            .members
            .insert(crate::types::game_state::LoopDecision::Random);
    }
    Some(HeldPeriod {
        counted_take,
        replayed,
    })
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
    /// The point's answer paths, found when the closure first counts them; `None` when they
    /// cannot all be listed.
    edges: std::cell::OnceCell<Option<Vec<Edge>>>,
    /// The frames before the next two points with this node's key, along `road`.
    onward: std::cell::OnceCell<Result<[GameState; 2], Halt>>,
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
/// frame's floor, equates it with a node at the same point.
pub(crate) fn decision_points(frame: &GameState, span: NamedSpan) -> OptionProof {
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
    // state at it, and the answers given on the way.
    let replay_from = |from: &GameState, answering: Answering| {
        #[cfg(feature = "test-support")]
        crate::game::perf_counters::record_play_trace(|counters| counters.confirm_drives += 1);
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
            None,
            &mut Vec::new(),
            Some(&mut stop),
        );
        match (out, stop.reached) {
            (Ok(_), Some(frame)) => Ok((frame, replay, stop.taken)),
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
    let node_at = |frame: Option<GameState>, prompt: GameState, road: Road| {
        // A prompt following another with no priority between is no node frame.
        let frame = frame.ok_or(Halt::Undecided)?;
        let (key, _) = decision_prompt(&prompt, ORDER_EDGES).ok_or(Halt::Undecided)?;
        Ok(PointNode {
            frame,
            prompt,
            key,
            road,
            edges: std::cell::OnceCell::new(),
            onward: std::cell::OnceCell::new(),
        })
    };
    let step = |node: &PointNode, path: &[(PromptKey, PointAnswer)]| {
        let (frame, prompt, taken) =
            replay_from(&node.frame, Answering::Path(path.iter().cloned().collect()))?;
        node_at(frame, prompt, road_with(&node.road, &taken))
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
                Ok((None, prompt, _)) => {
                    let (_, tails) = decision_paths(&prompt, ORDER_EDGES)?;
                    edges.extend(tails.into_iter().map(|tail| Edge {
                        path: [path.as_slice(), &tail].concat(),
                        reached: std::cell::RefCell::new(None),
                    }));
                }
                reached => {
                    let reached = reached.and_then(|(frame, prompt, taken)| {
                        node_at(frame, prompt, road_with(&node.road, &taken))
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
            let (frame, prompt, taken) = replay_from(&at.frame, Answering::Road(&at.road))?;
            let next = node_at(frame, prompt, road_with(&at.road, &taken))?;
            if next.key == node.key {
                return Ok(next);
            }
            held = Some(next);
        }
        Err(Halt::Undecided)
    };
    let Ok(root) = replay_from(&s0, Answering::Path(Default::default()))
        .and_then(|(frame, prompt, _)| node_at(frame, prompt, Vec::new()))
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
                Ok([first.frame, second.frame])
            });
            let [y1, y2] = chain.as_ref().map_err(|halt| *halt)?;
            let normalize = |state: &GameState| normalize_period_frame(state, &items);
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

/// [`mandatory_period_holds`] on the mandatory span at `state`, a priority frame; `None` when
/// there is no such span.
#[cfg(any(test, feature = "test-support"))]
pub fn mandatory_period_holds_for_tests(state: &GameState) -> Option<bool> {
    let span = play_trace::mandatory_recurrence_span(state)?;
    Some(mandatory_period_holds(state, span).is_some())
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
    /// Notes one replayed repetition from a frame holding `before` to `after`; whether another
    /// may follow. CR 104.2a + CR 800.4a: a repetition the game ends in, or `holder` leaves in,
    /// was performed although it did not come round, and any other refused repetition was not.
    fn note(
        &mut self,
        before: BTreeSet<PlayerId>,
        after: &GameState,
        completed: bool,
        holder: PlayerId,
    ) -> bool {
        let ended = after.game_end.is_some() || !crate::game::players::is_alive(after, holder);
        if !completed && !ended {
            return false;
        }
        self.repetitions += 1;
        let left = before.difference(&seats(after)).copied().collect();
        self.departures
            .extend(PredictedDeparture::new(self.repetitions, left));
        completed
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
/// they end it with `holder` the winner.
fn counted_ending(holder: PlayerId, start: &GameState, end: &GameState) -> Option<WinKind> {
    let won = end
        .game_end
        .as_ref()
        .is_some_and(|game_end| game_end.winner == Some(holder));
    won.then(
        || match classify_win_kind(holder, &ResourceVector::period(start, end), Some(end)) {
            kind @ (WinKind::LethalDamage
            | WinKind::PoisonLoss
            | WinKind::Decking
            | WinKind::ImmediateWin) => kind,
            // CR 104.2b / CR 104.3e: no opponent's life, poison or library moved, so an effect won it.
            WinKind::ExtraTurns | WinKind::Advantage => WinKind::ImmediateWin,
        },
    )
}

/// CR 732.2c: performs one cycle of `period` from `state`, a frame where `holder` has priority,
/// ending where the cycle comes round again; the events it emits are appended to `events`.
pub(crate) fn perform_cycle(
    state: &mut GameState,
    period: &ConfirmedPeriod,
    holder: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<(), OfferRefusal> {
    let end = FrameEnd::of(state, holder, period.reach);
    replay_cycle(state, &period.items, &end, &mut Vec::new(), None, events).map(drop)
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
        assert!(matches!(key, PromptKey::Order(_)), "{key:?}");
        assert_eq!(paths.len(), 4, "{paths:?}");
        for path in &paths {
            assert!(
                matches!(
                    path.as_slice(),
                    [
                        (PromptKey::Order(_), PointAnswer::Order(_)),
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
}
