use crate::game::filter::FilterContext;
use crate::game::layers::{
    active_continuous_effects_from_base_static_source, active_effect_condition_controller,
    collect_shared_active_continuous_effects_matching,
    collect_shared_active_continuous_effects_matching_from, evaluate_condition_with_recipient,
    order_active_continuous_effects, GatherDependence, OffZoneStaticSourceIds,
    OffZoneStaticSources,
};
use crate::game::quantity::resolve_quantity;
use crate::types::ability::{ContinuousModification, TargetFilter, TriggerProducerOrigin};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::layers::{ActiveContinuousEffect, Layer};
use crate::types::zones::Zone;

thread_local! {
    /// CR 613.1f self-reference guard: the set of object ids whose off-zone
    /// keyword set is currently being computed on this call stack. A keyword
    /// grant may be gated on a keyword-presence predicate over the SAME object
    /// (Dream Devourer: "Each nonland card in your hand WITHOUT FORETELL has
    /// foretell"). Evaluating that predicate re-enters off-zone keyword
    /// computation for the same object; without a guard this recurses forever.
    /// While an object is in this set, a nested query resolves against the base
    /// (printed) keywords only — the grant cannot use its own output as its
    /// applicability input, which is exactly the rules-correct behavior.
    static OFF_ZONE_KEYWORD_STACK: std::cell::RefCell<Vec<ObjectId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// CR 613.1f self-reference guard, RAII form. Constructing the guard records
/// this frame's `object_id` in `OFF_ZONE_KEYWORD_STACK` iff it is not already
/// present. `entered` is `true` only for the frame that actually inserted the
/// id (the outermost frame for that object); re-entrant frames construct a
/// guard with `entered == false` and thus own no removal. `Drop` pops the id
/// unconditionally on every exit path — normal return, early return, or unwind
/// — so a panic or early-return in nested keyword computation can never leave
/// the thread-local set poisoned for the rest of the thread's life.
struct OffZoneRecursionGuard {
    object_id: ObjectId,
    entered: bool,
}

impl OffZoneRecursionGuard {
    fn enter(object_id: ObjectId) -> Self {
        let entered = OFF_ZONE_KEYWORD_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.contains(&object_id) {
                false
            } else {
                stack.push(object_id);
                true
            }
        });
        Self { object_id, entered }
    }

    /// `true` when this is a re-entrant frame for the same object — the caller
    /// must resolve against base (printed) keywords only.
    fn is_reentrant(&self) -> bool {
        !self.entered
    }
}

impl Drop for OffZoneRecursionGuard {
    fn drop(&mut self) {
        // Only the inserting frame owns removal; a re-entrant guard leaves the
        // outer frame's entry intact.
        if self.entered {
            OFF_ZONE_KEYWORD_STACK.with(|stack| {
                let mut stack = stack.borrow_mut();
                let popped = stack.pop();
                debug_assert_eq!(
                    popped,
                    Some(self.object_id),
                    "off-zone keyword guard imbalance"
                );
            });
        }
    }
}

/// The keyword gather one scope shares across its queries.
enum SharedGather {
    /// No query has gathered for this scope yet.
    Pending,
    /// A gather that evaluated no filter and no condition, so it is a pure
    /// function of the scoped state and every query may read it.
    Reusable(std::rc::Rc<[ActiveContinuousEffect]>),
    /// The gather had to evaluate a filter or a condition; every query in this
    /// scope gathers for itself.
    ContextDependent,
}

/// One open [`with_off_zone_keyword_scope`] scope.
struct OffZoneKeywordFrame {
    /// Address identity of the scoped state. Only ever compared with
    /// `std::ptr::eq`, never dereferenced: the scope's own shared borrow keeps
    /// the state alive and unmutated for as long as the frame exists.
    state: *const GameState,
    gather: SharedGather,
    /// The scoped state's static-effect sources outside the battlefield and
    /// command zone, once a gather in this scope has scanned for them. Which
    /// objects those are depends on no filter or condition, so every gather
    /// for this state shares them, `ContextDependent` ones included.
    /// CR 611.3b + CR 113.6: an off-zone static's effect applies only while
    /// its source is in a zone the ability functions from, so the list is
    /// valid only for this frame's unchanged state and ends with the frame.
    off_zone_sources: Option<std::rc::Rc<OffZoneStaticSourceIds>>,
}

thread_local! {
    /// The open off-zone keyword scopes on this call stack, innermost last.
    /// At most one frame exists per state address.
    static OFF_ZONE_KEYWORD_SCOPES: std::cell::RefCell<Vec<OffZoneKeywordFrame>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The single accessor for the scope stack. `access` must only compare
/// addresses, clone a handle out, write one slot, push, or pop; it must never
/// call out of this module, because a gather, a recipient filter, or a
/// condition can re-enter an off-zone keyword query and take this borrow again.
fn with_off_zone_keyword_frames<R>(access: impl FnOnce(&mut Vec<OffZoneKeywordFrame>) -> R) -> R {
    OFF_ZONE_KEYWORD_SCOPES.with(|frames| access(&mut frames.borrow_mut()))
}

/// What a query found on the scope stack for its state.
enum OffZoneKeywordFrameLookup {
    /// No open scope covers this state address.
    Unmatched,
    /// A scope covers it and has not gathered yet.
    Pending,
    /// A scope covers it and holds a shareable gather.
    Reusable(std::rc::Rc<[ActiveContinuousEffect]>),
    /// A scope covers it but its gather cannot be shared.
    ContextDependent,
}

/// Finds the open scope for `state`'s address and clones its shared handle
/// out, so no borrow of the scope stack outlives this call.
fn lookup_off_zone_keyword_frame(state: &GameState) -> OffZoneKeywordFrameLookup {
    with_off_zone_keyword_frames(|frames| {
        match frames
            .iter()
            .find(|frame| std::ptr::eq(frame.state, state))
            .map(|frame| &frame.gather)
        {
            None => OffZoneKeywordFrameLookup::Unmatched,
            Some(SharedGather::Pending) => OffZoneKeywordFrameLookup::Pending,
            Some(SharedGather::Reusable(shared)) => {
                OffZoneKeywordFrameLookup::Reusable(std::rc::Rc::clone(shared))
            }
            Some(SharedGather::ContextDependent) => OffZoneKeywordFrameLookup::ContextDependent,
        }
    })
}

/// Records the outcome of the first gather for `state`'s scope. Only a
/// `Pending` slot is written (first writer wins); a slot that is no longer
/// `Pending` is left as it is.
fn store_off_zone_keyword_gather(state: &GameState, gather: SharedGather) {
    #[cfg(test)]
    let fills = usize::from(matches!(gather, SharedGather::Reusable(_)));
    let stored = with_off_zone_keyword_frames(|frames| {
        let Some(frame) = frames
            .iter_mut()
            .find(|frame| std::ptr::eq(frame.state, state))
        else {
            return false;
        };
        match frame.gather {
            SharedGather::Pending => {
                frame.gather = gather;
                true
            }
            SharedGather::Reusable(_) | SharedGather::ContextDependent => false,
        }
    });
    #[cfg(test)]
    record_off_zone_keyword_scope(|counts| counts.fills += if stored { fills } else { 0 });
    #[cfg(not(test))]
    let _ = stored;
}

/// The off-zone static-effect sources of the open scope for `state`: read from
/// its frame, or scanned once and kept there (first writer wins). The scan runs
/// outside the frame borrow; it reads only zones, phasing and static
/// definitions, so it cannot re-enter an off-zone keyword query.
fn scoped_off_zone_static_sources(state: &GameState) -> std::rc::Rc<OffZoneStaticSourceIds> {
    let known = with_off_zone_keyword_frames(|frames| {
        frames
            .iter()
            .find(|frame| std::ptr::eq(frame.state, state))
            .and_then(|frame| frame.off_zone_sources.clone())
    });
    if let Some(known) = known {
        return known;
    }
    let scanned = std::rc::Rc::new(OffZoneStaticSourceIds::scan(state));
    with_off_zone_keyword_frames(|frames| {
        if let Some(frame) = frames
            .iter_mut()
            .find(|frame| std::ptr::eq(frame.state, state))
        {
            frame
                .off_zone_sources
                .get_or_insert_with(|| std::rc::Rc::clone(&scanned));
        }
    });
    scanned
}

/// RAII owner of one scope frame. `pushed` is `Some` only for the guard that
/// actually pushed a frame; a scope nested inside another scope on the same
/// state pushes nothing and owns no removal. `Drop` pops on every exit path —
/// normal return, early return, or unwind.
struct OffZoneKeywordScopeGuard {
    pushed: Option<*const GameState>,
    /// Test-only: the readings as this guard pushed its frame, taken only while
    /// [`off_zone_keyword_scopes_during`] records.
    #[cfg(test)]
    recorded_from: Option<OffZoneKeywordScopeRecord>,
}

impl OffZoneKeywordScopeGuard {
    /// Opens a scope for `state`, unless one is already open for its address.
    fn enter(state: &GameState) -> Self {
        let address: *const GameState = state;
        let pushed = with_off_zone_keyword_frames(|frames| {
            if frames
                .iter()
                .any(|frame| std::ptr::eq(frame.state, address))
            {
                None
            } else {
                frames.push(OffZoneKeywordFrame {
                    state: address,
                    gather: SharedGather::Pending,
                    off_zone_sources: None,
                });
                Some(address)
            }
        });
        Self {
            pushed,
            #[cfg(test)]
            recorded_from: pushed
                .and_then(|_| OffZoneKeywordScopeRecord::reading_while_recording()),
        }
    }
}

impl Drop for OffZoneKeywordScopeGuard {
    /// Pops the frame this guard pushed, if it pushed one.
    fn drop(&mut self) {
        if let Some(address) = self.pushed {
            with_off_zone_keyword_frames(|frames| {
                if let Some(index) = frames
                    .iter()
                    .rposition(|frame| std::ptr::eq(frame.state, address))
                {
                    frames.remove(index);
                }
            });
        }
        #[cfg(test)]
        record_off_zone_keyword_scope_frame(self.recorded_from);
    }
}

/// Shares one keyword gather across every off-zone keyword query made
/// against `state` while `f` runs.
///
/// The first query gathers. If that gather evaluated no filter and no
/// condition it is kept and later queries against the same state address read
/// it instead of gathering again; each query still applies its own recipient
/// filter and conditions. Otherwise, and for any other state (a clone, a probe
/// state), every query gathers for itself exactly as it does outside a scope.
/// Either way, the gathers against the scoped state walk the object map for
/// off-zone static-effect sources at most once and share the list.
pub(crate) fn with_off_zone_keyword_scope<R>(state: &GameState, f: impl FnOnce() -> R) -> R {
    let _scope = OffZoneKeywordScopeGuard::enter(state);
    f()
}

/// Test-only tallies of how off-zone keyword queries met the scope stack.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OffZoneKeywordScopeCounts {
    /// Queries that found a frame for their state address.
    pub(crate) matched: usize,
    /// Gathers stored into a frame.
    pub(crate) fills: usize,
    /// Queries served from a stored slice.
    pub(crate) hits: usize,
    /// Matched queries that could not reuse because of `ContextDependent`.
    pub(crate) context_fall_opens: usize,
    /// Queries that found no frame for their address.
    pub(crate) unmatched: usize,
    /// Effects cloned out of a shared slice.
    pub(crate) shared_clones: usize,
}

#[cfg(test)]
impl OffZoneKeywordScopeCounts {
    /// The tallies accumulated since `start`, an earlier reading on this
    /// thread with no reset in between.
    fn since(self, start: Self) -> Self {
        Self {
            matched: self.matched - start.matched,
            fills: self.fills - start.fills,
            hits: self.hits - start.hits,
            context_fall_opens: self.context_fall_opens - start.context_fall_opens,
            unmatched: self.unmatched - start.unmatched,
            shared_clones: self.shared_clones - start.shared_clones,
        }
    }
}

#[cfg(test)]
thread_local! {
    static OFF_ZONE_KEYWORD_SCOPE_COUNTS: std::cell::Cell<OffZoneKeywordScopeCounts> =
        const {
            std::cell::Cell::new(OffZoneKeywordScopeCounts {
                matched: 0,
                fills: 0,
                hits: 0,
                context_fall_opens: 0,
                unmatched: 0,
                shared_clones: 0,
            })
        };
}

/// Applies `update` to this thread's scope tallies.
#[cfg(test)]
fn record_off_zone_keyword_scope(update: impl FnOnce(&mut OffZoneKeywordScopeCounts)) {
    OFF_ZONE_KEYWORD_SCOPE_COUNTS.with(|counts| {
        let mut current = counts.get();
        update(&mut current);
        counts.set(current);
    });
}

/// Zeroes this thread's scope tallies.
#[cfg(test)]
pub(crate) fn reset_off_zone_keyword_scope_counts() {
    OFF_ZONE_KEYWORD_SCOPE_COUNTS.with(|counts| counts.set(OffZoneKeywordScopeCounts::default()));
}

/// Reads this thread's scope tallies.
#[cfg(test)]
pub(crate) fn off_zone_keyword_scope_counts() -> OffZoneKeywordScopeCounts {
    OFF_ZONE_KEYWORD_SCOPE_COUNTS.with(std::cell::Cell::get)
}

/// The number of open scope frames on this thread.
#[cfg(test)]
pub(crate) fn off_zone_keyword_scope_depth() -> usize {
    with_off_zone_keyword_frames(|frames| frames.len())
}

/// Test-only: walks of the object map for off-zone static-effect sources
/// (`layers::off_zone_static_source_scan_count`) and the scope tallies. A
/// recorded frame's value is what accumulated on this thread, by any code,
/// between the frame's push and its pop.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct OffZoneKeywordScopeRecord {
    /// Walks of the object map for off-zone static-effect sources.
    pub(crate) source_scans: usize,
    /// The scope tallies.
    pub(crate) counts: OffZoneKeywordScopeCounts,
}

#[cfg(test)]
impl OffZoneKeywordScopeRecord {
    /// This thread's running readings, if a recording is open.
    fn reading_while_recording() -> Option<Self> {
        OFF_ZONE_KEYWORD_SCOPE_RECORDS
            .with_borrow(Option::is_some)
            .then(|| Self {
                source_scans: crate::game::layers::off_zone_static_source_scan_count(),
                counts: off_zone_keyword_scope_counts(),
            })
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only record of the scope frames pushed and popped on this thread, in
    /// pop order. It holds a list only while `off_zone_keyword_scopes_during`
    /// runs, so the hook keeps nothing for any other test.
    static OFF_ZONE_KEYWORD_SCOPE_RECORDS: std::cell::RefCell<Option<Vec<OffZoneKeywordScopeRecord>>> =
        const { std::cell::RefCell::new(None) };
}

/// Appends, to this thread's open recording, what accumulated since `opened`:
/// the readings taken when the frame now being popped was pushed.
#[cfg(test)]
fn record_off_zone_keyword_scope_frame(opened: Option<OffZoneKeywordScopeRecord>) {
    let (Some(opened), Some(now)) = (opened, OffZoneKeywordScopeRecord::reading_while_recording())
    else {
        return;
    };
    let record = OffZoneKeywordScopeRecord {
        source_scans: now.source_scans - opened.source_scans,
        counts: now.counts.since(opened.counts),
    };
    OFF_ZONE_KEYWORD_SCOPE_RECORDS.with_borrow_mut(|records| {
        if let Some(records) = records {
            records.push(record);
        }
    });
}

/// Runs `f` with a scope recording open on this thread and returns, in pop
/// order, a record of every scope frame pushed and popped while `f` ran.
#[cfg(test)]
pub(crate) fn off_zone_keyword_scopes_during(f: impl FnOnce()) -> Vec<OffZoneKeywordScopeRecord> {
    let outer = OFF_ZONE_KEYWORD_SCOPE_RECORDS.replace(Some(Vec::new()));
    f();
    OFF_ZONE_KEYWORD_SCOPE_RECORDS
        .replace(outer)
        .unwrap_or_default()
}

pub fn effective_off_zone_keywords(state: &GameState, object_id: ObjectId) -> Vec<Keyword> {
    effective_off_zone_keyword_contributions(state, object_id)
        .into_iter()
        .map(|entry| entry.keyword)
        .collect()
}

/// Off-zone keyword characteristics together with the exact Layer-6 producer
/// that supplied each nonprinted keyword. Trigger collection reconciles these
/// contributions into recipient-local `TriggerEntry`s before matching, rather
/// than recreating payload-only keyword companions at read time.
pub(crate) fn effective_off_zone_keyword_contributions(
    state: &GameState,
    object_id: ObjectId,
) -> Vec<OffZoneKeywordContribution> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    if obj.zone == Zone::Battlefield {
        return obj
            .keywords
            .iter()
            .cloned()
            .map(OffZoneKeywordContribution::printed)
            .collect();
    }

    // CR 613.1f: re-entrant computation for the same object returns base
    // (printed) keywords only, breaking the self-referential grant cycle. The
    // RAII guard cleans up the thread-local set on every exit path (including
    // panics/early returns), so the set can never be left poisoned.
    let _guard = OffZoneRecursionGuard::enter(object_id);
    if _guard.is_reentrant() {
        return obj
            .base_keywords
            .iter()
            .cloned()
            .map(OffZoneKeywordContribution::printed)
            .collect();
    }

    let mut keywords = obj
        .base_keywords
        .iter()
        .cloned()
        .map(OffZoneKeywordContribution::printed)
        .collect();
    let effects = collect_applicable_off_zone_keyword_effects(state, object_id);
    let ordered = order_active_continuous_effects(Layer::Ability, &effects, state);

    for effect in ordered {
        apply_keyword_modification(state, object_id, &mut keywords, &effect);
    }

    keywords
}

pub fn effective_off_zone_keyword(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> Option<Keyword> {
    effective_off_zone_keywords(state, object_id)
        .into_iter()
        .find(|keyword| keyword.kind() == kind)
}

pub fn off_zone_has_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    effective_off_zone_keyword(state, object_id, kind).is_some()
}

/// The continuous effects that apply to `object_id`'s keywords while it is
/// outside the battlefield: the shared keyword gather followed by the object's
/// own base statics, each kept only if [`off_zone_keyword_effect_applies`].
///
/// Inside a [`with_off_zone_keyword_scope`] scope for this exact state the
/// shared gather may be read from the scope instead of being gathered again.
/// Inside such a scope the gather also reads the scope's off-zone static-effect
/// sources instead of walking the object map for them.
/// The recipient filter and the conditions are evaluated per query on every
/// path, shared effects first and own base statics second.
pub(crate) fn collect_applicable_off_zone_keyword_effects(
    state: &GameState,
    object_id: ObjectId,
) -> Vec<ActiveContinuousEffect> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    match lookup_off_zone_keyword_frame(state) {
        OffZoneKeywordFrameLookup::Unmatched => {
            #[cfg(test)]
            record_off_zone_keyword_scope(|counts| counts.unmatched += 1);
            let (effects, _) = collect_shared_active_continuous_effects_matching(
                state,
                supports_off_zone_keyword_query,
            );
            applicable_from_owned_gather(state, obj, effects)
        }
        OffZoneKeywordFrameLookup::ContextDependent => {
            #[cfg(test)]
            record_off_zone_keyword_scope(|counts| {
                counts.matched += 1;
                counts.context_fall_opens += 1;
            });
            let sources = scoped_off_zone_static_sources(state);
            let (effects, _) = collect_shared_active_continuous_effects_matching_from(
                state,
                supports_off_zone_keyword_query,
                OffZoneStaticSources::Known(&sources),
            );
            applicable_from_owned_gather(state, obj, effects)
        }
        OffZoneKeywordFrameLookup::Pending => {
            #[cfg(test)]
            record_off_zone_keyword_scope(|counts| counts.matched += 1);
            let sources = scoped_off_zone_static_sources(state);
            let (effects, dependence) = collect_shared_active_continuous_effects_matching_from(
                state,
                supports_off_zone_keyword_query,
                OffZoneStaticSources::Known(&sources),
            );
            match dependence {
                GatherDependence::ContextDependent => {
                    store_off_zone_keyword_gather(state, SharedGather::ContextDependent);
                    #[cfg(test)]
                    record_off_zone_keyword_scope(|counts| counts.context_fall_opens += 1);
                    applicable_from_owned_gather(state, obj, effects)
                }
                GatherDependence::Structural => {
                    // Elements are moved into the shared slice, not cloned.
                    let shared: std::rc::Rc<[ActiveContinuousEffect]> = effects.into();
                    store_off_zone_keyword_gather(
                        state,
                        SharedGather::Reusable(std::rc::Rc::clone(&shared)),
                    );
                    applicable_from_shared_gather(state, obj, &shared)
                }
            }
        }
        OffZoneKeywordFrameLookup::Reusable(shared) => {
            #[cfg(test)]
            record_off_zone_keyword_scope(|counts| {
                counts.matched += 1;
                counts.hits += 1;
            });
            applicable_from_shared_gather(state, obj, &shared)
        }
    }
}

/// The effects `obj`'s own base statics contribute to its off-zone keyword
/// query. Never shared between queries.
fn own_base_static_keyword_effects(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
) -> Vec<ActiveContinuousEffect> {
    if obj.zone != Zone::Battlefield && !(obj.zone == Zone::Command && obj.is_emblem) {
        active_continuous_effects_from_base_static_source(
            state,
            obj,
            supports_off_zone_keyword_query,
        )
    } else {
        Vec::new()
    }
}

/// Finishes a query from a gather this query owns: append the object's own
/// base statics and keep the applicable effects by move, cloning nothing.
fn applicable_from_owned_gather(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    mut effects: Vec<ActiveContinuousEffect>,
) -> Vec<ActiveContinuousEffect> {
    effects.extend(own_base_static_keyword_effects(state, obj));
    effects
        .into_iter()
        .filter(|effect| off_zone_keyword_effect_applies(state, obj.id, obj.zone, effect))
        .collect()
}

/// Finishes a query from a scope's shared gather: read it by reference and
/// clone only the applicable effects, then append the object's own applicable
/// base-static effects by move.
fn applicable_from_shared_gather(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    shared: &[ActiveContinuousEffect],
) -> Vec<ActiveContinuousEffect> {
    let mut applicable: Vec<ActiveContinuousEffect> = shared
        .iter()
        .filter(|effect| off_zone_keyword_effect_applies(state, obj.id, obj.zone, effect))
        .cloned()
        .collect();
    #[cfg(test)]
    record_off_zone_keyword_scope(|counts| counts.shared_clones += applicable.len());
    applicable.extend(
        own_base_static_keyword_effects(state, obj)
            .into_iter()
            .filter(|effect| off_zone_keyword_effect_applies(state, obj.id, obj.zone, effect)),
    );
    applicable
}

/// Whether one gathered effect applies to `object_id`'s keywords in `zone`:
/// a Layer-6 keyword-shaped modification whose affected filter matches the
/// object and whose condition, if any, holds for it.
fn off_zone_keyword_effect_applies(
    state: &GameState,
    object_id: ObjectId,
    zone: Zone,
    effect: &ActiveContinuousEffect,
) -> bool {
    let condition_controller = active_effect_condition_controller(state, effect);
    let ctx = FilterContext::from_source_with_controller(effect.source_id, condition_controller);
    effect.layer == Layer::Ability
        && supports_off_zone_keyword_query(&effect.modification)
        && matches_off_zone_keyword_recipient(state, object_id, zone, &effect.affected_filter, &ctx)
        && effect.condition.as_ref().is_none_or(|condition| {
            evaluate_condition_with_recipient(
                state,
                condition,
                condition_controller,
                effect.source_id,
                object_id,
            )
        })
}

/// CR 613.1f + CR 611.3b: Could ANY currently-active SHARED continuous effect
/// add a keyword of `kind` to an object outside the battlefield?
///
/// Together with [`base_statics_can_grant_off_zone_keyword_kind`] this pair
/// pre-filters EXACTLY the two effect sources
/// [`collect_applicable_off_zone_keyword_effects`] draws from — the shared
/// collect and the recipient's own base statics — and nothing else. When BOTH
/// answer `false`, that function cannot produce a keyword-ADDING effect for
/// ANY recipient, so every off-zone keyword query on that board resolves to
/// the object's printed keywords or fewer.
///
/// CR 611.3b is why the shared half must be consulted at all: a static's
/// continuous effect applies while its SOURCE is on the battlefield (or in
/// whichever zone it functions from), even though its recipients sit in a
/// graveyard. CR 613.1f is the layer that grant is applied in, which is why
/// the predicate is scoped to `Layer::Ability`.
///
/// Safe to hoist ACROSS recipients because `collect_shared_active_continuous_effects_matching`
/// takes no recipient at all — one sweep answers the question for every card
/// in a zone. The recipient filter (`matches_off_zone_keyword_recipient`) and
/// the per-effect `condition` are deliberately NOT applied here: they are the
/// recipient-dependent half that cannot be hoisted. That makes a `true`
/// conservative (a grant aimed at someone else's graveyard still answers
/// `true`, and the caller then pays the full per-recipient resolve) and a
/// `false` exact.
///
/// Intended use: ONE shared call per event, with
/// [`base_statics_can_grant_off_zone_keyword_kind`] called per candidate.
pub(crate) fn shared_effects_can_grant_off_zone_keyword_kind(
    state: &GameState,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    let (effects, _) =
        collect_shared_active_continuous_effects_matching(state, supports_off_zone_keyword_query);
    effects
        .iter()
        .any(|effect| effect_can_add_off_zone_keyword_kind(state, effect, kind))
}

/// CR 613.1f + CR 113.6b: Could `object_id`'s OWN base statics add a keyword of
/// `kind` to it while it is off the battlefield?
///
/// The per-object half of the pair documented on
/// [`shared_effects_can_grant_off_zone_keyword_kind`] — it pre-filters
/// [`collect_applicable_off_zone_keyword_effects`]'s second input, the
/// `active_continuous_effects_from_base_static_source` extension. CR 113.6b is
/// the rule behind that builder's zone gate: an ability that states which
/// zones it functions in functions only from those zones, which is how a
/// graveyard card's own printed static ("as long as this card is in your
/// graveyard, …") reaches itself while a battlefield-only static does not.
///
/// This half never sweeps the game. It reads one already-hashed object,
/// short-circuits on an empty `base_static_definitions`, and otherwise re-uses
/// [`active_continuous_effects_from_base_static_source`] — the SAME builder
/// the real collector calls — so the pre-filter cannot drift from it.
pub(crate) fn base_statics_can_grant_off_zone_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.base_static_definitions.is_empty() {
        return false;
    }
    active_continuous_effects_from_base_static_source(state, obj, supports_off_zone_keyword_query)
        .iter()
        .any(|effect| effect_can_add_off_zone_keyword_kind(state, effect, kind))
}

/// CR 109.5 + CR 400.3: "your" cards in hand/library/graveyard are scoped by owner,
/// not by a stale object controller/LKI. Delegates to
/// `filter::matches_target_filter_for_zone`, the single authority for that
/// partition, so this path and target enumeration in `game::targeting` cannot drift
/// on which zones are owner-scoped.
fn matches_off_zone_keyword_recipient(
    state: &GameState,
    object_id: ObjectId,
    zone: Zone,
    filter: &TargetFilter,
    ctx: &FilterContext<'_>,
) -> bool {
    crate::game::filter::matches_target_filter_for_zone(state, object_id, zone, filter, ctx)
}

fn supports_off_zone_keyword_query(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::AddDynamicKeyword { .. }
            // CR 702.143d: derived-cost cast-from-off-zone keyword grants are
            // realized exclusively through this path (the recipient lives in a
            // non-battlefield zone), so they must be retained by the off-zone
            // collector.
            | ContinuousModification::AddKeywordWithDerivedCost { .. }
            | ContinuousModification::RemoveAllAbilities
            // CR 608.2d + CR 613.1f: `RemoveChosenKeyword` strips the exact
            // keyword stored in the source's `chosen_attributes`
            // (Urborg / Walking Sponge). Same off-zone
            // applicability as `RemoveKeyword` — the granted/printed keyword
            // it targets may live on an object outside the battlefield.
            | ContinuousModification::RemoveChosenKeyword
            // CR 608.2d + CR 613.1f: `AddChosenKeyword` grants the keyword
            // stored in the source's `chosen_attributes`. Same off-zone
            // applicability as `AddKeyword`.
            | ContinuousModification::AddChosenKeyword
    )
}

/// The Layer-6 ADD subset of [`supports_off_zone_keyword_query`]: does this
/// already-collected effect ADD a keyword of `kind` to whatever it applies to?
///
/// Reuses verbatim the same two conjuncts
/// [`collect_applicable_off_zone_keyword_effects`] applies —
/// `effect.layer == Layer::Ability` and the superset predicate above — so this
/// can neither admit a class the real collector rejects nor reject one it
/// admits. The collector's other two conjuncts (the recipient filter and the
/// per-effect condition) are the recipient-dependent half and are deliberately
/// omitted; see [`shared_effects_can_grant_off_zone_keyword_kind`].
fn effect_can_add_off_zone_keyword_kind(
    state: &GameState,
    effect: &ActiveContinuousEffect,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    effect.layer == Layer::Ability
        && supports_off_zone_keyword_query(&effect.modification)
        && modification_can_add_keyword_kind(state, effect, kind)
}

/// Which of the seven arms [`supports_off_zone_keyword_query`] admits can
/// actually produce a `Keyword` of `kind`? Mirrors [`apply_keyword_modification`]
/// arm for arm, which is why the two sit in the same module: a new arm added to
/// one is read next to the other.
///
/// The four ADD arms report the kind they would produce. The `with_value(0)` /
/// `with_cost(ManaCost::generic(0))` placeholders are deliberate: the concrete
/// `Keyword` VARIANT each of those arms produces is a function of its
/// `DynamicKeywordKind` / `CostBearingKeywordKind` alone, never of the payload,
/// so a placeholder payload reads the produced `KeywordKind` EXACTLY. Calling
/// the real constructors rather than writing a `kind -> KeywordKind` mapping
/// table here is what makes this drift-proof — a new variant on either enum
/// updates this predicate automatically, where a hand mapping would silently
/// miss it.
///
/// The three removal arms return `false` because a removal can only SHRINK the
/// contribution list `effective_off_zone_keyword_contributions` seeds from
/// `base_keywords`. With no ADD arm live, the recipient therefore ends with
/// either no keyword of this kind, or exactly its printed one — and both cases
/// are already `None` at the consumer (`granted_dredge_value` returns `None`
/// when `effective_dredge_value` is `None`, and again when its redundancy
/// comparison against `printed_dredge_value` matches). `printed_dredge_value`
/// assumes a single printed instance of the kind, as every real card has.
/// Skipping the resolve on `false` is therefore exact, not merely conservative.
fn modification_can_add_keyword_kind(
    state: &GameState,
    effect: &ActiveContinuousEffect,
    kind: crate::types::keywords::KeywordKind,
) -> bool {
    match &effect.modification {
        // `apply_keyword_modification` upserts this keyword verbatim.
        ContinuousModification::AddKeyword { keyword } => keyword.kind() == kind,
        // `apply_keyword_modification` builds `kind.with_value(resolved)`.
        ContinuousModification::AddDynamicKeyword { kind: dynamic, .. } => {
            dynamic.with_value(0).kind() == kind
        }
        // `apply_keyword_modification` builds `kind.with_cost(derived)`. Its
        // per-recipient "recipient already has one ⇒ no-op" dedup can only make
        // the real result NARROWER than this predicate, never wider.
        ContinuousModification::AddKeywordWithDerivedCost { kind: bearing, .. } => {
            bearing
                .with_cost(crate::types::mana::ManaCost::generic(0))
                .kind()
                == kind
        }
        // CR 608.2d: the keyword identity lives on the granting SOURCE's
        // `chosen_attributes`, never on the modification or on the recipient,
        // so this arm reads `effect.source_id` exactly as
        // `apply_keyword_modification` does. A missing source or an empty
        // chosen list makes the real arm a no-op, so `false` is exact there.
        ContinuousModification::AddChosenKeyword => state
            .objects
            .get(&effect.source_id)
            .is_some_and(|src| src.chosen_keywords().iter().any(|kw| kw.kind() == kind)),
        // Removal-only arms: `retain` by the shared removal relation, `retain`
        // by the source's exact chosen keyword, and `keywords.clear()`. None can
        // create a keyword — see the exactness argument on this function's doc.
        ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::RemoveAllAbilities => false,
        // Non-keyword arms: rejected by `supports_off_zone_keyword_query`, so
        // unreachable under that conjunct in `effect_can_add_off_zone_keyword_kind`.
        // Listed exhaustively (no wildcard) so a new `ContinuousModification`
        // variant is a compile error here and must be classified explicitly —
        // otherwise a new keyword-ADDING arm would silently answer `false` and
        // stop offering granted dredge.
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::GrantTrigger { .. }
        | ContinuousModification::GrantReplacement { .. }
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::SetDynamicPower { .. }
        | ContinuousModification::SetDynamicToughness { .. }
        | ContinuousModification::SetPowerDynamic { .. }
        | ContinuousModification::SetToughnessDynamic { .. }
        | ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::SubstituteTextWord { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {
            // The exhaustive list catches a NEW variant at compile time; this
            // assertion catches an EXISTING listed variant later admitted by
            // `supports_off_zone_keyword_query` without being classified above.
            debug_assert!(
                !supports_off_zone_keyword_query(&effect.modification),
                "a modification admitted by `supports_off_zone_keyword_query` must be \
                 classified explicitly above: {:?}",
                effect.modification
            );
            false
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OffZoneKeywordContribution {
    pub(crate) keyword: Keyword,
    pub(crate) origin: Option<TriggerProducerOrigin>,
    /// Stable output position inside one exact producer. This distinguishes
    /// multiple keywords granted by `AddChosenKeyword` without inspecting
    /// their payloads.
    pub(crate) producer_output_index: usize,
}

impl OffZoneKeywordContribution {
    fn printed(keyword: Keyword) -> Self {
        Self {
            keyword,
            origin: None,
            producer_output_index: 0,
        }
    }

    fn granted(
        keyword: Keyword,
        effect: &ActiveContinuousEffect,
        producer_output_index: usize,
    ) -> Self {
        Self {
            keyword,
            origin: effect.trigger_producer_origin.clone(),
            producer_output_index,
        }
    }
}

fn apply_keyword_modification(
    state: &GameState,
    object_id: ObjectId,
    keywords: &mut Vec<OffZoneKeywordContribution>,
    effect: &ActiveContinuousEffect,
) {
    match &effect.modification {
        ContinuousModification::AddKeyword { keyword } => upsert_keyword_contribution(
            keywords,
            OffZoneKeywordContribution::granted(keyword.clone(), effect, 0),
        ),
        // CR 702.143d + CR 702 (alt-cost off-zone family): grant a cost-bearing
        // keyword whose cost is DERIVED from the recipient's mana cost. The
        // "without foretell" clause is enforced per-recipient here: if the
        // recipient already carries a keyword of this family (printed or granted),
        // no-op so its existing cost is preserved (Singing Towers of Darillium).
        ContinuousModification::AddKeywordWithDerivedCost { kind, derivation } => {
            if keywords
                .iter()
                .any(|entry| kind.matches_keyword(&entry.keyword))
            {
                return;
            }
            if let Some(recipient) = state.objects.get(&object_id) {
                let derived = derivation.derive(&recipient.mana_cost);
                upsert_keyword_contribution(
                    keywords,
                    OffZoneKeywordContribution::granted(kind.with_cost(derived), effect, 0),
                );
            }
        }
        ContinuousModification::RemoveKeyword { keyword } => {
            // CR 113.10b + CR 702.11e: use the same family removal as layer 6.
            keywords
                .retain(|entry| !super::keywords::keyword_matches_removal(&entry.keyword, keyword));
        }
        ContinuousModification::AddDynamicKeyword { kind, value } => {
            let dynamic_value = resolve_quantity(state, value, effect.controller, effect.source_id);
            let keyword = kind.with_value(dynamic_value.max(0) as u32);
            upsert_keyword_contribution(
                keywords,
                OffZoneKeywordContribution::granted(keyword, effect, 0),
            );
        }
        ContinuousModification::RemoveAllAbilities => keywords.clear(),
        // CR 608.2d + CR 613.1f + CR 702.14: Strip the *exact* keyword
        // chosen at resolution time — mirrors the battlefield
        // `RemoveChosenKeyword` arm in `layers.rs`. Uses `PartialEq` rather
        // than discriminant equality so that removing swampwalk leaves
        // islandwalk intact (CR 702.14 treats each landwalk subtype as a
        // distinct keyword). If the source has no stored chosen keyword
        // (e.g. the static is gathered before the choose effect has
        // resolved), this is a no-op rather than a panic, matching
        // `layers.rs` semantics.
        ContinuousModification::RemoveChosenKeyword => {
            if let Some(kw) = state
                .objects
                .get(&effect.source_id)
                .and_then(|src| src.chosen_keyword())
            {
                keywords.retain(|entry| &entry.keyword != kw);
            }
        }
        // CR 608.2d + CR 613.1f: Grant EACH keyword chosen at resolution time —
        // the additive mirror of `RemoveChosenKeyword`, matching the `AddKeyword`
        // upsert semantics above. Reads the PLURAL list so a multi-keyword choice
        // (Greymond's "each of the chosen abilities") grants every chosen ability
        // off-zone, not just the first. No-op when the source has no stored
        // chosen keyword. Source-scoped via `effect.source_id`.
        ContinuousModification::AddChosenKeyword => {
            let chosen: Vec<Keyword> = state
                .objects
                .get(&effect.source_id)
                .map(|src| src.chosen_keywords().into_iter().cloned().collect())
                .unwrap_or_default();
            for (producer_output_index, kw) in chosen.into_iter().enumerate() {
                upsert_keyword_contribution(
                    keywords,
                    OffZoneKeywordContribution::granted(kw, effect, producer_output_index),
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
fn upsert_keyword(keywords: &mut Vec<OffZoneKeywordContribution>, keyword: Keyword) {
    upsert_keyword_contribution(keywords, OffZoneKeywordContribution::printed(keyword));
}

fn upsert_keyword_contribution(
    keywords: &mut Vec<OffZoneKeywordContribution>,
    contribution: OffZoneKeywordContribution,
) {
    // CR 702.164b: summing keywords (Toxic) accumulate — never overwrite; push so
    // every instance is counted by the aggregate reader (effective_total_toxic_value).
    // Gate on the INCOMING keyword's summing flag (not a kind comparison) so a
    // granted off-zone Toxic pushes rather than clobbering an unrelated printed
    // keyword that shares its (Unknown) kind. Non-summing keywords keep the
    // upsert-by-kind dedup below unchanged.
    if !contribution.keyword.instances_must_coexist() {
        if let Some(existing) = keywords
            .iter_mut()
            .find(|existing| existing.keyword.kind() == contribution.keyword.kind())
        {
            *existing = contribution;
            return;
        }
    }

    keywords.push(contribution);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, CostDerivation, Duration, FilterProp, QuantityExpr,
        StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::keywords::{
        CostBearingKeywordKind, DynamicKeywordKind, FlashbackCost, Keyword, KeywordKind,
    };
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn create_card(state: &mut GameState, owner: PlayerId, name: &str, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let timestamp = state.next_timestamp();
        let object_id = create_object(state, card_id, owner, name.to_string(), zone);
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.timestamp = timestamp;
        }
        object_id
    }

    #[test]
    fn lose_hexproof_off_zone_removes_family_only_from_recipient() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Removal Source", Zone::Battlefield);
        let recipient = create_card(&mut state, PlayerId(1), "Recipient", Zone::Graveyard);
        let neighbor = create_card(&mut state, PlayerId(1), "Neighbor", Zone::Graveyard);
        let qualified = Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black));
        for id in [recipient, neighbor] {
            state.objects.get_mut(&id).unwrap().base_keywords =
                vec![qualified.clone(), Keyword::Flying];
            assert_eq!(
                effective_off_zone_keyword(&state, id, KeywordKind::Hexproof),
                Some(qualified.clone()),
            );
        }
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: recipient },
            vec![ContinuousModification::RemoveKeyword {
                keyword: Keyword::Hexproof,
            }],
            None,
        );
        assert_eq!(
            effective_off_zone_keyword(&state, recipient, KeywordKind::Flying),
            Some(Keyword::Flying),
        );
        assert_eq!(
            effective_off_zone_keyword(&state, neighbor, KeywordKind::Hexproof),
            Some(qualified),
        );
        // CR 702.11e: the shared removal rule applies to objects off the battlefield too.
        assert_eq!(
            effective_off_zone_keyword(&state, recipient, KeywordKind::Hexproof),
            None,
            "lose hexproof must remove off-zone HexproofFrom",
        );
    }

    /// CR 702.164b (issue #955): `upsert_keyword` must PUSH summing keywords
    /// (Toxic) so the off-zone aggregate reader counts every instance, while
    /// keeping the upsert-by-kind dedup for all non-summing keywords. The gate
    /// keys on the INCOMING keyword's summing flag, not a kind comparison, so a
    /// granted Toxic (which collapses to `KeywordKind::Unknown`) does NOT clobber
    /// an unrelated printed keyword that happens to share that `Unknown` kind.
    #[test]
    fn upsert_keyword_sums_toxic_but_dedups_others() {
        use crate::types::keywords::WardCost;

        // Summing keyword: two Toxic(1) accumulate (push) -> len 2.
        let mut toxic = Vec::new();
        upsert_keyword(&mut toxic, Keyword::Toxic(1));
        upsert_keyword(&mut toxic, Keyword::Toxic(1));
        assert_eq!(toxic.len(), 2, "two granted Toxic(1) must both be retained");
        assert_eq!(
            toxic
                .iter()
                .filter(|entry| matches!(entry.keyword, Keyword::Toxic(_)))
                .count(),
            2
        );

        // Non-summing keyword sharing a distinct kind: the second Ward overwrites
        // the first (upsert-by-kind dedup preserved) -> len 1.
        let mut ward = Vec::new();
        upsert_keyword(
            &mut ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })),
        );
        upsert_keyword(
            &mut ward,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })),
        );
        assert_eq!(ward.len(), 1, "non-summing Ward keeps upsert-by-kind dedup");
        assert_eq!(
            ward[0].keyword,
            Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })),
            "the later Ward instance overwrites the earlier one"
        );

        // Clobber-protection residual: an incoming Toxic grant must NOT overwrite
        // an unrelated printed keyword that also collapses to KeywordKind::Unknown
        // (StartingIntensity). Both must survive.
        assert_eq!(Keyword::StartingIntensity(1).kind(), KeywordKind::Unknown);
        assert_eq!(Keyword::Toxic(1).kind(), KeywordKind::Unknown);
        let mut mixed = vec![OffZoneKeywordContribution::printed(
            Keyword::StartingIntensity(1),
        )];
        upsert_keyword(&mut mixed, Keyword::Toxic(1));
        assert_eq!(
            mixed.len(),
            2,
            "Toxic grant must not clobber an unrelated same-(Unknown)-kind printed keyword"
        );
        assert!(mixed
            .iter()
            .any(|entry| entry.keyword == Keyword::StartingIntensity(1)));
        assert!(mixed.iter().any(|entry| entry.keyword == Keyword::Toxic(1)));
    }

    #[test]
    fn printed_graveyard_keyword_is_returned_unchanged() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&card_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&card_id).unwrap().keywords = base_keywords;

        let keywords = effective_off_zone_keywords(&state, card_id);
        assert_eq!(keywords.len(), 1);
        assert_eq!(
            keywords[0],
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            }))
        );
    }

    #[test]
    fn transient_add_keyword_applies_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Snapcaster Mage",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
            }],
            None,
        );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(
                ManaCost::SelfManaCost
            )))
        );
    }

    /// Matrix row 1 (F1) — CR 613.1f + CR 611.3b: the hoisted shared guard
    /// answers `false` on a board where nothing can grant Dredge outside the
    /// battlefield, and `true` the moment one real grant is live. The `true`
    /// case carries a positive reach-guard through the production authority
    /// (`effective_off_zone_keyword`), so the `false` case cannot pass merely
    /// because the instrument is inert.
    #[test]
    fn guard_is_false_without_a_dredge_grant_and_true_with_one() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "The Necrobloom", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Forest", Zone::Graveyard);

        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "a board with no keyword-granting continuous effect must answer false"
        );

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Dredge(2),
            }],
            None,
        );

        // Positive reach-guard: the fixture's grant really reaches the graveyard
        // card through the production authority, so the `true` below measures a
        // live grant rather than an inert board.
        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Dredge),
            Some(Keyword::Dredge(2)),
            "reach-guard: the transient must really grant Dredge 2 to the graveyard card"
        );
        assert!(
            shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "one live AddKeyword {{ Dredge(2) }} must make the shared guard true"
        );

        // Kind discrimination: the guard answers for the KIND asked about, not
        // for "some grant exists somewhere".
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Flashback),
            "a live Dredge grant must not answer true for an unrelated keyword kind"
        );

        // Hostile fixture: a board whose only live effects REMOVE. Removal arms
        // can only shrink the contribution list, so the guard must stay `false`
        // — this is the exactness claim, not a conservative approximation.
        let mut removal_only = GameState::new_two_player(42);
        let remover = create_card(
            &mut removal_only,
            PlayerId(0),
            "Stripping Source",
            Zone::Battlefield,
        );
        let victim = create_card(
            &mut removal_only,
            PlayerId(0),
            "Dakmor Salvage",
            Zone::Graveyard,
        );
        removal_only
            .objects
            .get_mut(&victim)
            .unwrap()
            .base_keywords
            .push(Keyword::Dredge(2));
        removal_only.add_transient_continuous_effect(
            remover,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: victim },
            vec![
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Dredge(2),
                },
                ContinuousModification::RemoveAllAbilities,
            ],
            None,
        );
        // Reach-guard on the hostile board: the removals really are live and
        // really do reach the recipient (they strip its printed Dredge), so the
        // `false` below cannot pass because the board is empty.
        assert_eq!(
            effective_off_zone_keyword(&removal_only, victim, KeywordKind::Dredge),
            None,
            "reach-guard: the removal effects must really apply to the graveyard card"
        );
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&removal_only, KeywordKind::Dredge),
            "removal-only arms cannot create a Dredge candidate, so the guard must stay false"
        );
    }

    /// Matrix row 2 (F1): `modification_can_add_keyword_kind` is exhaustive over
    /// the seven arms `supports_off_zone_keyword_query` admits. Every ADD arm is
    /// reported for the kind it actually produces and refused for a kind it does
    /// not; every REMOVE arm is refused on a board where the keyword it acts on
    /// really is present, so the refusal is measured on a live arm rather than a
    /// trivially absent one.
    ///
    /// The `AddChosenKeyword` pair is the multi-authority row: the keyword
    /// identity lives on the granting SOURCE's `chosen_attributes` (CR 608.2d),
    /// never on the modification or the recipient, so a guard that read the
    /// recipient would answer `false` where the truth is `true`.
    #[test]
    fn guard_admits_every_add_arm_and_refuses_every_remove_arm() {
        use crate::types::ability::ChosenAttribute;

        fn board(modifications: Vec<ContinuousModification>) -> (GameState, ObjectId, ObjectId) {
            let mut state = GameState::new_two_player(42);
            let source_id = create_card(
                &mut state,
                PlayerId(0),
                "Granting Source",
                Zone::Battlefield,
            );
            let target_id = create_card(&mut state, PlayerId(0), "Graveyard Card", Zone::Graveyard);
            state.add_transient_continuous_effect(
                source_id,
                PlayerId(0),
                Duration::UntilEndOfTurn,
                TargetFilter::SpecificObject { id: target_id },
                modifications,
                None,
            );
            (state, source_id, target_id)
        }

        // --- ADD arm 1: AddKeyword (the keyword verbatim).
        let (state, _, target_id) = board(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Dredge(2),
        }]);
        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Dredge),
            Some(Keyword::Dredge(2)),
            "reach-guard: AddKeyword must really grant Dredge 2"
        );
        assert!(shared_effects_can_grant_off_zone_keyword_kind(
            &state,
            KeywordKind::Dredge
        ));
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Modular),
            "AddKeyword must be refused for a kind it does not produce"
        );

        // --- ADD arm 2: AddDynamicKeyword (variant is a function of the kind).
        let (state, _, target_id) = board(vec![ContinuousModification::AddDynamicKeyword {
            kind: DynamicKeywordKind::Modular,
            value: QuantityExpr::Fixed { value: 2 },
        }]);
        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Modular),
            Some(Keyword::Modular(2)),
            "reach-guard: AddDynamicKeyword must really grant Modular 2"
        );
        assert!(shared_effects_can_grant_off_zone_keyword_kind(
            &state,
            KeywordKind::Modular
        ));
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "a Modular grant must not answer true for Dredge"
        );

        // --- ADD arm 3: AddKeywordWithDerivedCost (same placeholder argument).
        let (state, _, target_id) =
            board(vec![ContinuousModification::AddKeywordWithDerivedCost {
                kind: CostBearingKeywordKind::Foretell,
                derivation: CostDerivation::ManaCostReducedBy(ManaCost::generic(2)),
            }]);
        assert!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Foretell).is_some(),
            "reach-guard: AddKeywordWithDerivedCost must really grant Foretell"
        );
        assert!(shared_effects_can_grant_off_zone_keyword_kind(
            &state,
            KeywordKind::Foretell
        ));
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "a Foretell grant must not answer true for Dredge"
        );

        // --- ADD arm 4: AddChosenKeyword, both polarities of the SOURCE's list.
        let (mut state, source_id, target_id) =
            board(vec![ContinuousModification::AddChosenKeyword]);
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "AddChosenKeyword with an empty chosen list is a no-op, so false is exact"
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Keyword(Keyword::Dredge(3)));
        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Dredge),
            Some(Keyword::Dredge(3)),
            "reach-guard: AddChosenKeyword must really grant the SOURCE's chosen Dredge"
        );
        assert!(
            shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "AddChosenKeyword's keyword identity lives on the granting source"
        );

        // --- REMOVE arms: refused, each on a board where the keyword the arm
        // acts on is really present on the recipient.
        for (label, modification) in [
            (
                "RemoveKeyword",
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Dredge(2),
                },
            ),
            (
                "RemoveChosenKeyword",
                ContinuousModification::RemoveChosenKeyword,
            ),
            (
                "RemoveAllAbilities",
                ContinuousModification::RemoveAllAbilities,
            ),
        ] {
            let (mut state, source_id, target_id) = board(vec![modification]);
            state
                .objects
                .get_mut(&target_id)
                .unwrap()
                .base_keywords
                .push(Keyword::Dredge(2));
            state
                .objects
                .get_mut(&source_id)
                .unwrap()
                .chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::Dredge(2)));
            // Reach-guard: the arm really strips the recipient's printed Dredge.
            assert_eq!(
                effective_off_zone_keyword(&state, target_id, KeywordKind::Dredge),
                None,
                "{label}: the removal arm must really apply to the recipient"
            );
            assert!(
                !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
                "{label}: a removal-only board cannot create a Dredge candidate"
            );
        }
    }

    /// V8 — CR 608.2d + CR 613.1f: the off-zone `AddChosenKeyword` arm must read
    /// the PLURAL chosen-keyword list off the granting source (Greymond's two
    /// chosen abilities), not just the first. A battlefield source carrying TWO
    /// `ChosenAttribute::Keyword` grants both to an off-battlefield recipient.
    #[test]
    fn add_chosen_keyword_off_zone_reads_all_chosen_keywords() {
        use crate::types::ability::ChosenAttribute;

        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Greymond", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Exiled Human", Zone::Exile);

        // Two abilities chosen as Greymond entered.
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::FirstStrike));
            obj.chosen_attributes
                .push(ChosenAttribute::Keyword(Keyword::Lifelink));
        }

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddChosenKeyword],
            None,
        );

        let kws = effective_off_zone_keywords(&state, target_id);
        assert!(
            kws.contains(&Keyword::FirstStrike) && kws.contains(&Keyword::Lifelink),
            "off-zone AddChosenKeyword must surface BOTH chosen keywords, got {kws:?}"
        );
    }

    /// CR 702.138a + CR 601.2g/h: a transient `AddKeyword(Escape)` carrying the
    /// COMPOUND granted cost (mana sub-cost + "exile N other cards from your
    /// graveyard" residual) makes a graveyard card castable via escape —
    /// `effective_escape_data` resolves the mana sub-cost (CR 601.2g) and surfaces
    /// the exile residual for `pay_additional_cost` (CR 601.2h). Runtime proof for
    /// the parser front door `parse_grant_graveyard_keyword_to_target_ir`
    /// (Confession Dial / Desdemona). Tests the building block — a transient
    /// off-zone Escape grant — not a single card.
    #[test]
    fn transient_granted_compound_escape_makes_graveyard_card_castable() {
        use crate::types::ability::AbilityCost;
        use crate::types::keywords::EscapeCost;

        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Snapcaster Mage",
            Zone::Battlefield,
        );
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Scrubland Mongoose",
            Zone::Graveyard,
        );

        let exile_residual = AbilityCost::Exile {
            count: 3,
            zone: Some(Zone::Graveyard),
            filter: None,
        };

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target_id },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Escape(EscapeCost::NonMana(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::SelfManaCost,
                        },
                        exile_residual.clone(),
                    ],
                })),
            }],
            None,
        );

        let (_, residual) = crate::game::keywords::effective_escape_data(&state, target_id)
            .expect("granted compound escape must make the graveyard card castable");
        assert_eq!(residual, exile_residual);
    }

    #[test]
    fn battlefield_static_grants_sneak_to_graveyard_creature() {
        // CR 702.190a: Ninja Teen Level 3 grants Sneak to creature cards in GY.
        // Verifies the off-zone pipeline routes the static's AddKeyword::Sneak
        // through to the GY object, so `effective_sneak_cost` (used by the cost
        // substitution branch in casting.rs) will resolve correctly.
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Ninja Teen", Zone::Battlefield);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Scrubland Mongoose",
            Zone::Graveyard,
        );

        let sneak_cost = ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black],
        };
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Sneak(sneak_cost.clone()),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Sneak),
            Some(Keyword::Sneak(sneak_cost.clone()))
        );
        assert_eq!(
            crate::game::keywords::effective_sneak_cost(&state, target_id),
            Some(sneak_cost)
        );
    }

    #[test]
    fn battlefield_static_grants_keyword_to_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Lier", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }]),
            );

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_keyword_static_respects_condition() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Conditional Source",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }])
                    .condition(StaticCondition::IsPresent {
                        filter: Some(TargetFilter::SpecificObject { id: source_id }),
                    }),
            );
        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));

        state.objects.get_mut(&source_id).unwrap().zone = Zone::Graveyard;
        state.battlefield.retain(|id| *id != source_id);
        state.players[0].graveyard.push_back(source_id);

        assert!(!off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn self_static_in_graveyard_grants_keyword_to_self() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);

        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&card_id)
                .unwrap()
                .base_static_definitions,
        )
        .push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::Green],
                    })),
                }]),
        );
        let base_static_definitions = state
            .objects
            .get(&card_id)
            .unwrap()
            .base_static_definitions
            .clone();
        state.objects.get_mut(&card_id).unwrap().static_definitions =
            (*base_static_definitions).clone().into();

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            })))
        );
    }

    /// Matrix row 3 (F1) — CR 613.1f + CR 113.6b, the guard's hostile fixture:
    /// the ONLY Dredge grant on the board is the recipient's OWN base static
    /// (`affected: SelfRef`), the source a shared-only guard would overlook.
    /// Modeled on `self_static_in_graveyard_grants_keyword_to_self` above, which
    /// is the live proof that this shape is reachable rather than `UNREACHABLE`.
    ///
    /// The configuration asserted here is the only one that proves the two
    /// halves are independent — the shared half `false` WHILE the per-object
    /// half is `true`. Dropping the per-object half would silently skip exactly
    /// this card.
    #[test]
    fn base_statics_half_sees_a_self_granting_graveyard_card() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Dredging Spawn", Zone::Graveyard);

        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&card_id)
                .unwrap()
                .base_static_definitions,
        )
        .push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Dredge(2),
                }]),
        );
        let base_static_definitions = state
            .objects
            .get(&card_id)
            .unwrap()
            .base_static_definitions
            .clone();
        state.objects.get_mut(&card_id).unwrap().static_definitions =
            (*base_static_definitions).clone().into();

        // Reach-guard: the self-static really grants Dredge through the
        // production authority, so the two guard answers below are measured on a
        // live grant.
        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Dredge),
            Some(Keyword::Dredge(2)),
            "reach-guard: the card's own base static must really grant it Dredge 2"
        );
        assert!(
            base_statics_can_grant_off_zone_keyword_kind(&state, card_id, KeywordKind::Dredge),
            "the per-object half must see a graveyard card's OWN base-static grant"
        );
        assert!(
            !shared_effects_can_grant_off_zone_keyword_kind(&state, KeywordKind::Dredge),
            "the shared half must NOT see it — that is exactly what makes the \
             per-object half load-bearing rather than redundant"
        );

        // Negative sibling: the same shape with an empty `base_static_definitions`.
        let mut bare = GameState::new_two_player(42);
        let bare_id = create_card(&mut bare, PlayerId(0), "Plain Land", Zone::Graveyard);
        assert!(
            !base_statics_can_grant_off_zone_keyword_kind(&bare, bare_id, KeywordKind::Dredge),
            "a graveyard card with no base statics cannot self-grant"
        );
    }

    #[test]
    fn command_zone_emblem_grants_keyword_to_non_battlefield_card() {
        let mut state = GameState::new_two_player(42);
        let emblem_id = create_card(&mut state, PlayerId(0), "Emblem", Zone::Command);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Exile);

        {
            let emblem = state.objects.get_mut(&emblem_id).unwrap();
            emblem.is_emblem = true;
            emblem.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    }]),
            );
        }

        assert!(off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_keyword_static_matches_owner_scoped_hand_card_with_stale_controller() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(
            &mut state,
            PlayerId(0),
            "Singing Towers Source",
            Zone::Battlefield,
        );
        let target_id = create_card(&mut state, PlayerId(0), "Expensive Spell", Zone::Hand);
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.controller = PlayerId(1);
            target.mana_cost = ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Blue],
            };
        }

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::card()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::InAnyZone {
                                zones: vec![Zone::Hand],
                            }]),
                    ))
                    .modifications(vec![ContinuousModification::AddKeywordWithDerivedCost {
                        kind: CostBearingKeywordKind::Foretell,
                        derivation: CostDerivation::ManaCostReducedBy(ManaCost::generic(2)),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Foretell),
            Some(Keyword::Foretell(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            }))
        );
    }

    #[test]
    fn remove_keyword_suppresses_matching_keyword_kind() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Red],
                        })),
                    }]),
            );

        assert!(!off_zone_has_keyword_kind(
            &state,
            target_id,
            KeywordKind::Flashback
        ));
    }

    #[test]
    fn off_zone_queries_ignore_non_base_keyword_residue() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Creature", Zone::Graveyard);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        assert!(effective_off_zone_keywords(&state, card_id).is_empty());
    }

    #[test]
    fn off_zone_self_statics_use_base_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Green],
                })),
            }]);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .base_static_definitions = Arc::new(vec![static_def]);
        state
            .objects
            .get_mut(&card_id)
            .unwrap()
            .static_definitions
            .clear();

        assert_eq!(
            effective_off_zone_keyword(&state, card_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            })))
        );
    }

    #[test]
    fn remove_all_abilities_clears_keywords_for_query() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(
            &mut state,
            PlayerId(0),
            "Faithless Looting",
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Red],
            })));
        let base_keywords = state.objects.get(&target_id).unwrap().base_keywords.clone();
        state.objects.get_mut(&target_id).unwrap().keywords = base_keywords;

        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::RemoveAllAbilities]),
            );

        assert!(effective_off_zone_keywords(&state, target_id).is_empty());
    }

    #[test]
    fn add_dynamic_keyword_uses_quantity_resolution() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        let target_id = create_card(&mut state, PlayerId(0), "Arcbound Ravager", Zone::Graveyard);

        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddDynamicKeyword {
                        kind: DynamicKeywordKind::Modular,
                        value: QuantityExpr::Fixed { value: 3 },
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Modular),
            Some(Keyword::Modular(3))
        );
    }

    #[test]
    fn later_effect_replaces_same_keyword_kind_payload() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard);
        let earlier_id = create_card(&mut state, PlayerId(0), "Earlier", Zone::Battlefield);
        let later_id = create_card(&mut state, PlayerId(0), "Later", Zone::Battlefield);

        state
            .objects
            .get_mut(&earlier_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::Blue],
                        })),
                    }]),
            );
        state
            .objects
            .get_mut(&later_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: target_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Blue],
                        })),
                    }]),
            );

        assert_eq!(
            effective_off_zone_keyword(&state, target_id, KeywordKind::Flashback),
            Some(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            })))
        );
    }

    // -- Keyword-only gather and scoped sharing --------------------------------

    use crate::game::layers::collect_shared_active_continuous_effects;
    use crate::types::statics::StaticMode;

    /// Flashback whose cost is the card's own mana cost.
    fn flashback() -> Keyword {
        Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost))
    }

    /// A filter matching any card in a graveyard.
    fn graveyard_cards() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InAnyZone {
            zones: vec![Zone::Graveyard],
        }]))
    }

    /// An admission predicate that keeps every modification.
    fn admit_every(_: &ContinuousModification) -> bool {
        true
    }

    /// The `Debug` rendering of each effect, in order.
    fn debug_each(effects: &[ActiveContinuousEffect]) -> Vec<String> {
        effects.iter().map(|effect| format!("{effect:?}")).collect()
    }

    /// The unchanged full gather narrowed by the keyword predicate: the oracle
    /// the filtered gather must reproduce element for element.
    fn full_gather_narrowed(state: &GameState) -> Vec<String> {
        collect_shared_active_continuous_effects(state)
            .iter()
            .filter(|effect| supports_off_zone_keyword_query(&effect.modification))
            .map(|effect| format!("{effect:?}"))
            .collect()
    }

    /// Appends `definition` to `source`'s `static_definitions`.
    fn push_static(state: &mut GameState, source: ObjectId, definition: StaticDefinition) {
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(definition);
    }

    /// A continuous static whose only modification adds `keyword` to `affected`.
    fn keyword_static(affected: TargetFilter, keyword: Keyword) -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword { keyword }])
    }

    /// No definition index and no transient id: how these tests spot a grant-expansion child.
    fn is_grant_child(effect: &ActiveContinuousEffect) -> bool {
        effect.def_index.is_none() && effect.transient_id.is_none()
    }

    /// The ids of every object not on the battlefield, sorted.
    fn off_battlefield_ids(state: &GameState) -> Vec<ObjectId> {
        let mut ids: Vec<ObjectId> = state
            .objects
            .iter()
            .filter(|(_, obj)| obj.zone != Zone::Battlefield)
            .map(|(id, _)| *id)
            .collect();
        ids.sort();
        ids
    }

    /// `effective_off_zone_keywords` for each id, in order.
    fn keywords_of_each(state: &GameState, ids: &[ObjectId]) -> Vec<Vec<Keyword>> {
        ids.iter()
            .map(|id| effective_off_zone_keywords(state, *id))
            .collect()
    }

    /// The keyword-filtered gather equals the full gather narrowed by the keyword predicate.
    #[test]
    fn keyword_filtered_gather_is_bit_identical_to_full_gather() {
        let mut state = GameState::new_two_player(42);
        let lier = create_card(&mut state, PlayerId(0), "Lier", Zone::Battlefield);
        let stripper = create_card(&mut state, PlayerId(0), "Stripper", Zone::Battlefield);
        let granter = create_card(&mut state, PlayerId(0), "Granter", Zone::Battlefield);
        let bear_a = create_card(&mut state, PlayerId(0), "Bear A", Zone::Battlefield);
        let bear_b = create_card(&mut state, PlayerId(0), "Bear B", Zone::Battlefield);
        let opt = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);
        let consider = create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard);
        let spawning = create_card(&mut state, PlayerId(0), "Viral Spawning", Zone::Graveyard);
        let anchor = create_card(&mut state, PlayerId(0), "Graveyard Anchor", Zone::Graveyard);

        push_static(
            &mut state,
            lier,
            keyword_static(graveyard_cards(), flashback()),
        );
        // A non-keyword anthem and a keyword removal in one list.
        push_static(
            &mut state,
            stripper,
            StaticDefinition::continuous()
                .affected(graveyard_cards())
                .modifications(vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::RemoveKeyword {
                        keyword: Keyword::Dredge(1),
                    },
                ]),
        );
        push_static(
            &mut state,
            stripper,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: consider })
                .modifications(vec![ContinuousModification::RemoveAllAbilities]),
        );
        push_static(
            &mut state,
            stripper,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear_a })
                .modifications(vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ]),
        );
        // A granted static wrapping a keyword grant, with two battlefield recipients.
        push_static(
            &mut state,
            granter,
            StaticDefinition::continuous()
                .affected(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::SpecificObject { id: bear_a },
                        TargetFilter::SpecificObject { id: bear_b },
                    ],
                })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(keyword_static(graveyard_cards(), Keyword::Dredge(2))),
                }]),
        );
        // A static that functions from the graveyard.
        push_static(
            &mut state,
            anchor,
            keyword_static(TargetFilter::SpecificObject { id: opt }, Keyword::Dredge(4))
                .active_zones(vec![Zone::Graveyard]),
        );
        // A self-referential own-base static.
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&spawning)
                .unwrap()
                .base_static_definitions,
        )
        .push(keyword_static(TargetFilter::SelfRef, Keyword::Dredge(3)));
        // A transient keyword grant with a layered non-keyword modification ahead
        // of its `AddKeyword`, and a non-keyword transient.
        state.add_transient_continuous_effect(
            lier,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: opt },
            vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Dredge(5),
                },
            ],
            None,
        );
        state.add_transient_continuous_effect(
            lier,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: bear_a },
            vec![ContinuousModification::AddPower { value: 2 }],
            None,
        );

        let full = collect_shared_active_continuous_effects(&state);
        let (filtered, _) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );

        // Reach-guards.
        assert!(
            !filtered.is_empty(),
            "reach-guard: the board has keyword effects"
        );
        assert!(
            full.len() > filtered.len(),
            "reach-guard: the board has non-keyword effects to drop"
        );
        assert!(
            filtered.iter().any(is_grant_child),
            "reach-guard: a granted-static child must survive the filter"
        );
        assert!(
            filtered.iter().any(|effect| effect.transient_id.is_some()),
            "reach-guard: the mixed transient's keyword must survive the filter"
        );
        assert!(
            filtered.iter().any(|effect| effect.source_id == anchor),
            "reach-guard: the graveyard-functioning static must be gathered"
        );

        assert_eq!(debug_each(&filtered), full_gather_narrowed(&state));

        // Literal order of the unfiltered gather: every grant child precedes its
        // host meta-effect.
        let hosts: Vec<usize> = full
            .iter()
            .enumerate()
            .filter(|(_, effect)| {
                matches!(
                    effect.modification,
                    ContinuousModification::GrantStaticAbility { .. }
                )
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(hosts.len(), 1, "reach-guard: exactly one host meta-effect");
        let children: Vec<usize> = full
            .iter()
            .enumerate()
            .filter(|(_, effect)| is_grant_child(effect))
            .map(|(index, _)| index)
            .collect();
        assert!(
            !children.is_empty(),
            "reach-guard: at least one grant child"
        );
        assert!(
            children.iter().all(|child| *child < hosts[0]),
            "grant children must be gathered before their host: children {children:?}, host {hosts:?}"
        );

        // Per-object: the production query equals the unfiltered recipe.
        let mut any_applicable = false;
        for id in off_battlefield_ids(&state) {
            let obj = state.objects.get(&id).unwrap();
            let mut recipe = collect_shared_active_continuous_effects(&state);
            if !(obj.zone == Zone::Command && obj.is_emblem) {
                recipe.extend(active_continuous_effects_from_base_static_source(
                    &state,
                    obj,
                    admit_every,
                ));
            }
            let recipe: Vec<String> = recipe
                .iter()
                .filter(|effect| off_zone_keyword_effect_applies(&state, id, obj.zone, effect))
                .map(|effect| format!("{effect:?}"))
                .collect();
            any_applicable |= !recipe.is_empty();
            assert_eq!(
                debug_each(&collect_applicable_off_zone_keyword_effects(&state, id)),
                recipe,
                "object {id:?}"
            );
        }
        assert!(
            any_applicable,
            "reach-guard: some object has applicable effects"
        );
        assert!(
            effective_off_zone_keywords(&state, spawning).contains(&Keyword::Dredge(3)),
            "reach-guard: the own-base static applies to its own card"
        );
    }

    /// An anthem, a live non-keyword transient, and a granted static whose inner
    /// definition is non-keyword.
    fn non_keyword_board() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Anthem", Zone::Battlefield);
        let bear = create_card(&mut state, PlayerId(0), "Bear", Zone::Battlefield);
        push_static(
            &mut state,
            source,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear })
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
        push_static(
            &mut state,
            source,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(
                        StaticDefinition::continuous()
                            .affected(TargetFilter::SpecificObject { id: bear })
                            .modifications(vec![ContinuousModification::AddToughness { value: 1 }]),
                    ),
                }]),
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: bear },
            vec![ContinuousModification::AddPower { value: 2 }],
            None,
        );
        (state, source, bear)
    }

    /// With no keyword effect on the board the filtered gather is empty and `Structural`.
    #[test]
    fn keyword_filtered_gather_skips_non_keyword_work() {
        let (state, _, _) = non_keyword_board();

        // Reach-guard: both context-dependent statements are reachable on this
        // board through the full gather.
        let full = collect_shared_active_continuous_effects(&state);
        assert!(full.iter().any(|effect| effect.transient_id.is_some()));
        assert!(full.iter().any(is_grant_child));

        let (filtered, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert!(
            filtered.is_empty(),
            "no keyword effect exists on this board: {filtered:?}"
        );
        assert_eq!(dependence, GatherDependence::Structural);
    }

    /// A keyword transient or keyword-inner grant makes the filtered gather `ContextDependent`.
    #[test]
    fn keyword_relevant_transient_or_grant_marks_gather_context_dependent() {
        // A live keyword transient.
        let (mut state, source, _) = non_keyword_board();
        let opt = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: opt },
            vec![ContinuousModification::AddKeyword {
                keyword: flashback(),
            }],
            None,
        );
        let (filtered, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert_eq!(dependence, GatherDependence::ContextDependent);
        assert!(
            filtered.iter().any(|effect| effect.transient_id.is_some()
                && matches!(
                    effect.modification,
                    ContinuousModification::AddKeyword { .. }
                )),
            "reach-guard: the live keyword transient is gathered"
        );

        // A keyword transient whose recipient-independent source condition is
        // false: it contributes nothing, and deciding that already needed the
        // state-dependent liveness check.
        let (mut state, source, _) = non_keyword_board();
        let opt = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: opt },
            vec![ContinuousModification::AddKeyword {
                keyword: flashback(),
            }],
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::SpecificObject {
                    id: ObjectId(9_999_999),
                }),
            }),
        );
        let (filtered, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert!(
            filtered.is_empty(),
            "a false source condition yields no effect: {filtered:?}"
        );
        assert_eq!(dependence, GatherDependence::ContextDependent);

        // A keyword-inner granted static with a matching host.
        let (mut state, source, bear) = non_keyword_board();
        push_static(
            &mut state,
            source,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(keyword_static(graveyard_cards(), flashback())),
                }]),
        );
        let (filtered, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert_eq!(dependence, GatherDependence::ContextDependent);
        assert!(
            filtered.iter().any(is_grant_child),
            "reach-guard: the keyword grant child is gathered"
        );

        // The same grant when its host filter matches no permanent.
        let (mut state, source, _) = non_keyword_board();
        push_static(
            &mut state,
            source,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject {
                    id: ObjectId(9_999_999),
                })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(keyword_static(graveyard_cards(), flashback())),
                }]),
        );
        let (filtered, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert!(filtered.is_empty(), "no host, no child: {filtered:?}");
        assert_eq!(dependence, GatherDependence::ContextDependent);
    }

    #[derive(Clone, Copy, Debug)]
    enum GatherPosition {
        /// Top-level modification of a battlefield permanent's static.
        Static,
        /// Inner modification of a granted static with one matching recipient.
        Granted,
        /// Modification of a live transient effect.
        Transient,
    }

    const ALL_GATHER_POSITIONS: [GatherPosition; 3] = [
        GatherPosition::Static,
        GatherPosition::Granted,
        GatherPosition::Transient,
    ];

    /// A board whose only modification list is `[hostile, AddKeyword]`, placed
    /// at `position`.
    fn hostile_first_board(position: GatherPosition, hostile: ContinuousModification) -> GameState {
        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Source", Zone::Battlefield);
        let bear = create_card(&mut state, PlayerId(0), "Bear", Zone::Battlefield);
        let opt = create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard);
        let modifications = vec![
            hostile,
            ContinuousModification::AddKeyword {
                keyword: flashback(),
            },
        ];
        let affected = TargetFilter::SpecificObject { id: opt };
        match position {
            GatherPosition::Static => push_static(
                &mut state,
                source,
                StaticDefinition::continuous()
                    .affected(affected)
                    .modifications(modifications),
            ),
            GatherPosition::Granted => push_static(
                &mut state,
                source,
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: bear })
                    .modifications(vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(
                            StaticDefinition::continuous()
                                .affected(affected)
                                .modifications(modifications),
                        ),
                    }]),
            ),
            GatherPosition::Transient => {
                state.add_transient_continuous_effect(
                    source,
                    PlayerId(0),
                    Duration::UntilEndOfTurn,
                    affected,
                    modifications,
                    None,
                );
            }
        }
        state
    }

    /// Whether any effect's own modification is `AddKeyword` of `flashback()`.
    fn carries_sibling_keyword(effects: &[ActiveContinuousEffect]) -> bool {
        effects.iter().any(|effect| {
            matches!(
                &effect.modification,
                ContinuousModification::AddKeyword { keyword } if *keyword == flashback()
            )
        })
    }

    /// Whether any effect's own top-level modification is the same variant as `hostile`.
    fn carries_same_variant(
        effects: &[ActiveContinuousEffect],
        hostile: &ContinuousModification,
    ) -> bool {
        effects.iter().any(|effect| {
            std::mem::discriminant(&effect.modification) == std::mem::discriminant(hostile)
        })
    }

    /// A hostile modification ahead of an `AddKeyword` is dropped; the keyword is still gathered.
    #[test]
    fn keyword_admission_is_total_at_every_gather_position() {
        use crate::types::counter::CounterType;
        use crate::types::statics::{
            ActivationExemption, CastFreeOrigin, CastFrequency, CostModifyMode,
        };

        // Modifications that never pass through the layer system:
        // `ContinuousModification::layer()` has no answer for them, so the full
        // gather cannot be asked. Only the filtered entry point is exercised.
        let never_layered = || {
            vec![
                ContinuousModification::AddCounterOnEnter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    if_type: None,
                },
                ContinuousModification::SetStartingLoyalty { value: 3 },
                ContinuousModification::RemoveManaCost,
            ]
        };
        for position in ALL_GATHER_POSITIONS {
            for hostile in never_layered() {
                let state = hostile_first_board(position, hostile.clone());
                let (filtered, _) = collect_shared_active_continuous_effects_matching(
                    &state,
                    supports_off_zone_keyword_query,
                );
                assert!(
                    carries_sibling_keyword(&filtered),
                    "{position:?} {hostile:?}: the sibling AddKeyword must be gathered"
                );
                assert!(
                    !carries_same_variant(&filtered, &hostile),
                    "{position:?} {hostile:?}: the hostile modification must be dropped"
                );
            }
        }

        // Combat-assignment rule modifications.
        let combat_assignment = || {
            vec![
                ContinuousModification::AssignDamageFromToughness,
                ContinuousModification::AssignDamageAsThoughUnblocked,
                ContinuousModification::AssignNoCombatDamage,
            ]
        };
        for position in ALL_GATHER_POSITIONS {
            for hostile in combat_assignment() {
                let state = hostile_first_board(position, hostile.clone());
                let (filtered, _) = collect_shared_active_continuous_effects_matching(
                    &state,
                    supports_off_zone_keyword_query,
                );
                assert!(
                    carries_sibling_keyword(&filtered),
                    "{position:?} {hostile:?}: the sibling AddKeyword must be gathered"
                );
                assert!(
                    !carries_same_variant(&filtered, &hostile),
                    "{position:?} {hostile:?}: the hostile modification must be dropped"
                );
                assert_eq!(
                    debug_each(&filtered),
                    full_gather_narrowed(&state),
                    "{position:?} {hostile:?}"
                );
            }
        }

        // Static-mode grants the gather skips before it ever builds an effect.
        let goaded = ContinuousModification::AddStaticMode {
            mode: StaticMode::Goaded,
        };
        let mut static_mode_cases: Vec<(GatherPosition, ContinuousModification)> =
            ALL_GATHER_POSITIONS
                .into_iter()
                .map(|position| (position, goaded.clone()))
                .collect();
        static_mode_cases.extend(
            [
                StaticMode::MayLookAtFaceDown,
                StaticMode::ReduceAbilityCost {
                    mode: CostModifyMode::Reduce,
                    keyword: "ninjutsu".to_string(),
                    amount: 1,
                    minimum_mana: None,
                    dynamic_count: None,
                    exemption: ActivationExemption::None,
                    activator: None,
                    targets: None,
                    frequency: None,
                },
                StaticMode::CastFromHandFree {
                    frequency: CastFrequency::Unlimited,
                    origin: CastFreeOrigin::Hand,
                },
            ]
            .into_iter()
            .map(|mode| {
                (
                    GatherPosition::Transient,
                    ContinuousModification::AddStaticMode { mode },
                )
            }),
        );
        for (position, hostile) in static_mode_cases {
            let state = hostile_first_board(position, hostile.clone());
            let (filtered, _) = collect_shared_active_continuous_effects_matching(
                &state,
                supports_off_zone_keyword_query,
            );
            let full = collect_shared_active_continuous_effects(&state);
            assert!(
                carries_sibling_keyword(&filtered),
                "{position:?} {hostile:?}: the sibling AddKeyword must be gathered"
            );
            assert!(
                carries_sibling_keyword(&full),
                "reach-guard {position:?} {hostile:?}: the unfiltered gather reaches the list"
            );
            assert_eq!(
                debug_each(&filtered),
                full_gather_narrowed(&state),
                "{position:?} {hostile:?}"
            );
            // A top-level match on each effect's own modification, not a search
            // of a granted definition: the unfiltered host meta-effect still
            // lists the static mode inside its definition.
            assert!(
                !carries_same_variant(&filtered, &hostile),
                "{position:?} {hostile:?}: filtered gather must not carry the static mode"
            );
            assert!(
                !carries_same_variant(&full, &hostile),
                "{position:?} {hostile:?}: unfiltered gather must not carry the static mode"
            );
        }
    }

    /// One battlefield source granting three different keywords to three
    /// graveyard cards, one each. No transient and no granted static, so the
    /// keyword gather is structural.
    fn sharing_board() -> (GameState, ObjectId, [ObjectId; 3]) {
        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Lier", Zone::Battlefield);
        let cards = [
            create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard),
        ];
        push_static(
            &mut state,
            source,
            keyword_static(TargetFilter::SpecificObject { id: cards[0] }, flashback()),
        );
        push_static(
            &mut state,
            source,
            keyword_static(
                TargetFilter::SpecificObject { id: cards[1] },
                Keyword::Dredge(1),
            ),
        );
        push_static(
            &mut state,
            source,
            keyword_static(
                TargetFilter::SpecificObject { id: cards[2] },
                Keyword::Dredge(2),
            ),
        );
        (state, source, cards)
    }

    /// Runs the three queries inside one scope and returns the results with
    /// the tallies they produced.
    fn scoped_queries(
        state: &GameState,
        ids: &[ObjectId],
    ) -> (Vec<Vec<Keyword>>, OffZoneKeywordScopeCounts) {
        reset_off_zone_keyword_scope_counts();
        let keywords = with_off_zone_keyword_scope(state, || keywords_of_each(state, ids));
        (keywords, off_zone_keyword_scope_counts())
    }

    /// A keyword transient makes scoped queries fall open, with results equal to unscoped ones.
    #[test]
    fn scope_falls_open_on_keyword_relevant_transient() {
        let (mut state, source, cards) = sharing_board();

        // Paired positive: without the transient the scope shares.
        let (shared, counts) = scoped_queries(&state, &cards);
        assert_eq!(shared, keywords_of_each(&state, &cards));
        assert_eq!((counts.fills, counts.hits), (1, 2), "{counts:?}");

        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: cards[2] },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Dredge(7),
            }],
            None,
        );
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside[2].contains(&Keyword::Dredge(7)),
            "reach-guard: the transient's keyword is granted: {outside:?}"
        );
        let (inside, counts) = scoped_queries(&state, &cards);
        assert_eq!(inside, outside);
        assert_eq!(
            (counts.fills, counts.hits, counts.context_fall_opens),
            (0, 0, 3),
            "{counts:?}"
        );
    }

    /// A keyword-inner granted static makes scoped queries fall open, with unscoped results.
    #[test]
    fn scope_falls_open_on_keyword_relevant_static_grant() {
        let (mut state, source, cards) = sharing_board();
        let bear = create_card(&mut state, PlayerId(0), "Bear", Zone::Battlefield);

        // Paired positive: without the granted static the scope shares.
        let (shared, counts) = scoped_queries(&state, &cards);
        assert_eq!(shared, keywords_of_each(&state, &cards));
        assert_eq!((counts.fills, counts.hits), (1, 2), "{counts:?}");

        push_static(
            &mut state,
            source,
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: bear })
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(keyword_static(
                        TargetFilter::SpecificObject { id: cards[2] },
                        Keyword::Dredge(7),
                    )),
                }]),
        );
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside[2].contains(&Keyword::Dredge(7)),
            "reach-guard: the granted static's keyword is granted: {outside:?}"
        );
        let (inside, counts) = scoped_queries(&state, &cards);
        assert_eq!(inside, outside);
        assert_eq!(
            (counts.fills, counts.hits, counts.context_fall_opens),
            (0, 0, 3),
            "{counts:?}"
        );
    }

    /// A query on a clone made inside a scope is unmatched and returns the clone's own result.
    #[test]
    fn scope_does_not_serve_a_cloned_state() {
        let (state, source, cards) = sharing_board();
        reset_off_zone_keyword_scope_counts();
        let (original_first, cloned, original_second) = with_off_zone_keyword_scope(&state, || {
            let original_first = effective_off_zone_keywords(&state, cards[0]);
            let mut clone = state.clone();
            clone.objects.get_mut(&source).unwrap().static_definitions = Vec::new().into();
            let before = off_zone_keyword_scope_counts();
            let cloned = effective_off_zone_keywords(&clone, cards[0]);
            let after = off_zone_keyword_scope_counts();
            assert_eq!(after.unmatched, before.unmatched + 1, "{after:?}");
            assert_eq!(after.hits, before.hits, "the clone must not hit: {after:?}");
            assert_eq!(after.matched, before.matched, "{after:?}");
            let original_second = effective_off_zone_keywords(&state, cards[0]);
            (original_first, cloned, original_second)
        });
        let counts = off_zone_keyword_scope_counts();
        assert_eq!(
            (counts.unmatched, counts.fills, counts.hits),
            (1, 1, 1),
            "reach-guard: the original fills then hits: {counts:?}"
        );
        assert_eq!(original_first, vec![flashback()]);
        assert_eq!(original_second, original_first);
        assert!(
            cloned.is_empty(),
            "the clone lost its granting static: {cloned:?}"
        );
        assert_ne!(cloned, original_second);
    }

    use crate::game::layers::{
        off_zone_static_source_scan_count, reset_off_zone_static_source_scan_count,
    };

    /// `sharing_board` plus a static that functions from the graveyard, granting
    /// Dredge 4 to the first card, and a keyword transient granting flashback to
    /// the third, so every scoped query falls open.
    fn falling_open_board() -> (GameState, ObjectId, [ObjectId; 3]) {
        let (mut state, source, cards) = sharing_board();
        let anchor = create_card(&mut state, PlayerId(0), "Graveyard Anchor", Zone::Graveyard);
        push_static(
            &mut state,
            anchor,
            keyword_static(
                TargetFilter::SpecificObject { id: cards[0] },
                Keyword::Dredge(4),
            )
            .active_zones(vec![Zone::Graveyard]),
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: cards[2] },
            vec![ContinuousModification::AddKeyword {
                keyword: flashback(),
            }],
            None,
        );
        (state, anchor, cards)
    }

    /// Falling-open queries in one scope share one off-zone source scan, with unscoped results.
    #[test]
    fn falling_open_scope_scans_off_zone_static_sources_once() {
        let (state, _, cards) = falling_open_board();
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside[0].contains(&Keyword::Dredge(4)),
            "reach-guard: the graveyard-functioning static grants its keyword: {outside:?}"
        );
        assert!(
            outside[2].contains(&flashback()),
            "reach-guard: the keyword transient is live: {outside:?}"
        );

        reset_off_zone_static_source_scan_count();
        let (inside, counts) = scoped_queries(&state, &cards);
        assert_eq!(inside, outside);
        assert_eq!(
            counts.context_fall_opens, 3,
            "reach-guard: every query falls open: {counts:?}"
        );
        assert_eq!(
            off_zone_static_source_scan_count(),
            1,
            "one source scan per scope, not one per query"
        );
    }

    /// A scope's source list ends with the scope: a later change is seen outside and by a new scope.
    #[test]
    fn off_zone_static_source_list_ends_with_its_scope() {
        let (mut state, _, cards) = sharing_board();

        reset_off_zone_static_source_scan_count();
        with_off_zone_keyword_scope(&state, || ());
        assert_eq!(
            off_zone_static_source_scan_count(),
            0,
            "a scope that never gathers never scans"
        );

        reset_off_zone_static_source_scan_count();
        let (before, counts) = scoped_queries(&state, &cards);
        assert_eq!(
            off_zone_static_source_scan_count(),
            1,
            "reach-guard: the structural scope scanned"
        );
        assert_eq!(
            (counts.fills, counts.hits),
            (1, 2),
            "reach-guard: the structural scope filled: {counts:?}"
        );
        assert_eq!(off_zone_keyword_scope_depth(), 0);

        // Same binding, so the same address as the ended scope's state.
        let late = create_card(&mut state, PlayerId(0), "Late Anchor", Zone::Graveyard);
        push_static(
            &mut state,
            late,
            keyword_static(
                TargetFilter::SpecificObject { id: cards[0] },
                Keyword::Dredge(5),
            )
            .active_zones(vec![Zone::Graveyard]),
        );
        assert!(!before[0].contains(&Keyword::Dredge(5)), "{before:?}");
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside[0].contains(&Keyword::Dredge(5)),
            "an unscoped query walks the live state: {outside:?}"
        );

        reset_off_zone_static_source_scan_count();
        let (inside, _) = scoped_queries(&state, &cards);
        assert_eq!(inside, outside);
        assert_eq!(
            off_zone_static_source_scan_count(),
            1,
            "a new scope on the same address scans afresh"
        );
        assert_eq!(off_zone_keyword_scope_depth(), 0);
    }

    /// Nested scopes on two states each keep their own source list, whichever frame is innermost.
    #[test]
    fn nested_scopes_keep_their_own_off_zone_static_sources() {
        let (state_a, anchor, cards) = falling_open_board();
        let mut state_b = state_a.clone();
        state_b.objects.get_mut(&anchor).unwrap().static_definitions = Vec::new().into();
        let outside_a = keywords_of_each(&state_a, &cards);
        let outside_b = keywords_of_each(&state_b, &cards);
        assert!(
            outside_a[0].contains(&Keyword::Dredge(4)),
            "reach-guard: A's graveyard-functioning static grants its keyword: {outside_a:?}"
        );
        assert!(
            !outside_b[0].contains(&Keyword::Dredge(4)),
            "B lacks A's graveyard-functioning static: {outside_b:?}"
        );

        reset_off_zone_static_source_scan_count();
        reset_off_zone_keyword_scope_counts();
        let (a1, b, a2) = with_off_zone_keyword_scope(&state_a, || {
            let a1 = keywords_of_each(&state_a, &cards);
            // A is queried again while B's frame is innermost.
            let (b, a2) = with_off_zone_keyword_scope(&state_b, || {
                (
                    keywords_of_each(&state_b, &cards),
                    keywords_of_each(&state_a, &cards),
                )
            });
            (a1, b, a2)
        });
        assert_eq!(a1, outside_a);
        assert_eq!(b, outside_b);
        assert_eq!(a2, outside_a);
        let counts = off_zone_keyword_scope_counts();
        assert_eq!(
            counts.context_fall_opens, 9,
            "reach-guard: every query falls open: {counts:?}"
        );
        assert_eq!(off_zone_static_source_scan_count(), 2, "one scan per frame");
        assert_eq!(off_zone_keyword_scope_depth(), 0);
    }

    /// Same-state nesting adds no frame, another state adds one, and an unwind pops its frame.
    #[test]
    fn nested_scopes_and_unwind_leave_stack_empty() {
        let (state, _, cards) = sharing_board();

        // A nested scope on the same state adds no frame.
        reset_off_zone_keyword_scope_counts();
        with_off_zone_keyword_scope(&state, || {
            effective_off_zone_keywords(&state, cards[0]);
            assert_eq!(off_zone_keyword_scope_depth(), 1);
            with_off_zone_keyword_scope(&state, || {
                assert_eq!(off_zone_keyword_scope_depth(), 1);
                let before = off_zone_keyword_scope_counts().hits;
                effective_off_zone_keywords(&state, cards[1]);
                assert_eq!(
                    off_zone_keyword_scope_counts().hits,
                    before + 1,
                    "reach-guard: the inner scope is served by the outer frame"
                );
            });
            assert_eq!(off_zone_keyword_scope_depth(), 1);
            effective_off_zone_keywords(&state, cards[2]);
        });
        let counts = off_zone_keyword_scope_counts();
        assert_eq!((counts.fills, counts.hits), (1, 2), "{counts:?}");
        assert_eq!(off_zone_keyword_scope_depth(), 0);

        // A nested scope on a different state has its own frame.
        let other = state.clone();
        reset_off_zone_keyword_scope_counts();
        with_off_zone_keyword_scope(&state, || {
            effective_off_zone_keywords(&state, cards[0]);
            with_off_zone_keyword_scope(&other, || {
                assert_eq!(off_zone_keyword_scope_depth(), 2);
                effective_off_zone_keywords(&other, cards[0]);
            });
            assert_eq!(off_zone_keyword_scope_depth(), 1);
        });
        assert_eq!(off_zone_keyword_scope_counts().fills, 2);
        assert_eq!(off_zone_keyword_scope_depth(), 0);

        // An unwind out of the closure still pops the frame.
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_off_zone_keyword_scope(&state, || {
                effective_off_zone_keywords(&state, cards[0]);
                assert_eq!(off_zone_keyword_scope_depth(), 1);
                std::panic::resume_unwind(Box::new("unwind out of the scope"));
            })
        }));
        assert!(unwound.is_err());
        assert_eq!(off_zone_keyword_scope_depth(), 0);
    }

    /// Scoped equals unscoped under self-reference and nested queries; a held slice stays unchanged.
    #[test]
    fn recursion_guard_and_reentrancy_inside_scope() {
        // (a) "Cards in your graveyard without foretell have foretell."
        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Foreteller", Zone::Battlefield);
        let cards = [
            create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard),
        ];
        push_static(
            &mut state,
            source,
            keyword_static(
                TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::InAnyZone {
                        zones: vec![Zone::Graveyard],
                    },
                    FilterProp::WithoutKeywordKind {
                        value: KeywordKind::Foretell,
                    },
                ])),
                Keyword::Foretell(ManaCost::generic(2)),
            ),
        );
        // Per-object differences, so anything a scope shared per object would
        // show: one shared grant reaches only the last card, and the first
        // card's own base static (functioning from the graveyard) names every
        // graveyard card but is gathered only for its own query.
        push_static(
            &mut state,
            source,
            keyword_static(TargetFilter::SpecificObject { id: cards[2] }, flashback()),
        );
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&cards[0])
                .unwrap()
                .base_static_definitions,
        )
        .push(
            keyword_static(graveyard_cards(), Keyword::Dredge(7))
                .active_zones(vec![Zone::Graveyard]),
        );
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside
                .iter()
                .all(|keywords| keywords.contains(&Keyword::Foretell(ManaCost::generic(2)))),
            "reach-guard: the self-referential grant applies: {outside:?}"
        );
        assert!(
            outside[0].contains(&Keyword::Dredge(7)) && !outside[1].contains(&Keyword::Dredge(7)),
            "reach-guard: only the first card's own query gathers its base static: {outside:?}"
        );
        assert!(
            outside[2].contains(&flashback()) && !outside[0].contains(&flashback()),
            "reach-guard: the shared grant reaches only the last card: {outside:?}"
        );
        let (inside, counts) = scoped_queries(&state, &cards);
        assert_eq!(inside, outside);
        assert!(counts.hits > 0, "{counts:?}");

        // (b) A filled slice stays readable through a held handle while another
        // state's frame is pushed, filled and popped.
        let (state_a, _, cards_a) = sharing_board();
        let state_b = state_a.clone();
        reset_off_zone_keyword_scope_counts();
        with_off_zone_keyword_scope(&state_a, || {
            effective_off_zone_keywords(&state_a, cards_a[0]);
            let OffZoneKeywordFrameLookup::Reusable(held) = lookup_off_zone_keyword_frame(&state_a)
            else {
                panic!("reach-guard: the first query must have filled the frame");
            };
            let before = debug_each(&held);
            let depth = off_zone_keyword_scope_depth();
            with_off_zone_keyword_scope(&state_b, || {
                effective_off_zone_keywords(&state_b, cards_a[0]);
            });
            let hits = off_zone_keyword_scope_counts().hits;
            with_off_zone_keyword_scope(&state_a, || {
                effective_off_zone_keywords(&state_a, cards_a[1]);
            });
            assert_eq!(off_zone_keyword_scope_counts().hits, hits + 1);
            assert_eq!(debug_each(&held), before);
            assert_eq!(off_zone_keyword_scope_depth(), depth);
        });
        assert_eq!(off_zone_keyword_scope_counts().fills, 2);

        // (c) A shared keyword effect whose condition asks whether a graveyard
        // card has flashback: evaluating it for one card queries the others
        // while the first card's filter is still running.
        let mut state = GameState::new_two_player(42);
        let source = create_card(&mut state, PlayerId(0), "Conditional", Zone::Battlefield);
        let cards = [
            create_card(&mut state, PlayerId(0), "Opt", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Consider", Zone::Graveyard),
            create_card(&mut state, PlayerId(0), "Think Twice", Zone::Graveyard),
        ];
        push_static(
            &mut state,
            source,
            keyword_static(TargetFilter::SpecificObject { id: cards[2] }, flashback()),
        );
        push_static(
            &mut state,
            source,
            keyword_static(graveyard_cards(), Keyword::Dredge(1)).condition(
                StaticCondition::IsPresent {
                    filter: Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                        FilterProp::InAnyZone {
                            zones: vec![Zone::Graveyard],
                        },
                        FilterProp::HasKeywordKind {
                            value: KeywordKind::Flashback,
                        },
                    ]))),
                },
            ),
        );
        let outside = keywords_of_each(&state, &cards);
        assert!(
            outside[0].contains(&Keyword::Dredge(1)),
            "reach-guard: the condition holds through a nested query: {outside:?}"
        );
        reset_off_zone_keyword_scope_counts();
        let inside = with_off_zone_keyword_scope(&state, || {
            // Fill the frame first, so the measured query is a hit whose filter
            // makes further hits.
            let last = effective_off_zone_keywords(&state, cards[2]);
            let before = off_zone_keyword_scope_counts().hits;
            let first = effective_off_zone_keywords(&state, cards[0]);
            let during = off_zone_keyword_scope_counts().hits - before;
            assert!(
                during > 1,
                "reach-guard: a nested query reached the frame during the filter ({during} hits)"
            );
            let middle = effective_off_zone_keywords(&state, cards[1]);
            vec![first, middle, last]
        });
        assert_eq!(inside, outside);
        assert_eq!(off_zone_keyword_scope_depth(), 0);
    }

    /// A scoped query clones only its applicable shared effects; an unscoped query clones none.
    #[test]
    fn shared_slice_clones_only_applicable_effects() {
        let (state, _, cards) = sharing_board();
        let (slice, dependence) = collect_shared_active_continuous_effects_matching(
            &state,
            supports_off_zone_keyword_query,
        );
        assert_eq!(dependence, GatherDependence::Structural);
        let applicable = collect_applicable_off_zone_keyword_effects(&state, cards[0]).len();
        assert!(
            applicable > 0 && applicable < slice.len(),
            "reach-guard: {applicable} applicable of {} shared",
            slice.len()
        );

        reset_off_zone_keyword_scope_counts();
        effective_off_zone_keywords(&state, cards[0]);
        assert_eq!(
            off_zone_keyword_scope_counts().shared_clones,
            0,
            "outside a scope every effect is moved"
        );

        reset_off_zone_keyword_scope_counts();
        with_off_zone_keyword_scope(&state, || {
            effective_off_zone_keywords(&state, cards[0]);
            let fill = off_zone_keyword_scope_counts();
            assert_eq!((fill.fills, fill.hits), (1, 0), "{fill:?}");
            assert_eq!(fill.shared_clones, applicable, "fill query: {fill:?}");
            effective_off_zone_keywords(&state, cards[0]);
            let hit = off_zone_keyword_scope_counts();
            assert_eq!((hit.fills, hit.hits), (1, 1), "{hit:?}");
            assert_eq!(hit.shared_clones, 2 * applicable, "hit query: {hit:?}");
        });
    }

    /// Emblem, face-down, phased-out and self-grant boards agree inside and outside a scope.
    #[test]
    fn hostile_fixtures_agree_inside_and_outside_a_scope() {
        /// Compares every off-battlefield object's keywords inside one scope
        /// with the same queries outside any scope.
        fn assert_scope_is_transparent(label: &str, state: &GameState) -> Vec<Vec<Keyword>> {
            let ids = off_battlefield_ids(state);
            let outside = keywords_of_each(state, &ids);
            let (inside, _) = scoped_queries(state, &ids);
            assert_eq!(inside, outside, "{label}");
            assert_eq!(off_zone_keyword_scope_depth(), 0, "{label}");
            outside
        }
        let granted = |keywords: &[Vec<Keyword>]| keywords.iter().flatten().count();

        // An emblem in the command zone.
        let mut state = GameState::new_two_player(42);
        let emblem = create_card(&mut state, PlayerId(0), "Emblem", Zone::Command);
        let exiled = create_card(&mut state, PlayerId(0), "Think Twice", Zone::Exile);
        state.objects.get_mut(&emblem).unwrap().is_emblem = true;
        push_static(
            &mut state,
            emblem,
            keyword_static(TargetFilter::SpecificObject { id: exiled }, flashback()),
        );
        let keywords = assert_scope_is_transparent("emblem", &state);
        assert_eq!(granted(&keywords), 1, "reach-guard: the emblem grants");

        // A face-down exiled card.
        state.objects.get_mut(&exiled).unwrap().face_down = true;
        assert_scope_is_transparent("face-down exiled card", &state);

        // A phased-out static source.
        let (mut state, source, _) = sharing_board();
        assert_eq!(
            granted(&assert_scope_is_transparent("phased-in source", &state)),
            3
        );
        state.objects.get_mut(&source).unwrap().phase_status =
            crate::game::game_object::PhaseStatus::PhasedOut {
                cause: crate::game::game_object::PhaseOutCause::Directly,
            };
        assert_scope_is_transparent("phased-out source", &state);

        // An object whose own static grants itself a keyword, beside a static
        // that functions from the graveyard.
        let (mut state, _, cards) = sharing_board();
        Arc::make_mut(
            &mut state
                .objects
                .get_mut(&cards[0])
                .unwrap()
                .base_static_definitions,
        )
        .push(keyword_static(TargetFilter::SelfRef, Keyword::Dredge(9)));
        push_static(
            &mut state,
            cards[1],
            keyword_static(
                TargetFilter::SpecificObject { id: cards[2] },
                Keyword::Dredge(8),
            )
            .active_zones(vec![Zone::Graveyard]),
        );
        let ids = off_battlefield_ids(&state);
        let keywords = assert_scope_is_transparent("self grant and graveyard static", &state);
        let of = |id: ObjectId| &keywords[ids.iter().position(|other| *other == id).unwrap()];
        assert!(of(cards[0]).contains(&Keyword::Dredge(9)), "{keywords:?}");
        assert!(of(cards[2]).contains(&Keyword::Dredge(8)), "{keywords:?}");

        // An empty graveyard: the wrapper is a no-op.
        let state = GameState::new_two_player(42);
        reset_off_zone_keyword_scope_counts();
        with_off_zone_keyword_scope(&state, || ());
        assert_eq!(off_zone_keyword_scope_counts().matched, 0);
        assert_eq!(off_zone_keyword_scope_depth(), 0);
    }
}
