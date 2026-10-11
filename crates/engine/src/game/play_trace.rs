//! CR 732.2a: the play trace — every play and answer every player makes in the current step, and
//! the spans it carries past that step (CR 500.7, CR 500.8), keyed by the ability made, which
//! names the candidate periods a shortcut may repeat.
//!
//! The trace records at the outermost `apply()` boundary, a triggered ability's resolution at
//! the priority pass that resolves it, and names candidates at the base's priority window
//! (`engine::reconcile_loop_shortcut`), where the producer asks the confirmer about each span.

use std::cell::Cell;

use crate::analysis::ability_graph::{candidate_cycles_from_sources, AbilitySlot, AbilitySource};
use crate::analysis::resource::ResourceAxis;
use crate::game::engine::in_simulation_probe;
use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, DelayedAbilityOrigin, TriggerDefinitionOccurrenceRef,
    TriggerDefinitionRef, TriggerPrintedOrigin,
};
use crate::types::actions::GameAction;
use crate::types::card::PrintedCardRef;
use crate::types::game_state::{
    printed_trigger_origin, ActionDisposition, GameState, PayCostKind, ReplacementChoiceKind,
    RetargetScope, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Increments the test-support meter; compiled out of every other build.
macro_rules! meter {
    ($($field:ident += $amount:expr),+ $(,)?) => {
        #[cfg(feature = "test-support")]
        crate::game::perf_counters::record_play_trace(|counters| {
            $(counters.$field += $amount;)+
        });
    };
}

thread_local! {
    /// How many action boundaries enclose the current point on this thread.
    static BOUNDARY_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// RAII guard held for one action boundary; restores the enclosing depth on drop.
#[must_use]
pub(crate) struct BoundaryDepth;

impl BoundaryDepth {
    pub(crate) fn enter() -> Self {
        let depth = BOUNDARY_DEPTH.with(|d| {
            let depth = d.get() + 1;
            d.set(depth);
            depth
        });
        if in_simulation_probe() {
            meter!(probe_entries += 1);
        } else if depth == 1 {
            meter!(boundary_entries += 1);
        } else {
            meter!(nested_applies += 1);
        }
        BoundaryDepth
    }
}

impl Drop for BoundaryDepth {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// The one gate every hook reads: a sampling mode, outside any probe, at the outermost boundary.
/// A deeper apply is the engine completing the outer choice, which replaying that choice
/// reproduces, so it records nothing.
pub(crate) fn recording(state: &GameState) -> bool {
    state.loop_detection.samples() && !in_simulation_probe() && BOUNDARY_DEPTH.with(Cell::get) == 1
}

/// CR 500.2: the window is the step (a phase without steps is one window); an empty stack does
/// not end it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowKey {
    turn: u32,
    phase: Phase,
    step_start: u32,
    /// How many turns have begun in the game; a skipped turn never begins.
    turns_begun: u32,
}

impl WindowKey {
    pub(crate) fn of(state: &GameState) -> Self {
        Self {
            turn: state.turn_number,
            phase: state.phase,
            step_start: state.steps_started_this_turn.count(state.phase),
            turns_begun: state.players.iter().map(|p| p.turns_taken).sum(),
        }
    }

    pub(crate) fn turn(self) -> u32 {
        self.turn
    }

    pub(crate) fn phase(self) -> Phase {
        self.phase
    }
}

/// How far past its step a period may run before it comes round again.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum PeriodReach {
    /// CR 500.2: within the step it began in.
    #[default]
    InStep,
    /// CR 500.8: into a later combat phase of the same turn.
    Combat,
    /// CR 500.7: into a later turn its controller takes.
    ExtraTurn,
}

impl PeriodReach {
    /// The reach of an Engine B candidate pumping `unbounded`; `None` for one that stays in a step.
    fn of_axes(unbounded: &[ResourceAxis]) -> Option<Self> {
        if unbounded.contains(&ResourceAxis::ExtraTurns) {
            Some(Self::ExtraTurn)
        } else if unbounded.contains(&ResourceAxis::CombatPhases) {
            Some(Self::Combat)
        } else {
            None
        }
    }

    /// Whether a span carried from a window of `from` still runs at `state`'s window `to`: a
    /// combat span within its turn (CR 500.8), an extra-turn span unless another player's turn
    /// began (CR 500.7).
    fn survives(self, seat: PlayerId, from: WindowKey, to: WindowKey, state: &GameState) -> bool {
        match self {
            Self::InStep => false,
            Self::Combat => to.turn == from.turn,
            // Counted, not read off the active player, because a turn can pass with no window
            // between `from` and `to`.
            Self::ExtraTurn => {
                to.turns_begun == from.turns_begun
                    || (state.active_player == seat && to.turns_begun == from.turns_begun + 1)
            }
        }
    }
}

/// Where a named span's entries live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanSource {
    /// The current window's trace.
    Window,
    /// The current window's named carried span at this index.
    Carried(usize),
}

/// A period begun in an earlier window: the entries from its key's last occurrence there on.
#[derive(Clone, Debug)]
struct CarriedSpan {
    key: NodeKey,
    seat: PlayerId,
    reach: PeriodReach,
    trace: PlayTrace,
}

/// A seat's printed faces outside its library, and the keys of the Engine B candidates over them
/// that run past a step, with each one's reach.
#[derive(Clone, Debug, Default)]
struct CarryingIndex {
    faces: Vec<String>,
    keys: Vec<(NodeKey, PeriodReach)>,
}

impl CarryingIndex {
    fn reach(&self, key: &NodeKey) -> Option<PeriodReach> {
        self.keys
            .iter()
            .filter(|(carried, _)| carried == key)
            .map(|(_, reach)| *reach)
            .max()
    }
}

/// The ability a play or resolution made, by its definition (CR 400.7: never the object).
#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeKey {
    /// CR 117.1a: a cast of the card with this printed identity.
    Cast(PrintedCardRef),
    /// CR 117.1b + CR 602.2: an activated ability, by its definition.
    Activated(Box<AbilityDefinition>),
    /// CR 305.6: a basic land type's intrinsic "{T}: Add [mana symbol]."
    IntrinsicMana(ManaType),
    /// An activated keyword ability whose definition is the keyword on the printed card.
    KeywordActivated {
        printed: PrintedCardRef,
        kind: KeywordActivation,
    },
    /// CR 603.3: a printed triggered ability, on the card and on every copy of it.
    Triggered(TriggerPrintedOrigin),
    /// CR 603.7a: a delayed triggered ability, by its creator and what it was created to do.
    Delayed(Box<DelayedAbilityOrigin>),
    /// Any other trigger occurrence keys only itself, so it names no repeat it cannot prove.
    TriggeredOccurrence(TriggerDefinitionRef),
    /// A play or resolution with no identity: the trace index it was recorded at.
    Unkeyed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeywordActivation {
    /// CR 702.6a: equip.
    Equip,
    /// CR 702.122a: crew.
    Crew,
    /// CR 702.184a: station.
    Station,
    /// CR 702.171a: saddle.
    Saddle,
    /// CR 702.49a: ninjutsu.
    Ninjutsu,
}

/// The live object a play's legality read asks about (an `ObjectId` persists across zones).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayLocus {
    Cast(ObjectId),
    Activate(ObjectId, usize),
    Mana(ObjectId, Option<usize>),
    /// No single-play authority reads it; it is named only by a repeat.
    Unread,
}

/// Whether the instruction an answer responds to let its chooser decline or choose none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerOptionality {
    Optional,
    Mandatory,
}

/// Whether an entry was made at priority or at another prompt, such as a mana ability activated
/// during a payment (CR 605.3a).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromptClass {
    Priority,
    Other,
}

/// Where an object-moving cost sends its objects, and the zones the moved objects arrived in
/// (CR 400.7), sorted; a replacement may send them elsewhere (CR 614.6).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CostMove {
    pub destination: Zone,
    /// `None` for an object that no longer exists.
    pub arrivals: Vec<Option<Zone>>,
}

/// Where an object-moving cost sends the objects it chooses; `None` for a cost that moves none.
fn cost_destination(kind: &PayCostKind) -> Option<Zone> {
    match kind {
        // CR 701.21a + CR 701.9a: sacrificed and discarded objects go to the graveyard.
        PayCostKind::Sacrifice | PayCostKind::Discard => Some(Zone::Graveyard),
        // CR 701.13a: an exiled object goes to exile.
        PayCostKind::ExileFromZone { .. }
        | PayCostKind::ExileMaterials { .. }
        | PayCostKind::ExilePermanent { .. }
        | PayCostKind::ExileFromManaZone { .. }
        | PayCostKind::ExileAggregate { .. }
        | PayCostKind::Behold {
            action: crate::types::ability::BeholdCostAction::ExileChosen,
        } => Some(Zone::Exile),
        PayCostKind::ReturnToHand => Some(Zone::Hand),
        PayCostKind::Reveal
        | PayCostKind::UnattachFrom { .. }
        | PayCostKind::RemoveCounter { .. }
        | PayCostKind::TapCreatures { .. }
        | PayCostKind::Behold {
            action: crate::types::ability::BeholdCostAction::ChooseOrReveal,
        } => None,
    }
}

/// The objects an object-moving cost prompt offers, with the zone each is in now.
#[derive(Clone, Debug)]
pub(crate) struct CostChoices {
    destination: Zone,
    choices: Vec<(ObjectId, Zone)>,
}

impl CostChoices {
    pub(crate) fn at(state: &GameState) -> Option<Self> {
        let WaitingFor::PayCost { kind, choices, .. } = &state.waiting_for else {
            return None;
        };
        Some(Self {
            destination: cost_destination(kind)?,
            choices: choices
                .iter()
                .filter_map(|id| state.objects.get(id).map(|o| (*id, o.zone)))
                .collect(),
        })
    }

    /// The move as it stands in `after`: every offered object no longer where it was.
    pub(crate) fn moved(&self, after: &GameState) -> CostMove {
        let mut arrivals: Vec<Option<Zone>> = self
            .choices
            .iter()
            .filter_map(|(id, zone)| {
                let now = after.objects.get(id).map(|o| o.zone);
                (now != Some(*zone)).then_some(now)
            })
            .collect();
        arrivals.sort_unstable();
        CostMove {
            destination: self.destination,
            arrivals,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntryKind {
    Play {
        action: GameAction,
        node: usize,
        locus: PlayLocus,
    },
    /// CR 603.3: a triggered ability resolving.
    Resolution { node: usize },
    Answer {
        action: GameAction,
        optional: AnswerOptionality,
        /// The triggered ability whose resolution asked it (CR 603.5 + CR 608.2d).
        asked_by: Option<usize>,
        /// Set when the answer chose the objects an object-moving cost moves.
        cost_move: Option<CostMove>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceEntry {
    pub seat: PlayerId,
    pub kind: EntryKind,
    /// The stack's size when the entry was made.
    pub depth: usize,
    pub prompt: PromptClass,
    /// The object-id counter when the entry was made, so a replay can tell an object minted
    /// since then from one that existed.
    pub next_object_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamingCause {
    /// CR 104.4b + CR 732.1b: a node repeated with an optional choice between its occurrences.
    Repeat,
    /// A triggered node is about to resolve again, and its resolution asked an optional answer, or
    /// an optional answer and another node's repeat were made since its previous instance resolved.
    TriggerTop,
    /// A play the period consumed is legal again.
    Restored,
    /// No play was consumed and this one is legal now.
    LegalNow,
}

/// A candidate period: the entries `start..end` of its source as they stood when it was named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedSpan {
    pub start: usize,
    pub end: usize,
    pub cause: NamingCause,
    pub source: SpanSource,
    pub reach: PeriodReach,
}

impl NamedSpan {
    fn in_window(start: usize, end: usize, cause: NamingCause) -> Self {
        Self {
            start,
            end,
            cause,
            source: SpanSource::Window,
            reach: PeriodReach::InStep,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingRead {
    node: usize,
    seat: PlayerId,
    locus: PlayLocus,
}

/// An action that reverses a play the trace holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reversal {
    /// CR 733.1: a cancel reverses the entire play in progress.
    Process,
    /// Untapping a manually tapped land reverses the latest mana ability it activated.
    ManaSource(ObjectId),
    /// Withdrawing a mana ability at its payment window reverses that activation alone, in the
    /// shape CR 733.1 gives a reversal: mana abilities activated meanwhile stand. The activation
    /// is named by its source and its `PendingManaAbility::chain_place`.
    ManaAbility(ObjectId, usize),
}

/// The trace as it stood before a play a later action may reverse.
#[derive(Clone, Debug)]
struct UndoPoint {
    undoes: Reversal,
    before: Box<PlayTrace>,
}

impl UndoPoint {
    /// An untap names the land alone, so it undoes whichever of its mana abilities this is.
    fn undone_by(&self, reversal: Reversal) -> bool {
        match (self.undoes, reversal) {
            (Reversal::ManaAbility(source, _), Reversal::ManaSource(untapped)) => {
                source == untapped
            }
            (made, asked) => made == asked,
        }
    }
}

/// Every play and answer of one window. `im` collections, so a state copy shares it.
#[derive(Clone, Debug)]
pub(crate) struct PlayTrace {
    window: WindowKey,
    entries: im::Vector<TraceEntry>,
    // ponytail: linear in the step's distinct nodes; bucket by a key hash if a step's node
    // count ever matters.
    nodes: im::Vector<NodeKey>,
    last: im::HashMap<usize, usize>,
    repeated: im::HashSet<usize>,
    last_optional: Option<usize>,
    last_play: Option<usize>,
    optional_triggers: im::HashSet<usize>,
    consumed: im::HashSet<usize>,
    pending_reads: im::Vector<PendingRead>,
    named: im::Vector<NamedSpan>,
    /// The first named span no window has read.
    unread: usize,
    /// The span the latest offer was made for.
    offered: Option<NamedSpan>,
    undo: im::Vector<UndoPoint>,
    /// CR 733.1: the mana abilities the engine reversed inside the action in progress.
    reversed: Vec<(ObjectId, usize)>,
    /// Each entry that activates a mana ability or answers one of its choices (CR 605.3b), with
    /// the ability it belongs to.
    mana_ability_entries: im::HashMap<usize, (ObjectId, usize)>,
    /// Spans carried from earlier windows, in the order their keys last occurred.
    carried: im::Vector<CarriedSpan>,
    /// Carried spans this window named, which a named span's `SpanSource::Carried` indexes.
    carried_named: im::Vector<CarriedSpan>,
    /// Each seat's carrying index, kept across windows.
    carrying: im::HashMap<PlayerId, CarryingIndex>,
}

impl PlayTrace {
    fn new(window: WindowKey) -> Self {
        Self {
            window,
            entries: im::Vector::new(),
            nodes: im::Vector::new(),
            last: im::HashMap::new(),
            repeated: im::HashSet::new(),
            last_optional: None,
            last_play: None,
            optional_triggers: im::HashSet::new(),
            consumed: im::HashSet::new(),
            pending_reads: im::Vector::new(),
            named: im::Vector::new(),
            unread: 0,
            offered: None,
            undo: im::Vector::new(),
            reversed: Vec::new(),
            mana_ability_entries: im::HashMap::new(),
            carried: im::Vector::new(),
            carried_named: im::Vector::new(),
            carrying: im::HashMap::new(),
        }
    }

    fn intern(&mut self, key: NodeKey) -> usize {
        if let Some(node) = self.find(&key) {
            return node;
        }
        self.nodes.push_back(key);
        self.nodes.len() - 1
    }

    fn find(&self, key: &NodeKey) -> Option<usize> {
        self.nodes.iter().position(|known| {
            meter!(node_key_compares += 1);
            known == key
        })
    }

    /// Appends an entry that plays or resolves `node`, naming the span back to its previous
    /// occurrence the first time it repeats with an optional choice in between.
    fn push_node_entry(&mut self, node: usize, entry: TraceEntry) {
        let at = self.entries.len();
        meter!(node_map_reads += 1, node_map_writes += 1);
        if let Some(&previous) = self.last.get(&node) {
            if !self.repeated.contains(&node)
                && self
                    .last_optional
                    .is_some_and(|optional| optional >= previous)
            {
                self.repeated.insert(node);
                self.named
                    .push_back(NamedSpan::in_window(previous, at, NamingCause::Repeat));
            }
        }
        self.last.insert(node, at);
        if let EntryKind::Play { locus, .. } = entry.kind {
            // CR 117.1a + CR 117.1b: casting a spell or activating an ability is optional.
            self.last_optional = Some(at);
            self.last_play = Some(at);
            if locus != PlayLocus::Unread {
                self.pending_reads.push_back(PendingRead {
                    node,
                    seat: entry.seat,
                    locus,
                });
            }
        }
        self.entries.push_back(entry);
    }

    /// Appends an entry whose nodes this trace has interned, keeping `prior`, this trace before
    /// anything of the entry was recorded, as where an untap of its mana source returns it.
    fn record(
        &mut self,
        prior: &PlayTrace,
        entry: TraceEntry,
        continues: Option<(ObjectId, usize)>,
    ) {
        self.carry(&entry, continues);
        self.append(Some(prior), entry, continues);
    }

    /// Appends `entry`, whose nodes are this trace's, to every live carried span.
    fn carry(&mut self, entry: &TraceEntry, continues: Option<(ObjectId, usize)>) {
        let nodes = &self.nodes;
        for span in self.carried.iter_mut() {
            meter!(carry_appends += 1);
            let entry = span.trace.reinterned(nodes, entry.clone());
            span.trace.append(None, entry, continues);
        }
    }

    /// `entry` with each node of `nodes` it names interned here.
    fn reinterned(&mut self, nodes: &im::Vector<NodeKey>, mut entry: TraceEntry) -> TraceEntry {
        let reintern = |trace: &mut Self, node: usize| {
            let key = match &nodes[node] {
                NodeKey::Unkeyed(_) => NodeKey::Unkeyed(trace.entries.len()),
                key => key.clone(),
            };
            trace.intern(key)
        };
        match &mut entry.kind {
            EntryKind::Play { node, .. } | EntryKind::Resolution { node } => {
                *node = reintern(self, *node);
            }
            EntryKind::Answer { asked_by, .. } => {
                *asked_by = asked_by.map(|node| reintern(self, node));
            }
        }
        entry
    }

    /// Appends an entry whose nodes this trace has interned; `prior`, when given, is kept as where
    /// an untap of its mana source returns the trace. `continues` names the mana-ability
    /// activation the entry begins or continues.
    fn append(
        &mut self,
        prior: Option<&PlayTrace>,
        entry: TraceEntry,
        continues: Option<(ObjectId, usize)>,
    ) {
        let mana_play = match entry.kind {
            EntryKind::Play {
                locus: PlayLocus::Mana(..),
                ..
            } => continues,
            _ => None,
        };
        if let Some(ability) = continues {
            self.mana_ability_entries
                .insert(self.entries.len(), ability);
        }
        let undo = mana_play
            .zip(prior)
            .map(|((source, place), prior)| UndoPoint {
                undoes: Reversal::ManaAbility(source, place),
                before: Box::new(prior.clone()),
            });
        match &entry.kind {
            EntryKind::Play { node, .. } | EntryKind::Resolution { node } => {
                self.push_node_entry(*node, entry);
            }
            EntryKind::Answer {
                optional, asked_by, ..
            } => {
                if *optional == AnswerOptionality::Optional {
                    self.last_optional = Some(self.entries.len());
                    // CR 603.5: a trigger whose resolution asked an optional answer.
                    if let Some(node) = asked_by {
                        self.optional_triggers.insert(*node);
                    }
                }
                self.entries.push_back(entry);
            }
        }
        if let Some(undo) = undo {
            self.undo.push_back(undo);
        }
    }

    /// Keeps `before`, this trace before the entry that began the play in progress, as where a
    /// cancel returns it.
    fn begin_play(&mut self, before: Box<PlayTrace>) {
        self.undo.retain(|point| point.undoes != Reversal::Process);
        self.undo.push_back(UndoPoint {
            undoes: Reversal::Process,
            before,
        });
    }

    /// Records the entry at `at` of the trace `from` again, re-interning its nodes here.
    fn rerecord(&mut self, from: &PlayTrace, at: usize, entry: TraceEntry, begins_play: bool) {
        let prior = self.clone();
        let entry = self.reinterned(&from.nodes, entry);
        self.record(&prior, entry, from.mana_ability_entries.get(&at).copied());
        if begins_play {
            self.begin_play(Box::new(prior));
        }
    }
}

fn trace_mut(state: &mut GameState) -> &mut PlayTrace {
    roll(state);
    let window = WindowKey::of(state);
    state
        .play_trace
        .get_or_insert_with(|| Box::new(PlayTrace::new(window)))
}

/// CR 500.2: the first touch of a new window ends the old one's trace. Each node of the old
/// window whose key its seat's carrying index carries begins a carried span from its last
/// occurrence there, and a carried span runs on while its reach admits the new window and its key
/// is still carried.
fn roll(state: &mut GameState) {
    let window = WindowKey::of(state);
    let Some(old) = state
        .play_trace
        .take_if(|trace| trace.window != window)
        .map(|trace| *trace)
    else {
        return;
    };
    let mut next = PlayTrace::new(window);
    next.carrying = old.carrying.clone();
    let mut seats: Vec<PlayerId> = old
        .last
        .values()
        .map(|&at| old.entries[at].seat)
        .chain(old.carried.iter().map(|span| span.seat))
        .collect();
    seats.sort_unstable();
    seats.dedup();
    for seat in seats {
        refresh_carrying(state, &mut next.carrying, seat);
    }
    let runs = |span: &CarriedSpan, carrying: &im::HashMap<PlayerId, CarryingIndex>| {
        span.reach.survives(span.seat, old.window, window, state)
            && carrying
                .get(&span.seat)
                .and_then(|index| index.reach(&span.key))
                .is_some()
    };
    next.carried = old
        .carried
        .iter()
        .filter(|span| runs(span, &next.carrying))
        .cloned()
        .collect();
    let mut last: Vec<(usize, usize)> = old.last.iter().map(|(&node, &at)| (at, node)).collect();
    last.sort_unstable();
    for (at, node) in last {
        meter!(step_end_lookups += 1);
        let seat = old.entries[at].seat;
        let key = &old.nodes[node];
        let Some(reach) = next.carrying.get(&seat).and_then(|index| index.reach(key)) else {
            continue;
        };
        let mut trace = PlayTrace::new(old.window);
        for (from, entry) in old.entries.iter().enumerate().skip(at) {
            trace.rerecord(&old, from, entry.clone(), false);
        }
        let span = CarriedSpan {
            key: key.clone(),
            seat,
            reach,
            trace,
        };
        next.carried
            .retain(|carried| carried.seat != seat || carried.key != *key);
        if runs(&span, &next.carrying) {
            next.carried.push_back(span);
        }
    }
    if !next.carried.is_empty() || !next.carrying.is_empty() {
        state.play_trace = Some(Box::new(next));
    }
}

/// Rebuilds `seat`'s carrying index with one Engine B run when its printed faces outside its
/// library changed.
fn refresh_carrying(
    state: &GameState,
    carrying: &mut im::HashMap<PlayerId, CarryingIndex>,
    seat: PlayerId,
) {
    let mut faces: std::collections::BTreeMap<&str, &GameObject> =
        std::collections::BTreeMap::new();
    for object in state.objects.values() {
        meter!(face_set_scans += 1);
        if (object.owner == seat || object.controller == seat) && object.zone != Zone::Library {
            faces.entry(object.base_name.as_str()).or_insert(object);
        }
    }
    let names: Vec<String> = faces.keys().map(|name| (*name).to_string()).collect();
    if carrying
        .get(&seat)
        .is_some_and(|index| index.faces == names)
    {
        return;
    }
    meter!(engine_b_rebuilds += 1);
    let objects: Vec<&GameObject> = faces.into_values().collect();
    let sources: Vec<AbilitySource<'_>> = objects
        .iter()
        .map(|object| AbilitySource {
            name: &object.base_name,
            mana_cost: &object.base_mana_cost,
            card_type: &object.base_card_types,
            abilities: &object.base_abilities,
            triggers: &object.base_trigger_definitions,
            replacements: &object.base_replacement_definitions,
            statics: &object.base_static_definitions,
        })
        .collect();
    let mut keys = Vec::new();
    for cycle in candidate_cycles_from_sources(&sources) {
        let Some(reach) = PeriodReach::of_axes(&cycle.unbounded) else {
            continue;
        };
        keys.extend(
            cycle
                .members
                .iter()
                .filter_map(|origin| origin_key(objects[origin.source], origin.slot))
                .map(|key| (key, reach)),
        );
    }
    carrying.insert(seat, CarryingIndex { faces: names, keys });
}

/// The trace's key for an Engine B node: a spell's cast, an activated ability's definition, or a
/// printed trigger; a replacement or a granted ability keys nothing the trace records.
fn origin_key(object: &GameObject, slot: AbilitySlot) -> Option<NodeKey> {
    match slot {
        AbilitySlot::Ability(index) => {
            let def = object.base_abilities.get(index)?;
            match def.kind {
                AbilityKind::Spell => object.base_printed_ref.clone().map(NodeKey::Cast),
                _ => Some(NodeKey::Activated(Box::new(def.clone()))),
            }
        }
        AbilitySlot::Trigger(index) => {
            printed_trigger_origin(object, index).map(NodeKey::Triggered)
        }
        AbilitySlot::Replacement(_) | AbilitySlot::Granted { .. } => None,
    }
}

/// Whether a play is in progress: the engine withholds priority for it (CR 601.2 + CR 602.2b +
/// CR 704.3), or its maker is choosing how to make it before it is announced.
fn play_in_progress(state: &GameState) -> bool {
    state.withholds_priority() || state.waiting_for.chooses_play_before_announcement()
}

/// A player's play: a spell cast or an ability activated.
enum Play {
    Cast(ObjectId),
    /// CR 707.12: a cast of a copy of the card `source` represents.
    CastCopy(ObjectId),
    Activate(ObjectId, usize),
    Mana(ObjectId, Option<usize>, ManaType),
    Keyword(ObjectId, KeywordActivation),
}

enum Choice {
    Play(Play),
    Answer(Answer),
    Reverse(Reversal),
    /// A preference or permission, not a game choice.
    Setting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Passing priority, the shortcut and Resolve All negotiations (CR 732.2a), or a concession:
    /// never a choice that stops a loop of mandatory actions (CR 104.4b).
    Pass,
    /// Any other answer.
    Take,
}

/// What a submitted action is. Casts and activations are plays (CR 117.1a, CR 117.1b), mana
/// abilities included whether activated at priority or during a payment (CR 605.3a); special
/// actions (CR 116.2) and every other choice are answers.
fn classify(action: &GameAction, prompt: &WaitingFor) -> Choice {
    let at_priority = matches!(prompt, WaitingFor::Priority { .. });
    match action {
        GameAction::CastSpell { object_id, .. }
        | GameAction::CastSpellForFree { object_id, .. }
        | GameAction::CastSpellAsMiracle { object_id, .. }
        | GameAction::CastSpellAsMadness { object_id, .. }
        // CR 702.37c: casting a card face down is a cast.
        | GameAction::PlayFaceDown { object_id, .. } => Choice::Play(Play::Cast(*object_id)),
        GameAction::CastSpellAsSneak { hand_object, .. }
        | GameAction::CastSpellAsWebSlinging { hand_object, .. } => {
            Choice::Play(Play::Cast(*hand_object))
        }
        GameAction::CastPreparedCopy { source } | GameAction::CastParadigmCopy { source } => {
            Choice::Play(Play::CastCopy(*source))
        }
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => Choice::Play(Play::Activate(*source_id, *ability_index)),
        GameAction::TapLandForMana { selection } | GameAction::ActivateManaSource { selection } => {
            Choice::Play(Play::Mana(
                selection.source.object_id,
                selection.ability_index,
                selection.mana_type,
            ))
        }
        GameAction::ActivateNinjutsu {
            ninjutsu_object_id, ..
        } => Choice::Play(Play::Keyword(*ninjutsu_object_id, KeywordActivation::Ninjutsu)),
        // The keyword's own prompt (which creatures, which target) is answered with the same
        // action, so only the one made at priority is the activation.
        GameAction::Equip { equipment_id, .. } if at_priority => {
            Choice::Play(Play::Keyword(*equipment_id, KeywordActivation::Equip))
        }
        GameAction::CrewVehicle { vehicle_id, .. } if at_priority => {
            Choice::Play(Play::Keyword(*vehicle_id, KeywordActivation::Crew))
        }
        GameAction::ActivateStation { spacecraft_id, .. } if at_priority => {
            Choice::Play(Play::Keyword(*spacecraft_id, KeywordActivation::Station))
        }
        GameAction::SaddleMount { mount_id, .. } if at_priority => {
            Choice::Play(Play::Keyword(*mount_id, KeywordActivation::Saddle))
        }
        // At a mana ability's payment window a cancel withdraws the innermost activation alone.
        GameAction::CancelCast => Choice::Reverse(
            prompt
                .suspended_mana_abilities()
                .next()
                .map_or(Reversal::Process, |begun| {
                    Reversal::ManaAbility(begun.source_id, begun.chain_place())
                }),
        ),
        GameAction::UntapLandForMana { object_id } => {
            Choice::Reverse(Reversal::ManaSource(*object_id))
        }
        GameAction::SetAutoPass { .. }
        | GameAction::CancelAutoPass
        | GameAction::SetPhaseStops { .. }
        | GameAction::SetPriorityPassingMode { .. }
        | GameAction::SetPriorityYield { .. }
        | GameAction::SetMayTriggerAutoChoice { .. }
        | GameAction::SetReplacementAutoChoice { .. }
        | GameAction::SetTriggerOrderTemplate { .. }
        | GameAction::Debug(_)
        | GameAction::GrantDebugPermission { .. }
        | GameAction::RevokeDebugPermission { .. }
        // CR 402.3: arranging one's hand has no game significance.
        | GameAction::ReorderHand { .. } => Choice::Setting,
        GameAction::Equip { .. }
        | GameAction::CrewVehicle { .. }
        | GameAction::ActivateStation { .. }
        | GameAction::SaddleMount { .. }
        | GameAction::ChooseMeldPair { .. }
        | GameAction::ChooseEntryAttackTarget { .. }
        | GameAction::PlayLand { .. }
        | GameAction::Foretell { .. }
        | GameAction::DeclareAttackers { .. }
        | GameAction::DeclareBlockers { .. }
        | GameAction::ChooseUntap { .. }
        | GameAction::ChooseExert { .. }
        | GameAction::ChooseEnlist { .. }
        | GameAction::ChooseClashOpponent { .. }
        | GameAction::ChooseZoneOpponentChooser { .. }
        | GameAction::ChoosePileOpponent { .. }
        | GameAction::ChooseAnnouncingOpponent { .. }
        | GameAction::ChooseGiftRecipient { .. }
        | GameAction::ChooseAssistPlayer { .. }
        | GameAction::CommitAssistPayment { .. }
        | GameAction::MulliganDecision { .. }
        | GameAction::BackToManaPayment
        | GameAction::SpendPoolMana { .. }
        | GameAction::UnspendPoolMana { .. }
        | GameAction::SelectCards { .. }
        | GameAction::ChooseRemoveCounterCostDistribution { .. }
        | GameAction::SelectCoinFlips { .. }
        | GameAction::SelectDieRolls { .. }
        | GameAction::ChooseOutsideGameCards { .. }
        | GameAction::SelectTargets { .. }
        | GameAction::ChooseTarget { .. }
        | GameAction::ChooseReplacement { .. }
        | GameAction::ChooseReplacementAndRemember { .. }
        | GameAction::ChooseEntryController { .. }
        | GameAction::OrderTriggers { .. }
        | GameAction::OrderCostReductions { .. }
        | GameAction::Transform { .. }
        | GameAction::TurnFaceUp { .. }
        | GameAction::SubmitSideboard { .. }
        | GameAction::ChoosePlayDraw { .. }
        | GameAction::ChooseOption { .. }
        | GameAction::SubmitVoteCandidate { .. }
        | GameAction::SubmitSpellbookDraft { .. }
        | GameAction::SubmitPilePartition { .. }
        | GameAction::ChoosePile { .. }
        | GameAction::ChooseBranch { .. }
        | GameAction::SubmitLifeRedistribution { .. }
        | GameAction::ChooseDamageSource { .. }
        | GameAction::SelectModes { .. }
        | GameAction::DecideOptionalCost { .. }
        | GameAction::ChooseAdventureFace { .. }
        | GameAction::ChooseModalFace { .. }
        | GameAction::ChooseAlternativeCast { .. }
        | GameAction::ChooseCastingVariant { .. }
        | GameAction::KeepAllCopyTargets
        | GameAction::ChoosePermanentTypeSlot { .. }
        | GameAction::DecideOptionalEffect { .. }
        | GameAction::ChooseResolutionOptionalPaymentBranch { .. }
        | GameAction::RespondToSpliceOffer { .. }
        | GameAction::DecideOptionalEffectAndRemember { .. }
        | GameAction::PayUnlessCost { .. }
        | GameAction::ChooseUnlessCostBranch { .. }
        | GameAction::ChooseActivationCostBranch { .. }
        | GameAction::PayCombatTax { .. }
        | GameAction::ChooseRingBearer { .. }
        | GameAction::ChoosePair { .. }
        | GameAction::ChooseDungeon { .. }
        | GameAction::ChooseDungeonRoom { .. }
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::RollPlanarDie
        | GameAction::ChooseRoomDoor { .. }
        | GameAction::TapForConvoke { .. }
        | GameAction::HarmonizeTap { .. }
        | GameAction::DeclareCompanion { .. }
        | GameAction::CompanionToHand
        | GameAction::DiscoverChoice { .. }
        | GameAction::GraveyardPaidCastChoice { .. }
        | GameAction::CascadeChoice { .. }
        | GameAction::RippleChoice { .. }
        | GameAction::FreeCastWindowChoice { .. }
        | GameAction::ChooseTopOrBottom { .. }
        | GameAction::ChooseMutateMergeSide { .. }
        | GameAction::CipherEncode { .. }
        | GameAction::ChooseLegend { .. }
        | GameAction::ChooseBattleProtector { .. }
        | GameAction::AssignCombatDamage { .. }
        | GameAction::AssignBlockerDamage { .. }
        | GameAction::DistributeAmong { .. }
        | GameAction::ChooseCounterMoveDistribution { .. }
        | GameAction::ChooseCountersToRemove { .. }
        | GameAction::SubmitPayAmount { .. }
        | GameAction::RetargetSpell { .. }
        | GameAction::LearnDecision { .. }
        | GameAction::SelectCategoryPermanents { .. }
        | GameAction::ChooseKeptCreatures { .. }
        | GameAction::ChooseKeptPermanents { .. }
        | GameAction::ChooseX { .. }
        | GameAction::SubmitPhyrexianChoices { .. }
        | GameAction::ChooseManaColor { .. }
        | GameAction::PayManaAbilityMana { .. }
        | GameAction::ChooseSpecializeColor { .. }
        | GameAction::PassParadigmOffer
        | GameAction::EndContinuousEffect { .. } => Choice::Answer(Answer::Take),
        GameAction::PassPriority
        | GameAction::Concede { .. }
        | GameAction::DeclareShortcut { .. }
        | GameAction::RespondToShortcut { .. }
        | GameAction::DeclineShortcut
        | GameAction::PrecastCopyShortcut { .. }
        | GameAction::BeginResolveAll { .. }
        | GameAction::RespondResolveAllConsent { .. }
        | GameAction::RevokeResolveAllConsent { .. } => Choice::Answer(Answer::Pass),
    }
}

/// Whether an answer was one its chooser could decline — a "may" (CR 603.5), an optional cost
/// (CR 601.2b), "any number" or "up to" — which is a property of the prompt and the answer: at
/// priority, any answer but a pass is an action the player may take instead (CR 116.2, CR 117.1).
fn answer_optionality(prompt: &WaitingFor, answer: Answer) -> AnswerOptionality {
    use AnswerOptionality::{Mandatory, Optional};
    let optional_if = |optional: bool| if optional { Optional } else { Mandatory };
    if answer == Answer::Pass {
        return Mandatory;
    }
    match prompt {
        WaitingFor::Priority { .. } => Optional,
        // CR 603.5 + CR 608.2d: "may" instructions and decline-able offers.
        WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::ResolutionOptionalPaymentChoice { .. }
        | WaitingFor::RepeatDecision { .. }
        | WaitingFor::RevealUntilKeptChoice { .. }
        | WaitingFor::CastOffer { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::RippleRevealChoice { .. }
        | WaitingFor::CipherEncodeChoice { .. }
        | WaitingFor::PairChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::AssistPayment { .. }
        | WaitingFor::ExertChoice { .. }
        | WaitingFor::EnlistChoice { .. }
        | WaitingFor::UntapChoice { .. }
        | WaitingFor::CopyRetarget { .. }
        | WaitingFor::CombatTaxPayment { .. }
        // CR 103.5: a player may take a mulligan.
        | WaitingFor::MulliganDecision { .. }
        // CR 605.3a: a mana ability is never required; declining returns to the payment step.
        | WaitingFor::ManaSourceSelection { .. } => Optional,
        // CR 601.2b + CR 118.9: optional additional and alternative costs.
        WaitingFor::OptionalCostChoice { .. } | WaitingFor::AlternativeCastChoice { .. } => {
            Optional
        }
        // CR 118.12: the player chooses whether to pay an "unless" cost.
        WaitingFor::UnlessPayment { .. } | WaitingFor::UnlessPaymentChooseCost { .. } => Optional,
        // CR 508.1a + CR 509.1a: attackers and blockers are chosen "if any".
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. } => Optional,
        // CR 107.1c + CR 701.22a + CR 701.25a + CR 701.34a + CR 701.56a: "any number".
        WaitingFor::ScryChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::TimeTravelChoice { .. }
        | WaitingFor::MoveCountersDistribution { .. }
        | WaitingFor::ChooseUntapSubset { .. }
        | WaitingFor::RedistributeLifeTotals { .. }
        | WaitingFor::KeepWithinTotalPowerChoice { .. }
        | WaitingFor::RemoveCountersChoice { .. } => Optional,
        // CR 115.7d: choosing new targets may leave every target unchanged.
        WaitingFor::RetargetChoice { scope, .. } => optional_if(*scope == RetargetScope::All),
        WaitingFor::ReplacementChoice { kind, .. } => {
            optional_if(*kind == ReplacementChoiceKind::OptionalBranch)
        }
        WaitingFor::TargetSelection { target_slots, .. }
        | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            optional_if(target_slots.iter().any(|slot| slot.optional))
        }
        WaitingFor::ModeChoice { modal, .. } | WaitingFor::AbilityModeChoice { modal, .. } => {
            optional_if(modal.min_choices == 0)
        }
        WaitingFor::MultiTargetSelection { min_targets, .. } => optional_if(*min_targets == 0),
        WaitingFor::ChooseObjectsSelection { min, .. }
        | WaitingFor::EachPlayerCopyChosenSelection { min, .. } => optional_if(*min == 0),
        WaitingFor::PayAmountChoice { min, .. } => optional_if(*min == 0),
        WaitingFor::PayCost { min_count, .. }
        | WaitingFor::DrawnThisTurnTopdeckChoice { min_count, .. } => optional_if(*min_count == 0),
        WaitingFor::EffectZoneChoice {
            up_to, min_count, ..
        } => optional_if(*up_to || *min_count == 0),
        WaitingFor::DigChoice {
            up_to, keep_count, ..
        } => optional_if(*up_to || *keep_count == 0),
        // CR 701.23b: a search for a stated quality need not find.
        WaitingFor::SearchChoice {
            up_to,
            allows_partial_find,
            ..
        } => optional_if(*up_to || *allows_partial_find),
        WaitingFor::OutsideGameChoice { up_to, .. }
        | WaitingFor::ChooseFromZoneChoice { up_to, .. }
        | WaitingFor::DiscardChoice { up_to, .. } => optional_if(*up_to),
        WaitingFor::RevealChoice { optional, .. } => optional_if(*optional),
        // CR 732.2a + CR 104.4b: the shortcut and Resolve All negotiations are not instructions.
        WaitingFor::ResolveAllConsent { .. }
        | WaitingFor::ResolveAllReady { .. }
        | WaitingFor::LoopShortcut { .. }
        | WaitingFor::RespondToShortcut { .. }
        | WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. } => Mandatory,
        // CR 701.44d: the next permanent to explore is chosen, never declined.
        WaitingFor::ExploreChoice { .. }
        // CR 701.30c: each clashing player puts their card on top or on the bottom.
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::EntryAttackTargetChoice { .. }
        | WaitingFor::OpeningHandBottomCards { .. }
        | WaitingFor::ManaPayment { .. }
        | WaitingFor::ChooseXValue { .. }
        | WaitingFor::GameOver { .. }
        | WaitingFor::EntryControllerChoice { .. }
        | WaitingFor::OrderTriggers { .. }
        | WaitingFor::SpellCopyOrderChoice { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ReturnAsAuraTarget { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::CrewVehicle { .. }
        | WaitingFor::StationTarget { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::RippleBottomOrder { .. }
        | WaitingFor::RevealUntilBottomOrder { .. }
        | WaitingFor::ArrangePlanarDeckTopChoice { .. }
        | WaitingFor::CoinFlipKeepChoice { .. }
        | WaitingFor::DieKeepChoice { .. }
        | WaitingFor::DigRestSplitChoice { .. }
        | WaitingFor::SearchPartitionChoice { .. }
        | WaitingFor::BeholdChoice { .. }
        | WaitingFor::EmpowerJaceChoice { .. }
        | WaitingFor::ChooseOneOfBranch { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::NamedChoice { .. }
        | WaitingFor::OpponentGuess { .. }
        | WaitingFor::SpellbookDraft { .. }
        | WaitingFor::DamageSourceChoice { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::ChooseGiftRecipient { .. }
        | WaitingFor::OrderCostReductions { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::MutateMergeChoice { .. }
        | WaitingFor::CastingVariantChoice { .. }
        | WaitingFor::ChoosePermanentTypeSlot { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::UnlessBounceChoice { .. }
        | WaitingFor::ChooseRingBearer { .. }
        | WaitingFor::ChooseRoomDoor { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::SpecializeColor { .. }
        | WaitingFor::ActivationCostOneOfChoice { .. }
        | WaitingFor::CostTypeChoice { .. }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::PayManaAbilityMana { .. }
        | WaitingFor::ManaAbilityManaPayment { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashChooseOpponent { .. }
        | WaitingFor::ChooseFromZoneOpponentChooser { .. }
        | WaitingFor::ChooseAnnouncingOpponent { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::SeparatePilesChooseOpponent { .. }
        | WaitingFor::SeparatePilesPartition { .. }
        | WaitingFor::SeparatePilesChoice { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::CategoryChoice { .. }
        | WaitingFor::KeepExactPermanentsChoice { .. }
        | WaitingFor::AssignCombatDamage { .. }
        | WaitingFor::AssignBlockerDamage { .. }
        | WaitingFor::DistributeAmong { .. }
        | WaitingFor::PhyrexianPayment { .. } => Mandatory,
    }
}

/// The node and legality locus of a play, read from the state before the play is made.
fn play_node(state: &GameState, play: &Play, at: usize) -> (NodeKey, PlayLocus) {
    let printed = |id: ObjectId| state.objects.get(&id).and_then(|o| o.printed_ref.clone());
    let activated = |id: ObjectId, index: usize| {
        state
            .objects
            .get(&id)
            .and_then(|o| o.abilities.get(index))
            .map(|def| {
                let locus = if crate::game::mana_abilities::is_mana_ability(def) {
                    PlayLocus::Mana(id, Some(index))
                } else {
                    PlayLocus::Activate(id, index)
                };
                (NodeKey::Activated(Box::new(def.clone())), locus)
            })
            .unwrap_or((NodeKey::Unkeyed(at), PlayLocus::Unread))
    };
    match *play {
        Play::Cast(id) => (
            printed(id).map_or(NodeKey::Unkeyed(at), NodeKey::Cast),
            PlayLocus::Cast(id),
        ),
        Play::CastCopy(source) => (
            printed(source).map_or(NodeKey::Unkeyed(at), NodeKey::Cast),
            PlayLocus::Unread,
        ),
        Play::Activate(id, index) | Play::Mana(id, Some(index), _) => activated(id, index),
        Play::Mana(id, None, mana_type) => {
            (NodeKey::IntrinsicMana(mana_type), PlayLocus::Mana(id, None))
        }
        Play::Keyword(id, kind) => (
            printed(id).map_or(NodeKey::Unkeyed(at), |printed| NodeKey::KeywordActivated {
                printed,
                kind,
            }),
            PlayLocus::Unread,
        ),
    }
}

/// The node of a triggered stack entry; `None` when the entry is not a triggered ability.
fn trigger_node(state: &GameState, entry: &StackEntry, at: usize) -> Option<NodeKey> {
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        return None;
    };
    if let Some(origin) = &ability.delayed_origin {
        return Some(NodeKey::Delayed(origin.clone()));
    }
    let Some(definition_ref) = &ability.trigger_definition_ref else {
        return Some(NodeKey::Unkeyed(at));
    };
    let origin = match &definition_ref.occurrence {
        TriggerDefinitionOccurrenceRef::Printed { printed_index, .. } => state
            .objects
            .get(&definition_ref.source.object_id)
            .and_then(|source| printed_trigger_origin(source, *printed_index)),
        TriggerDefinitionOccurrenceRef::CopiedValue { printed_origin, .. } => {
            printed_origin.clone()
        }
        TriggerDefinitionOccurrenceRef::KeywordCompanion { .. }
        | TriggerDefinitionOccurrenceRef::CopyRetained { .. }
        | TriggerDefinitionOccurrenceRef::Granted { .. }
        | TriggerDefinitionOccurrenceRef::ExpandedGrant { .. }
        | TriggerDefinitionOccurrenceRef::Unmaterialized => None,
    };
    Some(origin.map_or_else(
        || NodeKey::TriggeredOccurrence(definition_ref.clone()),
        NodeKey::Triggered,
    ))
}

/// The pre-action trace, restored when the action does not apply, and the reversal the action
/// makes when it does.
pub(crate) struct TraceSnapshot {
    before: Option<Box<PlayTrace>>,
    reverses: Option<Reversal>,
    /// The trace before the action's entry, when no play was in progress before it.
    opening: Option<Box<PlayTrace>>,
    /// The entry of an answer to an object-moving cost, and what the cost offered.
    cost: Option<(usize, CostChoices)>,
}

/// Records a player's game choice at the outermost action boundary, before it runs.
pub(crate) fn begin_action(
    state: &mut GameState,
    seat: PlayerId,
    action: &GameAction,
) -> Option<TraceSnapshot> {
    if !state.loop_detection.samples() {
        state.play_trace = None;
        return None;
    }
    if !recording(state) {
        return None;
    }
    let choice = classify(action, &state.waiting_for);
    if matches!(choice, Choice::Setting) {
        return None;
    }
    metered(|| {
        let before = state.play_trace.clone();
        meter!(actions_recorded += 1);
        // Every snapshot a reversal restores is this trace before the entry touches any field.
        let prior = trace_mut(state).clone();
        let at = prior.entries.len();
        let depth = state.stack.len();
        let prompt = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            PromptClass::Priority
        } else {
            PromptClass::Other
        };
        let next_object_id = state.next_object_id;
        let mut cost = None;
        let (entry, continues) = match choice {
            Choice::Reverse(reversal) => {
                return Some(TraceSnapshot {
                    before,
                    reverses: Some(reversal),
                    opening: None,
                    cost: None,
                });
            }
            Choice::Play(play) => {
                let (key, locus) = play_node(state, &play, at);
                let node = trace_mut(state).intern(key);
                let entry = TraceEntry {
                    seat,
                    kind: EntryKind::Play {
                        action: action.clone(),
                        node,
                        locus,
                    },
                    depth,
                    prompt,
                    next_object_id,
                };
                // It will stand above every activation suspended at the prompt it begins at.
                let begins = match locus {
                    PlayLocus::Mana(source, _) => {
                        Some((source, state.waiting_for.suspended_mana_abilities().count()))
                    }
                    PlayLocus::Cast(_) | PlayLocus::Activate(..) | PlayLocus::Unread => None,
                };
                (entry, begins)
            }
            Choice::Answer(answer) => {
                let optional = answer_optionality(&state.waiting_for, answer);
                let asking = state
                    .resolving_stack_entry
                    .as_ref()
                    .and_then(|entry| trigger_node(state, entry, at));
                cost = CostChoices::at(state).map(|choices| (at, choices));
                let trace = trace_mut(state);
                let asked_by = asking.map(|key| trace.intern(key));
                let entry = TraceEntry {
                    seat,
                    kind: EntryKind::Answer {
                        action: action.clone(),
                        optional,
                        asked_by,
                        cost_move: None,
                    },
                    depth,
                    prompt,
                    next_object_id,
                };
                let continues = state
                    .waiting_for
                    .continued_mana_ability()
                    .map(|pending| (pending.source_id, pending.chain_place()));
                (entry, continues)
            }
            Choice::Setting => return None,
        };
        let opening = (!play_in_progress(state)).then(|| Box::new(prior.clone()));
        trace_mut(state).record(&prior, entry, continues);
        Some(TraceSnapshot {
            before,
            reverses: None,
            opening,
            cost,
        })
    })
}

/// CR 733.1: the engine reversed this mana ability's activation inside the action in progress.
pub(crate) fn mana_ability_reversed(state: &mut GameState, source: ObjectId, place: usize) {
    if let Some(trace) = state.play_trace.as_deref_mut() {
        trace.reversed.push((source, place));
    }
}

/// Restores the trace when the action did not apply; applies the reversal when it did.
pub(crate) fn end_action(
    state: &mut GameState,
    snapshot: Option<TraceSnapshot>,
    disposition: Option<ActionDisposition>,
) {
    let Some(snapshot) = snapshot else {
        return;
    };
    match disposition {
        None => {
            state.play_trace = snapshot.before;
            return;
        }
        // CR 733.1: an action that can't be legally completed is reversed entire, as a cancel is.
        Some(ActionDisposition::Reversed) => {
            state.play_trace = snapshot.before;
            reverse(state, Reversal::Process);
            return;
        }
        Some(ActionDisposition::Applied) => {}
    }
    // Taken before the action's own reversal installs a trace restored from an undo point.
    let reversed = state
        .play_trace
        .as_deref_mut()
        .map(|trace| std::mem::take(&mut trace.reversed))
        .unwrap_or_default();
    if let Some(reversal) = snapshot.reverses {
        reverse(state, reversal);
    }
    let began_play = play_in_progress(state);
    let cost_move = snapshot
        .cost
        .map(|(at, choices)| (at, choices.moved(state)));
    if let Some(trace) = state.play_trace.as_deref_mut() {
        if let Some((at, moved)) = cost_move {
            if let Some(TraceEntry {
                kind: EntryKind::Answer { cost_move, .. },
                ..
            }) = trace.entries.get_mut(at)
            {
                *cost_move = Some(moved);
            }
        }
    }
    // Applied after the stamp, which indexes the action's entry in the unreversed trace.
    for (source, place) in reversed {
        reverse(state, Reversal::ManaAbility(source, place));
    }
    let tapped = &state.lands_tapped_for_mana;
    if let Some(trace) = state.play_trace.as_deref_mut() {
        // A play begins at the action after which one is in progress, whatever prompt that
        // action answered; the engine's state, not the prompt, says which action that was.
        if let Some(before) = snapshot.opening.filter(|_| began_play) {
            trace.begin_play(before);
        }
        // A mana ability stays reversible while the engine still tracks its land's tap, or while
        // a play is in progress whose payment window a cancel may yet withdraw it from.
        trace.undo.retain(|point| match point.undoes {
            Reversal::Process => true,
            // An untap's request; no point is kept under it.
            Reversal::ManaSource(_) => false,
            Reversal::ManaAbility(source, _) => {
                began_play || tapped.values().any(|ids| ids.contains(&source))
            }
        });
    }
}

/// Restores the trace to its state before the reversed play and records again what came after it
/// that the reversal leaves standing; a cancel leaves only the mana abilities activated while
/// making the play and their choices, which the engine does not reverse (CR 733.1), and a
/// withdrawn mana ability leaves everything but its own answers.
fn reverse(state: &mut GameState, reversal: Reversal) {
    let Some(trace) = state.play_trace.as_deref() else {
        return;
    };
    let Some(point) = trace
        .undo
        .iter()
        .rev()
        .find(|point| point.undone_by(reversal))
    else {
        return;
    };
    let process_at = trace
        .undo
        .iter()
        .find(|point| point.undoes == Reversal::Process)
        .map(|point| point.before.entries.len());
    let reversed_at = point.before.entries.len();
    let mut restored = (*point.before).clone();
    for (at, entry) in trace.entries.iter().enumerate().skip(reversed_at + 1) {
        let ability = trace.mana_ability_entries.get(&at);
        let dropped = match reversal {
            Reversal::Process => ability.is_none(),
            // The reversed activation's own answers, wherever they sit.
            Reversal::ManaAbility(source, place) => ability == Some(&(source, place)),
            Reversal::ManaSource(_) => false,
        };
        if dropped {
            continue;
        }
        restored.rerecord(trace, at, entry.clone(), Some(at) == process_at);
    }
    state.play_trace = Some(Box::new(restored));
}

/// The stack and object-id counter as they stood before a priority pass resolved its top.
pub(crate) struct BeforeResolution {
    stack: im::Vector<StackEntry>,
    next_object_id: u64,
}

/// The stack as it stood before a priority pass resolves its top, when the trace records it.
pub(crate) fn stack_before_resolution(state: &GameState) -> Option<BeforeResolution> {
    if in_simulation_probe()
        && state.loop_detection.samples()
        && state
            .stack
            .back()
            .is_some_and(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
    {
        meter!(resolution_hooks_in_probe += 1);
    }
    recording(state).then(|| BeforeResolution {
        stack: state.stack.clone(),
        next_object_id: state.next_object_id,
    })
}

/// CR 603.3: records each triggered ability among the `consumed` entries that resolved from
/// the top of `before`. Triggered mana abilities never use the stack (CR 605.4a).
pub(crate) fn record_resolutions(state: &mut GameState, before: &BeforeResolution, consumed: u32) {
    metered(|| {
        let consumed = usize::try_from(consumed).unwrap_or(usize::MAX);
        for (resolved, entry) in before.stack.iter().rev().take(consumed).enumerate() {
            let at = trace_mut(state).entries.len();
            let Some(key) = trigger_node(state, entry, at) else {
                continue;
            };
            meter!(resolutions_recorded += 1);
            let trace = trace_mut(state);
            let prior = trace.clone();
            let node = trace.intern(key);
            trace.record(
                &prior,
                TraceEntry {
                    seat: entry.controller,
                    kind: EntryKind::Resolution { node },
                    depth: before.stack.len() - resolved,
                    prompt: PromptClass::Priority,
                    next_object_id: before.next_object_id,
                },
                None,
            );
        }
    });
}

/// One legality read of one play through the engine's own authority (mana abilities by CR
/// 605.3a's); `None` when the locus has no single-play authority.
fn read_legality(state: &GameState, holder: PlayerId, locus: &PlayLocus) -> Option<bool> {
    #[cfg(feature = "test-support")]
    let copies_before = crate::game::perf_counters::take_cost_snapshot().state_copies;
    let legal = match *locus {
        PlayLocus::Cast(id) => crate::game::casting::can_cast_object_now(state, holder, id),
        PlayLocus::Activate(id, index) => {
            crate::game::casting::can_activate_ability_now(state, holder, id, index)
        }
        PlayLocus::Mana(id, Some(index)) => {
            let def = state.objects.get(&id)?.abilities.get(index)?;
            crate::game::mana_abilities::can_activate_mana_ability_now(
                state, holder, id, index, def,
            )
        }
        PlayLocus::Mana(_, None) | PlayLocus::Unread => return None,
    };
    meter!(
        legality_reads += 1,
        legality_read_copies +=
            crate::game::perf_counters::take_cost_snapshot().state_copies - copies_before,
    );
    Some(legal)
}

/// The window's naming, at the base's priority window: a play the period made is named once it
/// is available again (CR 732.1b, CR 732.2a), before it is repeated. Returns every span named
/// since the window before, repeats first, in the order they were named.
pub(crate) fn name_window(state: &mut GameState) -> Vec<NamedSpan> {
    if !recording(state) {
        return Vec::new();
    }
    let WaitingFor::Priority { player: holder } = state.waiting_for else {
        return Vec::new();
    };
    roll(state);
    let Some(mut trace) = state.play_trace.as_deref().cloned() else {
        return Vec::new();
    };
    metered(|| {
        meter!(windows += 1);
        // CR 117.3c: the priority a player receives right after a play shows whether the play
        // consumed what it needs to be made again.
        let mut kept = im::Vector::new();
        for read in trace.pending_reads.iter() {
            if read.seat != holder {
                kept.push_back(*read);
                continue;
            }
            match read_legality(state, holder, &read.locus) {
                Some(true) => {
                    trace.consumed.remove(&read.node);
                }
                Some(false) => {
                    trace.consumed.insert(read.node);
                }
                None => {}
            }
        }
        trace.pending_reads = kept;
        let end = trace.entries.len();
        if let Some(top) = state.stack.back() {
            // A play is named only with the stack empty; a trigger is named as its next instance
            // comes to the top when its own resolution asked an optional answer, or when an
            // optional answer and another node's repeat were both made since its previous
            // instance resolved, so the span closes a recurrence the trace has already seen.
            let node = trigger_node(state, top, end).and_then(|key| trace.find(&key));
            if let Some(node) = node {
                meter!(node_map_reads += 1);
                if let Some(&start) = trace.last.get(&node) {
                    let recurs_inside = || {
                        trace
                            .repeated
                            .iter()
                            .any(|repeated| trace.last.get(repeated).is_some_and(|&at| at > start))
                    };
                    if trace.optional_triggers.contains(&node)
                        || (trace.last_optional.is_some_and(|optional| optional > start)
                            && !trace.last_play.is_some_and(|play| play >= start)
                            && recurs_inside())
                    {
                        trace.named.push_back(NamedSpan::in_window(
                            start,
                            end,
                            NamingCause::TriggerTop,
                        ));
                    }
                }
            }
        } else {
            let mut played: Vec<(usize, usize, PlayLocus)> = trace
                .last
                .iter()
                .filter_map(|(&node, &at)| match trace.entries.get(at) {
                    Some(TraceEntry {
                        seat,
                        kind: EntryKind::Play { locus, .. },
                        ..
                    }) if *seat == holder => Some((at, node, *locus)),
                    _ => None,
                })
                .collect();
            // The retry order: each qualifying node in the order it was last made.
            played.sort_unstable_by_key(|(at, ..)| *at);
            let any_consumed = played
                .iter()
                .any(|(_, node, _)| trace.consumed.contains(node));
            let cause = if any_consumed {
                NamingCause::Restored
            } else {
                NamingCause::LegalNow
            };
            for (start, node, locus) in played {
                if any_consumed && !trace.consumed.contains(&node) {
                    continue;
                }
                if read_legality(state, holder, &locus) == Some(true) {
                    trace
                        .named
                        .push_back(NamedSpan::in_window(start, end, cause));
                }
            }
        }
        name_carried(state, holder, &mut trace);
    });
    let unread = trace.named.iter().skip(trace.unread).copied().collect();
    trace.unread = trace.named.len();
    state.play_trace = Some(Box::new(trace));
    unread
}

/// Names `holder`'s carried spans, in the order their keys last occurred, once each comes round:
/// with a trigger on top, when it is the carried trigger and its resolution asked an optional
/// answer in the span; with an empty stack, when the carried play is legal now. Nothing is asked
/// while a play of the span is on the stack. A named carried span ends.
fn name_carried(state: &GameState, holder: PlayerId, trace: &mut PlayTrace) {
    let mut kept = im::Vector::new();
    for span in std::mem::take(&mut trace.carried) {
        let cause = (span.seat == holder && !play_on_stack(state, &span.trace))
            .then(|| carried_cause(state, holder, &span))
            .flatten();
        let Some(cause) = cause else {
            kept.push_back(span);
            continue;
        };
        trace.named.push_back(NamedSpan {
            start: 0,
            end: span.trace.entries.len(),
            cause,
            source: SpanSource::Carried(trace.carried_named.len()),
            reach: span.reach,
        });
        trace.carried_named.push_back(span);
    }
    trace.carried = kept;
}

fn carried_cause(state: &GameState, holder: PlayerId, span: &CarriedSpan) -> Option<NamingCause> {
    let node = span.trace.find(&span.key)?;
    match state.stack.back() {
        Some(top) => (trigger_node(state, top, 0).as_ref() == Some(&span.key)
            && span.trace.optional_triggers.contains(&node))
        .then_some(NamingCause::TriggerTop),
        None => {
            let &at = span.trace.last.get(&node)?;
            let EntryKind::Play { locus, .. } = span.trace.entries.get(at)?.kind else {
                return None;
            };
            (read_legality(state, holder, &locus) == Some(true)).then_some(NamingCause::LegalNow)
        }
    }
}

/// Whether a spell or ability a play of `trace` made is on the stack.
fn play_on_stack(state: &GameState, trace: &PlayTrace) -> bool {
    let sources: Vec<ObjectId> = trace
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            EntryKind::Play {
                locus: PlayLocus::Cast(id) | PlayLocus::Activate(id, _),
                ..
            } => Some(id),
            _ => None,
        })
        .collect();
    state.stack.iter().any(|entry| {
        matches!(
            entry.kind,
            StackEntryKind::Spell { .. } | StackEntryKind::ActivatedAbility { .. }
        ) && sources.contains(&entry.source_id)
    })
}

/// Notes the span an offer was made for.
pub(crate) fn note_offered(state: &mut GameState, span: NamedSpan) {
    let window = WindowKey::of(state);
    if let Some(trace) = state
        .play_trace
        .as_deref_mut()
        .filter(|trace| trace.window == window)
    {
        trace.offered = Some(span);
    }
}

/// The printed identity of the node a triggered stack entry resolves, as a replayed cycle reports
/// what it performed; the source's name when the node has none.
pub(crate) fn resolution_identity(state: &GameState, entry: &StackEntry) -> Option<String> {
    let StackEntryKind::TriggeredAbility { source_name, .. } = &entry.kind else {
        return None;
    };
    Some(match trigger_node(state, entry, 0)? {
        NodeKey::Triggered(origin) => origin.printed_ref.face_name,
        NodeKey::Delayed(origin) => origin.creator.face_name,
        NodeKey::Cast(_)
        | NodeKey::Activated(_)
        | NodeKey::IntrinsicMana(_)
        | NodeKey::KeywordActivated { .. }
        | NodeKey::TriggeredOccurrence(_)
        | NodeKey::Unkeyed(_) => source_name.clone(),
    })
}

/// The current window's trace; `None` when none was recorded or it belongs to an ended step.
fn current(state: &GameState) -> Option<&PlayTrace> {
    state
        .play_trace
        .as_deref()
        .filter(|trace| trace.window == WindowKey::of(state))
}

/// CR 732.2a: drops `holder`'s plays and resolutions from the window's trace, each with the answers
/// made after it, since a period is evidence about the seat that made it and only the holder's own
/// is theirs to discard.
pub(crate) fn discard_seat(state: &mut GameState, holder: PlayerId) {
    let Some(trace) = current(state) else {
        state.play_trace = None;
        return;
    };
    let mut kept = PlayTrace::new(trace.window);
    kept.carrying = trace.carrying.clone();
    let mut owner = None;
    for (at, entry) in trace.entries.iter().enumerate() {
        if matches!(
            entry.kind,
            EntryKind::Play { .. } | EntryKind::Resolution { .. }
        ) {
            owner = Some(entry.seat);
        }
        if owner.unwrap_or(entry.seat) != holder {
            kept.rerecord(trace, at, entry.clone(), false);
        }
    }
    kept.carried = trace
        .carried
        .iter()
        .filter(|span| span.seat != holder)
        .cloned()
        .collect();
    state.play_trace =
        (!kept.entries.is_empty() || !kept.carried.is_empty()).then(|| Box::new(kept));
}

/// The current window's entries.
pub(crate) fn current_entries(state: &GameState) -> Option<&im::Vector<TraceEntry>> {
    current(state).map(|trace| &trace.entries)
}

/// The entries a span the current window named reads.
pub(crate) fn span_entries(state: &GameState, span: NamedSpan) -> Option<&im::Vector<TraceEntry>> {
    let trace = current(state)?;
    match span.source {
        SpanSource::Window => Some(&trace.entries),
        SpanSource::Carried(index) => trace
            .carried_named
            .get(index)
            .map(|span| &span.trace.entries),
    }
}

/// Installs a trace of `plays` for the current window, each a priority play at depth 0.
#[cfg(any(test, feature = "test-support"))]
pub fn install_plays_for_tests(state: &mut GameState, plays: &[(PlayerId, PlayLocus)]) {
    let mut trace = PlayTrace::new(WindowKey::of(state));
    for &(seat, locus) in plays {
        let node = trace.intern(NodeKey::Unkeyed(trace.entries.len()));
        let action = match locus {
            PlayLocus::Activate(source_id, ability_index)
            | PlayLocus::Mana(source_id, Some(ability_index)) => GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            PlayLocus::Cast(object_id) => GameAction::CastSpell {
                object_id,
                card_id: state
                    .objects
                    .get(&object_id)
                    .map_or(crate::types::identifiers::CardId(0), |object| {
                        object.card_id
                    }),
                targets: Vec::new(),
                payment_mode: Default::default(),
            },
            PlayLocus::Mana(_, None) | PlayLocus::Unread => GameAction::PassPriority,
        };
        trace.entries.push_back(TraceEntry {
            seat,
            kind: EntryKind::Play {
                action,
                node,
                locus,
            },
            depth: 0,
            prompt: PromptClass::Priority,
            next_object_id: state.next_object_id,
        });
    }
    state.play_trace = Some(Box::new(trace));
}

/// Every span the current window's trace names, in the order it named them.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn current_named(state: &GameState) -> Vec<NamedSpan> {
    current(state).map_or_else(Vec::new, |trace| trace.named.iter().copied().collect())
}

/// Runs a hook, metering the whole-state copies it makes outside its legality reads.
#[cfg(feature = "test-support")]
fn metered<R>(hook: impl FnOnce() -> R) -> R {
    use crate::game::perf_counters::{play_trace_counters, take_cost_snapshot};
    let copies = take_cost_snapshot().state_copies;
    let read_copies = play_trace_counters().legality_read_copies;
    let result = hook();
    let read_copies = play_trace_counters().legality_read_copies - read_copies;
    meter!(trace_state_copies += take_cost_snapshot().state_copies - copies - read_copies);
    result
}

#[cfg(not(feature = "test-support"))]
fn metered<R>(hook: impl FnOnce() -> R) -> R {
    hook()
}

/// The trace as a test reads it: every entry and every named span, in order.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct PlayTraceView {
    pub entries: Vec<TraceEntry>,
    pub named: Vec<NamedSpan>,
    pub node_count: usize,
    pub offered: Option<NamedSpan>,
    /// The live carried spans, in order.
    pub carried: Vec<CarriedView>,
}

/// A live carried span as a test reads it: its seat, reach, entry count, and its key's face.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedView {
    pub seat: PlayerId,
    pub reach: PeriodReach,
    pub entries: usize,
    /// The printed face of a cast or a trigger key; `None` for any other key.
    pub face: Option<String>,
}

/// The current window's trace; `None` when none was recorded or it belongs to an ended step.
#[cfg(any(test, feature = "test-support"))]
pub fn play_trace_view(state: &GameState) -> Option<PlayTraceView> {
    let trace = current(state)?;
    Some(PlayTraceView {
        entries: trace.entries.iter().cloned().collect(),
        named: trace.named.iter().copied().collect(),
        node_count: trace.nodes.len(),
        offered: trace.offered,
        carried: trace
            .carried
            .iter()
            .map(|span| CarriedView {
                seat: span.seat,
                reach: span.reach,
                entries: span.trace.entries.len(),
                face: match &span.key {
                    NodeKey::Cast(printed) => Some(printed.face_name.clone()),
                    NodeKey::Triggered(origin) => Some(origin.printed_ref.face_name.clone()),
                    _ => None,
                },
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use crate::types::counter::CounterType;
    use crate::types::game_state::{MulliganDecisionEntry, MulliganDecisionPhase};
    use crate::types::identifiers::CardId;

    fn effect(source: ObjectId) -> Box<ResolvedAbility> {
        Box::new(ResolvedAbility::new(
            Effect::Explore,
            vec![],
            source,
            PlayerId(0),
        ))
    }

    fn retarget(scope: RetargetScope) -> WaitingFor {
        WaitingFor::RetargetChoice {
            player: PlayerId(0),
            stack_entry_index: 0,
            scope,
            current_targets: vec![],
            slots: vec![],
            slot_pools: vec![],
            keep_is_distinct: Vec::new(),
            legal_new_targets: vec![],
        }
    }

    #[test]
    fn answer_optionality_is_whether_the_answer_could_be_declined() {
        use AnswerOptionality::{Mandatory, Optional};
        let (player, source) = (PlayerId(0), ObjectId(1));
        let rows = [
            (
                "explore order",
                WaitingFor::ExploreChoice {
                    player,
                    source_id: source,
                    choosable: vec![source],
                    remaining: vec![],
                    pending_effect: effect(source),
                },
                Mandatory,
            ),
            (
                "clash placement",
                WaitingFor::ClashCardPlacement {
                    player,
                    card: source,
                    remaining: vec![],
                },
                Mandatory,
            ),
            (
                "remove any number of counters",
                WaitingFor::RemoveCountersChoice {
                    player,
                    source_id: source,
                    counter_type: None,
                    available: vec![(CounterType::Plus1Plus1, 2)],
                    pending_effect: effect(source),
                },
                Optional,
            ),
            (
                "keep any number within total power",
                WaitingFor::KeepWithinTotalPowerChoice {
                    player,
                    target_player: player,
                    eligible: vec![source],
                    cap: 4,
                    choose_filter: TargetFilter::Any,
                    sacrifice_filter: TargetFilter::Any,
                    chooser_scope: Default::default(),
                    source_id: source,
                    source_controller: player,
                    remaining_players: vec![],
                    all_kept: vec![],
                    scoped_players: vec![],
                },
                Optional,
            ),
            (
                "redistribute any number of life totals",
                WaitingFor::RedistributeLifeTotals {
                    player,
                    options: vec![],
                },
                Optional,
            ),
            ("choose new targets", retarget(RetargetScope::All), Optional),
            (
                "change the target",
                retarget(RetargetScope::Single),
                Mandatory,
            ),
            (
                "mulligan",
                WaitingFor::MulliganDecision {
                    pending: vec![MulliganDecisionEntry {
                        player,
                        mulligan_count: 0,
                        free_reveals_taken: 0,
                        phase: MulliganDecisionPhase::Declare,
                    }],
                    free_first_mulligan: false,
                    declared: Vec::new(),
                },
                Optional,
            ),
            (
                "sacrificial mana source",
                WaitingFor::ManaSourceSelection {
                    player,
                    options: vec![],
                    convoke_mode: None,
                },
                Optional,
            ),
        ];
        let select = GameAction::SelectCards { cards: vec![] };
        let priority = WaitingFor::Priority { player };
        let land_drop = GameAction::PlayLand {
            object_id: source,
            card_id: CardId(1),
        };
        let answers = rows
            .iter()
            .map(|(label, prompt, expected)| (*label, prompt, &select, *expected))
            .chain([
                ("land drop at priority", &priority, &land_drop, Optional),
                (
                    "pass at priority",
                    &priority,
                    &GameAction::PassPriority,
                    Mandatory,
                ),
            ]);
        let misclassified: Vec<&str> = answers
            .filter(|(_, prompt, action, expected)| {
                let optional = match classify(action, prompt) {
                    Choice::Answer(answer) => Some(answer_optionality(prompt, answer)),
                    _ => None,
                };
                optional != Some(*expected)
            })
            .map(|(label, ..)| label)
            .collect();
        assert_eq!(misclassified, Vec::<&str>::new());
    }
}
