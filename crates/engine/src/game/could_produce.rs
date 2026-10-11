//! CR 106.7: the single authority for what mana a permanent "could produce".
//!
//! CR 106.7: "The type of mana a permanent could produce at any time includes any
//! type of mana that an ability of that permanent would produce if the ability
//! were to resolve at that time, taking into account any applicable replacement
//! effects in any possible order. Ignore whether any costs of the ability could
//! or could not be paid. If that permanent wouldn't produce any mana under these
//! conditions, or no type of mana can be defined this way, there's no type of
//! mana it could produce."
//!
//! Every reader of that phrase goes through here: the cost referent of
//! Squandered Resources ("the sacrificed land could produce"), the Reflecting
//! Pool class ("a land you control could produce", mana *types*) and the Exotic
//! Orchard class ("a land an opponent controls could produce", mana *colors* —
//! per the Orchard / Fellwar Stone rulings, never colorless). The module owns
//! four decisions:
//!
//! - **Which mana instructions would run** ([`hypothetical_productions`]). Every
//!   ability in the permanent's layered `abilities` is read — tap or not, mana
//!   ability or not — and its triggered abilities (CR 113.3 + CR 113.3c: "an
//!   ability" includes them). CR 106.7 ignores costs, and the 2025-07-25
//!   Reflecting Pool ruling adds that it "doesn't check their costs or
//!   legality", so no activation gate is consulted. The layered list already
//!   carries a basic land type's intrinsic ability (CR 305.6, applied in layer 4,
//!   CR 613.1d) and already lacks one an ability-removing effect took away
//!   (layer 6, CR 613.1f), so it is read as is. Each root is built by the builder
//!   the runtime uses for that path, and its chain is walked with the runtime's
//!   own gate authorities (`effects::evaluate_condition`, the "instead" swap, the
//!   false-gate escape). A gate or count the ability's own activation or
//!   resolution would fix (a target, X, the cost referent, its trigger event) is
//!   classified by `triggers::gate_binding_diverges` /
//!   `quantity_expr_binding_diverges` under
//!   [`BindingReader::HypotheticalResolution`] and read both ways — except what
//!   the hypothetical resolution itself fixes from state before the read
//!   ([`HypotheticalBindings`]): the ability's per-turn use ledgers, projected
//!   through the resolution's own entry bookkeeping
//!   ([`resolution_entry_state`]); the objects an earlier instruction looks at or
//!   reveals and hands on ([`bind_child`]), but only while every seat may see
//!   them; and the "if you do" outcome of a "you may" instruction whose own gate
//!   was false. Those are evaluated exactly as the resolution evaluates them.
//! - **How much** (CR 106.5 + CR 106.7's last sentence): an instruction that
//!   would add no mana contributes nothing (Gaea's Cradle with no creatures).
//! - **Replacements** (CR 614.1a + CR 616.1): each type is mapped through
//!   `replacement::hypothetical_produced_mana_types`, in every order, with
//!   CR 106.12's "tapped for mana" only for the {T} mana ability's own root
//!   instruction. One scan per solve (`replacement::produced_mana_may_be_replaced`)
//!   skips that lookup when no replacement in the game could apply.
//! - **Referential clauses**: a permanent whose ability is itself a
//!   could-produce clause reads its population's answers. The populations form a
//!   graph that may be cyclic (two Reflecting Pools; an Orchard on each side), so
//!   the answer is the least fixed point over the query's dependency closure —
//!   exactly the Orchard ruling's "won't help each other unless some other land
//!   allows one of them to actually produce some type of mana". Each permanent is
//!   read with its own controller and id (CR 109.5), so anchors survive chains
//!   that cross controllers.
//!
//! The answer is a pure function of `&GameState`; nothing is cached across
//! calls.

use std::borrow::Cow;
use std::cell::LazyCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;

use super::ability_utils::{apply_instead_swap, build_resolved_from_def};
use super::derived::continuously_revealed_cards;
use super::effects::mana::{distinct_colors_among_permanents, object_colors_for_scope};
use super::effects::{
    condition_depends_on_result_object, count_top_level_resolution,
    effect_writes_last_revealed_ids, evaluate_condition, false_gate_escape,
    inherited_parent_occurrences, inject_last_revealed_targets, instead_declined_continuation,
    instead_swap_applies, is_instead_override, published_revealed_ids, receives_last_revealed,
    should_propagate_parent_targets, InsteadDeclined,
};
use super::filter::{matches_target_filter, FilterContext};
use super::game_object::GameObject;
use super::ledger::record_ability_activation;
use super::mana_abilities::{
    apply_condition_instead_mana_swap, condition_instead_mana_branch, is_mana_ability,
    sacrifice_cost_choice,
};
use super::mana_sources::{has_tap_component, mana_options_from_production};
use super::replacement::{hypothetical_produced_mana_types, produced_mana_may_be_replaced};
use super::triggers::{
    build_triggered_ability_from_context, check_trigger_condition_with_source,
    filter_binding_diverges, gate_binding_diverges, quantity_expr_binding_diverges,
    trigger_source_context_for_latch, BindingReader, HypotheticalBindings, HypotheticalLedger,
    HypotheticalOptionalOutcome, HypotheticalTargets,
};
use super::visibility::revealed_to_every_seat;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, AbilityUseTally, Effect, ManaProduction,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TriggerDefinitionRef,
};
use crate::types::ability_visit::visit_own_resolution_effects;
use crate::types::card_type::CoreType;
use crate::types::events::ManaTapState;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaType, ManaTypeSet};
use crate::types::player::PlayerId;

/// CR 106.1b vs CR 105.1: whether a clause asks for mana *types* (Reflecting
/// Pool, Squandered Resources — colorless included) or mana *colors* (Exotic
/// Orchard, Fellwar Stone — colorless excluded, per their rulings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CouldProduceMeasure {
    Types,
    Colors,
}

/// Which permanents a could-produce clause surveys. Resolved to concrete ids
/// once per solve: membership never depends on what anything could produce, so
/// it is fixed during the fixed-point iteration.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CouldProducePopulation<'a> {
    /// "a land you control" etc. — `land_filter` evaluated with a
    /// `FilterContext` anchored to the clause's controller and source
    /// (CR 109.5), over the battlefield.
    Matching {
        land_filter: &'a TargetFilter,
        controller: PlayerId,
        source: ObjectId,
    },
    /// "a land an opponent controls" — lands on the battlefield controlled by an
    /// opponent of `controller`.
    OpponentLands { controller: PlayerId },
    /// CR 608.2k + CR 602.2b + CR 601.2h: a not-yet-paid cost referent ("the
    /// sacrificed land") ranges over the cost's legal choices —
    /// `mana_abilities::sacrifice_cost_choice`, the same candidate authority the
    /// activation surfaces.
    CostCandidates {
        controller: PlayerId,
        source: ObjectId,
        ability: &'a AbilityDefinition,
    },
}

/// How one would-be mana instruction is read.
enum ProductionReading {
    /// An option set independent of other permanents, already read for the
    /// hypothetical resolving node that carries it ([`production_reading`]).
    Direct(Vec<ManaType>),
    /// An option set that IS a could-produce census (`AnyTypeProduceableBy`,
    /// `OpponentLandColors`), over a population fixed when it was read.
    Referential {
        population: Vec<ObjectId>,
        measure: CouldProduceMeasure,
    },
}

/// One mana instruction a permanent's ability would carry out if it resolved
/// now (CR 106.7).
struct HypotheticalProduction {
    reading: ProductionReading,
    /// CR 106.12: `FromTap` only for the root mana instruction of an activated
    /// mana ability whose cost includes {T} — exactly when
    /// `produce_mana_from_ability` marks production tapped. Chain / inline-branch
    /// mana and every stack-resolved ability produce with `NotFromTap`.
    tap_state: ManaTapState,
    /// CR 106.5: the amount the instruction would add, already known to be
    /// positive — the resolved count, or 1 for a count the ability's own
    /// activation or resolution fixes ([`instruction_amount`]).
    amount: u32,
    /// CR 109.5: the controller of the hypothetical resolving node — the player
    /// who would receive the mana, and the one replacements see.
    controller: PlayerId,
}

/// CR 106.7: the mana types `object_id` could produce right now, in canonical
/// order. Empty for an id with no object.
pub(crate) fn could_produce(state: &GameState, object_id: ObjectId) -> Vec<ManaType> {
    let solution = solve(state, &[object_id]);
    solution.set_of(object_id).iter().collect()
}

/// CR 106.7: what the permanents of `population` could produce, projected by
/// `measure`, in canonical order.
pub(crate) fn census(
    state: &GameState,
    population: CouldProducePopulation<'_>,
    measure: CouldProduceMeasure,
) -> Vec<ManaType> {
    let members = population_members(state, population);
    let solution = solve(state, &members);
    let union = members.iter().fold(ManaTypeSet::EMPTY, |union, member| {
        union.union(solution.set_of(*member))
    });
    project(union, measure).iter().collect()
}

/// CR 106.7 + CR 608.2k + CR 608.2h: the option set of an "any type that
/// [`land_filter`] could produce" clause of `source`, controlled by `controller`.
///
/// The cost-referent form ("the sacrificed land") names no population: its
/// answer is the referent's could-produce set as it last existed, captured into
/// the resolving `ability`'s cost-paid snapshot while the land was still a
/// permanent (an LKI read, deliberately not incarnation-gated — it mirrors the
/// `ObjectScope::CostPaidObject` LKI readers in `game/quantity.rs`). With no
/// resolving ability (source enumeration, display) there is no referent yet, so
/// no option set (CR 106.5). Every other filter is a census of the matching
/// lands.
pub(crate) fn types_for_clause(
    state: &GameState,
    land_filter: &TargetFilter,
    controller: PlayerId,
    source: ObjectId,
    ability: Option<&ResolvedAbility>,
) -> Vec<ManaType> {
    match land_filter {
        TargetFilter::CostPaidObject => ability
            .and_then(|ability| ability.cost_paid_object.as_ref())
            .map(|snapshot| snapshot.lki.produceable_mana_types.clone())
            .unwrap_or_default(),
        _ => census(
            state,
            CouldProducePopulation::Matching {
                land_filter,
                controller,
                source,
            },
            CouldProduceMeasure::Types,
        ),
    }
}

/// CR 105.1 + the Orchard / Fellwar Stone ruling: a *colors* clause never
/// offers colorless, even when a surveyed land could produce it.
fn project(types: ManaTypeSet, measure: CouldProduceMeasure) -> ManaTypeSet {
    match measure {
        CouldProduceMeasure::Types => types,
        CouldProduceMeasure::Colors => types.colors(),
    }
}

/// The battlefield permanents `population` surveys, in battlefield order.
fn population_members(state: &GameState, population: CouldProducePopulation<'_>) -> Vec<ObjectId> {
    match population {
        CouldProducePopulation::Matching {
            land_filter,
            controller,
            source,
        } => {
            // CR 109.4: `ControllerRef::You` resolves against the clause's
            // controller even when its source has left play (a self-sacrifice
            // cost) or in a synthetic context.
            let filter_context = FilterContext::from_source_with_controller(source, controller);
            // CR 730.2: the independent-permanent list, so an absorbed merge
            // component is never surveyed separately.
            state
                .battlefield
                .iter()
                .copied()
                .filter(|object_id| {
                    let is_land = state.objects.get(object_id).is_some_and(|object| {
                        object.card_types.core_types.contains(&CoreType::Land)
                    });
                    is_land
                        && matches_target_filter(state, *object_id, land_filter, &filter_context)
                })
                .collect()
        }
        CouldProducePopulation::OpponentLands { controller } => {
            let opponents = super::players::opponents(state, controller);
            // CR 702.26b + CR 702.26d: a phased-out land stays on the battlefield
            // under its controller but is treated as though it doesn't exist, so
            // it anchors nothing. The `Matching` arm gets this from
            // `matches_target_filter`; this arm reads the phased-in list.
            state
                .battlefield_phased_in_ids()
                .into_iter()
                .filter(|object_id| {
                    state.objects.get(object_id).is_some_and(|object| {
                        opponents.contains(&object.controller)
                            && object.card_types.core_types.contains(&CoreType::Land)
                    })
                })
                .collect()
        }
        CouldProducePopulation::CostCandidates {
            controller,
            source,
            ability,
        } => sacrifice_cost_choice(state, controller, source, ability)
            .map(|(_, candidates)| candidates)
            .unwrap_or_default(),
    }
}

/// The least fixed point of the could-produce graph rooted at some permanents.
struct Solution {
    sets: Vec<ManaTypeSet>,
    index: HashMap<ObjectId, usize>,
}

impl Solution {
    fn set_of(&self, object_id: ObjectId) -> ManaTypeSet {
        self.index
            .get(&object_id)
            .map_or(ManaTypeSet::EMPTY, |position| self.sets[*position])
    }
}

/// One surveyed permanent of a solve: what it could produce on its own, and the
/// referential reads that add to it.
struct SolveNode {
    object_id: ObjectId,
    seed: ManaTypeSet,
    referential: Vec<ReferentialRead>,
}

/// A could-produce clause of a surveyed permanent, waiting on its population.
struct ReferentialRead {
    population: Vec<ObjectId>,
    measure: CouldProduceMeasure,
    tap_state: ManaTapState,
    amount: u32,
    controller: PlayerId,
}

/// CR 106.7: the least fixed point over the dependency closure of `roots`.
///
/// 1. Discover every permanent the roots' referential clauses reach, reading
///    each one's productions once.
/// 2. Seed each with its direct productions, mapped through the applicable
///    replacements.
/// 3. Iterate (Kleene): each referential clause adds the replacement-mapped
///    union of its population's current sets, projected by its measure, until a
///    pass changes nothing.
///
/// A least fixed point rather than a depth cap or an on-stack cycle cut: the
/// per-node map includes replacements, so revisiting a node can matter, and the
/// least fixed point is exactly "help each other only if some land actually
/// anchors a type" — an unanchored cycle stays empty. The lattice is six bits per
/// node and every pass only grows a set, so it terminates within
/// `6 · |closure| + 1` passes.
fn solve(state: &GameState, roots: &[ObjectId]) -> Solution {
    // CR 106.7 + CR 614.1a: one scan per solve decides whether any replacement
    // could rewrite a production. On the common board none can, and each
    // production then yields its own type — every amount read here is positive
    // (`instruction_amount`), so that is exactly what the per-production lookup
    // would answer, without its whole-game candidate scan per land and type.
    let replacements_may_apply = produced_mana_may_be_replaced(state);
    let produced_types = |source: ObjectId,
                          controller: PlayerId,
                          mana_type: ManaType,
                          amount: u32,
                          tap_state: ManaTapState| {
        if !replacements_may_apply {
            return ManaTypeSet::of(mana_type);
        }
        hypothetical_produced_mana_types(state, source, controller, mana_type, amount, tap_state)
    };

    let mut nodes: Vec<SolveNode> = Vec::new();
    let mut index: HashMap<ObjectId, usize> = HashMap::new();
    let mut worklist: VecDeque<ObjectId> = roots.iter().copied().collect();
    while let Some(object_id) = worklist.pop_front() {
        if index.contains_key(&object_id) {
            continue;
        }
        let mut node = SolveNode {
            object_id,
            seed: ManaTypeSet::EMPTY,
            referential: Vec::new(),
        };
        for production in hypothetical_productions(state, object_id) {
            match production.reading {
                // The replacement lookup reads this outer `state`, never a walk
                // frame's scratch copy: a scratch differs only in the use ledgers
                // and `last_revealed_ids`, which no replacement reads.
                ProductionReading::Direct(options) => {
                    for mana_type in options {
                        node.seed = node.seed.union(produced_types(
                            object_id,
                            production.controller,
                            mana_type,
                            production.amount,
                            production.tap_state,
                        ));
                    }
                }
                ProductionReading::Referential {
                    population,
                    measure,
                } => {
                    worklist.extend(population.iter().copied());
                    node.referential.push(ReferentialRead {
                        population,
                        measure,
                        tap_state: production.tap_state,
                        amount: production.amount,
                        controller: production.controller,
                    });
                }
            }
        }
        index.insert(object_id, nodes.len());
        nodes.push(node);
    }

    let mut sets: Vec<ManaTypeSet> = nodes.iter().map(|node| node.seed).collect();
    loop {
        let mut changed = false;
        for (position, node) in nodes.iter().enumerate() {
            let mut next = sets[position];
            for read in &node.referential {
                let union = read
                    .population
                    .iter()
                    .filter_map(|member| index.get(member))
                    .fold(ManaTypeSet::EMPTY, |union, member| {
                        union.union(sets[*member])
                    });
                for mana_type in project(union, read.measure).iter() {
                    next = next.union(produced_types(
                        node.object_id,
                        read.controller,
                        mana_type,
                        read.amount,
                        read.tap_state,
                    ));
                }
            }
            if next != sets[position] {
                sets[position] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Solution { sets, index }
}

/// CR 106.7: every mana instruction an ability of `object_id` would carry out if
/// it resolved now.
fn hypothetical_productions(state: &GameState, object_id: ObjectId) -> Vec<HypotheticalProduction> {
    let Some(object) = state.objects.get(&object_id) else {
        return Vec::new();
    };
    let mut productions = Vec::new();
    for (ability_index, definition) in object.abilities.iter().enumerate() {
        if definition.kind != AbilityKind::Activated {
            continue;
        }
        // CR 605.1a selects the resolution path only — a non-mana ability's
        // production counts too (CR 106.7 ignores costs).
        let resolves_as_mana_ability = is_mana_ability(definition);
        // CR 700.2a: modes are chosen on activation, so each mode is a way the
        // ability could resolve; the union is the CR 106.7 answer.
        let roots = std::iter::once(definition).chain(definition.mode_abilities.iter());
        for root in roots {
            if resolves_as_mana_ability {
                read_mana_ability_root(state, object, definition, root, &mut productions);
            } else {
                // The runtime activation: build, then stamp the printed index,
                // which every instruction of the chain shares (CR 608.2c). X
                // stays unbound — an activation choice.
                let mut resolved = build_resolved_from_def(root, object.id, object.controller);
                resolved.ability_index = Some(ability_index);
                resolved.inherit_ability_index_recursive(Some(ability_index));
                let (entry_state, ledger) = resolution_entry_state(
                    state,
                    &resolved,
                    definition,
                    HypotheticalEntry::Activation,
                );
                let frame = Frame::root(&entry_state, ledger, definition);
                walk_chain(&frame, &resolved, &mut productions);
            }
        }
    }
    read_triggered_abilities(state, object, &mut productions);
    productions
}

/// How the hypothetical resolution of a root reaches resolution.
#[derive(Debug, Clone, Copy)]
enum HypotheticalEntry {
    /// CR 602.2a: an activated ability is announced (and counted) on its way
    /// to resolving.
    Activation,
    /// CR 603.3: a triggered ability is put on the stack without being
    /// activated.
    Trigger,
}

/// CR 106.7 + CR 608.2c: the state the hypothetical resolution of `root` reads,
/// and whether its use ledgers were projected.
///
/// The projection is `state` with the runtime's own resolution-entry
/// bookkeeping applied once on a scratch copy: the activation ledger
/// (`ledger::record_ability_activation`, CR 602.2a — an activated ability that
/// resolves was announced, and that announcement is counted) and the resolution
/// ledger (`effects::count_top_level_resolution`, bumped before the first
/// instruction runs). So "if this is the third time this ability has resolved
/// this turn" reads exactly the count the next resolution would read.
///
/// `state` is borrowed with [`HypotheticalLedger::Live`] when `definition`
/// reads no use ledger or `root` carries no printed index (a mana ability's
/// root, which the runtime builds index-less): the gate is then read both ways.
fn resolution_entry_state<'a>(
    state: &'a GameState,
    root: &ResolvedAbility,
    definition: &AbilityDefinition,
    entry: HypotheticalEntry,
) -> (Cow<'a, GameState>, HypotheticalLedger) {
    let reads_ledger = definition.reads_ability_use_count(AbilityUseTally::Resolved)
        || definition.reads_ability_use_count(AbilityUseTally::Activated);
    let Some(ability_index) = root.ability_index.filter(|_| reads_ledger) else {
        return (Cow::Borrowed(state), HypotheticalLedger::Live);
    };
    let mut scratch = state.clone();
    if let HypotheticalEntry::Activation = entry {
        let recorded = record_ability_activation(&mut scratch, root.source_id, ability_index, None);
        // A replay-invariant mismatch cannot arise on a fresh copy of a valid
        // state. Should one ever appear, fail closed to the both-ways reading
        // rather than evaluate against a ledger the projection did not write.
        if let Err(error) = recorded {
            debug_assert!(false, "projecting a fresh activation failed: {error:?}");
            tracing::debug!(
                ?error,
                source = ?root.source_id,
                ability_index,
                "could_produce: activation projection failed; reading the ledger both ways"
            );
            return (Cow::Borrowed(state), HypotheticalLedger::Live);
        }
    }
    count_top_level_resolution(&mut scratch, root);
    (Cow::Owned(scratch), HypotheticalLedger::Projected)
}

/// CR 605.3b: an activated mana ability resolves immediately, through
/// `produce_mana_from_ability`, which builds its root with
/// `build_resolved_from_def` and decides its `ConditionInstead` sub with
/// `apply_condition_instead_mana_swap` — mirrored here step for step. The root
/// does not read its own `condition`, as `produce_mana_from_ability` does not.
fn read_mana_ability_root(
    state: &GameState,
    object: &GameObject,
    definition: &AbilityDefinition,
    root: &AbilityDefinition,
    productions: &mut Vec<HypotheticalProduction>,
) {
    // The activation's own stamps (`cost_paid_object`, `chosen_x`) record
    // activation choices, which a CR 106.7 reading never has.
    let unswapped = build_resolved_from_def(root, object.id, object.controller);
    // CR 602.2a + CR 605.3b: mana abilities are activated and resolve, but the
    // runtime builds this root index-less, so the projection reads `Live`.
    let (entry_state, ledger) =
        resolution_entry_state(state, &unswapped, definition, HypotheticalEntry::Activation);
    let frame = Frame::root(&entry_state, ledger, definition);
    let branches = match unswapped.sub_ability.as_deref() {
        // CR 608.2c + CR 106.7: a wrapped condition the resolution itself fixes
        // could go either way, so both branches are read.
        Some(sub)
            if matches!(
                sub.condition.as_ref(),
                Some(AbilityCondition::ConditionInstead { inner })
                    if gate_binding_diverges(inner, frame.reader())
            ) =>
        {
            vec![
                condition_instead_mana_branch(&unswapped, sub, true),
                condition_instead_mana_branch(&unswapped, sub, false),
            ]
        }
        _ => vec![apply_condition_instead_mana_swap(frame.state, &unswapped)],
    };
    // CR 106.12: the root instruction of a {T} mana ability is "tapped for mana".
    let root_tap_state = ManaTapState::from_tap(has_tap_component(&definition.cost));
    for branch in &branches {
        let _ = visit_own_resolution_effects(&branch.effect, &mut |effect| {
            // The root instruction is the node that IS `branch.effect`, the one
            // `produce_mana_from_ability` matches as `Effect::Mana { target: None }`;
            // anything reached beneath it produces through `effects::mana`.
            let is_root_instruction = std::ptr::eq(effect, &branch.effect)
                && matches!(effect, Effect::Mana { target: None, .. });
            let tap_state = if is_root_instruction {
                root_tap_state
            } else {
                ManaTapState::NotFromTap
            };
            push_production(&frame, branch, effect, tap_state, productions);
            ControlFlow::Continue(())
        });
        // CR 605.3b: the rest of the chain resolves through
        // `resolve_mana_ability_sub_chain` → `effects::resolve_ability_chain`.
        if let Some(sub) = branch.sub_ability.as_deref() {
            walk_chain(&frame, sub, productions);
        }
    }
}

/// CR 113.3 + CR 113.3c + CR 603.4: the object's triggered abilities, each built
/// exactly as the trigger collection in `stack.rs` builds it, and read only when
/// its intervening "if" holds now. Leaves that read the trigger event are
/// answered with no event of their own. Payloads registered for later (a granted
/// ability, a delayed trigger) are not this ability's own resolution and are not
/// read (Urza's Saga's chapter I grants its mana ability; it adds none itself).
fn read_triggered_abilities(
    state: &GameState,
    object: &GameObject,
    productions: &mut Vec<HypotheticalProduction>,
) {
    // The latch context is a full source snapshot; most surveyed lands have no
    // triggered ability to read, so skip building it for them.
    if object.trigger_definitions.is_empty() {
        return;
    }
    let context = trigger_source_context_for_latch(state, object);
    for (trigger_index, entry) in context.trigger_entries.iter().enumerate() {
        let Some(execute) = entry.definition.execute.as_deref() else {
            continue;
        };
        let definition_ref = TriggerDefinitionRef {
            source: context.identity.reference,
            occurrence: entry.occurrence.clone(),
        };
        if let Some(condition) = &entry.definition.condition {
            if !check_trigger_condition_with_source(
                state,
                condition,
                context.lki.controller,
                Some(&context),
                Some(&definition_ref),
                None,
            ) {
                continue;
            }
        }
        let mut resolved = build_triggered_ability_from_context(
            state,
            &entry.definition,
            &context,
            Some(&definition_ref),
        );
        resolved.ability_index = Some(trigger_index);
        resolved.inherit_ability_index_recursive(Some(trigger_index));
        let (entry_state, ledger) =
            resolution_entry_state(state, &resolved, execute, HypotheticalEntry::Trigger);
        let frame = Frame::root(&entry_state, ledger, execute);
        walk_chain(&frame, &resolved, productions);
    }
}

/// The inputs every step of one hypothetical resolution reads.
#[derive(Clone, Copy)]
struct Frame<'a> {
    /// The state the node reads: the query's state, or a scratch copy carrying
    /// what the resolution has published by then (its projected use ledgers,
    /// the objects an earlier instruction looked at).
    state: &'a GameState,
    /// What the hypothetical resolution has established for the node.
    bindings: HypotheticalBindings,
    /// The definition whose cost a cost-referent production ranges over: the
    /// activated (or mana) ability's own definition, or a triggered ability's
    /// execute definition.
    cost_owner: &'a AbilityDefinition,
}

impl<'a> Frame<'a> {
    /// A root, which has bound nothing yet but its projected ledger.
    fn root(
        state: &'a GameState,
        ledger: HypotheticalLedger,
        cost_owner: &'a AbilityDefinition,
    ) -> Self {
        Frame {
            state,
            bindings: HypotheticalBindings {
                ledger,
                ..HypotheticalBindings::NONE
            },
            cost_owner,
        }
    }

    fn reader(&self) -> BindingReader {
        BindingReader::HypotheticalResolution(self.bindings)
    }
}

/// CR 608.2c: whether a node's own effect ran before it handed off to a child.
#[derive(Debug, Clone, Copy)]
enum ParentOutcome {
    /// `walk_performed`'s sub link and an "instead" decline continuation: the
    /// effect ran, so it published whatever its handler publishes, and the
    /// child is dispatched as the runtime dispatches after the gate block.
    Performed,
    /// `walk_false_gate_path`'s else branch and escaped sub: the effect never
    /// ran and published nothing; the runtime's own-top false path hands on
    /// only `should_propagate_parent_targets` targets.
    Skipped,
}

/// A child node as the runtime would hand it off, with what it carries.
struct BoundChild<'s, 'n> {
    /// The state the child's subtree reads: its parent's, or a scratch copy
    /// with `last_revealed_ids` as the parent's handler would have left it.
    state: Cow<'s, GameState>,
    node: Cow<'n, ResolvedAbility>,
    bindings: HypotheticalBindings,
}

impl BoundChild<'_, '_> {
    fn walk(&self, cost_owner: &AbilityDefinition, productions: &mut Vec<HypotheticalProduction>) {
        let frame = Frame {
            state: &self.state,
            bindings: self.bindings,
            cost_owner,
        };
        walk_chain(&frame, &self.node, productions);
    }
}

/// What a hand-off gives a child: the state its subtree reads and, when the
/// child is bound, the objects handed to it.
type HandOff<'s> = (Cow<'s, GameState>, Option<Vec<TargetRef>>);

/// CR 608.2c: `child` as the runtime would hand it off from `parent`, with the
/// bindings it carries.
///
/// Targets are bound exactly where the runtime hand-off is fixed by state and
/// public to every seat ([`performed_hand_off`], [`skipped_hand_off`]); the
/// ledger binding is inherited unchanged; and the "if you do" outcome becomes
/// `NotPerformed` below a "you may" instruction whose own gate was false
/// (CR 608.2c), and `EitherWay` again below one that was performed (CR 608.2d:
/// the player chooses).
fn bind_child<'s, 'n>(
    frame: &Frame<'s>,
    parent: &ResolvedAbility,
    parent_outcome: ParentOutcome,
    child: &'n ResolvedAbility,
) -> BoundChild<'s, 'n> {
    let optional_outcome = match (parent_outcome, parent.optional) {
        (ParentOutcome::Skipped, true) => HypotheticalOptionalOutcome::NotPerformed,
        (ParentOutcome::Performed, true) => HypotheticalOptionalOutcome::EitherWay,
        (_, false) => frame.bindings.optional_outcome,
    };
    let (state, handed) = match parent_outcome {
        ParentOutcome::Performed => performed_hand_off(frame, parent, child),
        ParentOutcome::Skipped => skipped_hand_off(frame, parent, child),
    };
    let mut node = Cow::Borrowed(child);
    let targets = match handed {
        Some(handed) => {
            node.to_mut().set_unpinned_targets(handed);
            HypotheticalTargets::EstablishedByEarlierInstruction
        }
        None => HypotheticalTargets::Unchosen,
    };
    if let (ParentOutcome::Skipped, true) = (parent_outcome, parent.optional) {
        // CR 608.2c: the skipped "you may" was not performed, so its "if you
        // do" reads false and its "if you don't" reads true below it.
        node.to_mut().set_optional_effect_performed_recursive(false);
    }
    BoundChild {
        state,
        node,
        bindings: HypotheticalBindings {
            targets,
            optional_outcome,
            ledger: frame.bindings.ledger,
        },
    }
}

/// CR 608.2c + CR 701.20e: the hand-off after `parent`'s effect ran, mirroring
/// `effects::resolve_chain_body`'s post-gate dispatch.
///
/// 1. The look/reveal hand-off (`effects::receives_last_revealed`), taken
///    first, as the runtime takes it first: the child receives the objects the
///    parent published (`effects::published_revealed_ids`).
/// 2. Otherwise, target inheritance (`effects::inherited_parent_occurrences`) from
///    a parent whose own objects an earlier instruction established.
///
/// A gated child is bound only when its gate reads the same objects twice at
/// runtime ([`gate_pre_read_targets`]), and a publication only while every
/// seat may see it ([`identity_is_public_now`]). Every other case — a parent
/// that publishes a set state does not fix, a child whose subtree adds no
/// mana — leaves the child unbound, today's reading.
fn performed_hand_off<'s>(
    frame: &Frame<'s>,
    parent: &ResolvedAbility,
    child: &ResolvedAbility,
) -> HandOff<'s> {
    let unbound = || (Cow::Borrowed(frame.state), None);
    // A subtree that adds no mana cannot change the answer, so it is walked
    // unbound without paying for a scratch state.
    if !child.targets.is_empty() || !chain_carries_mana(child) {
        return unbound();
    }
    let Some(published) = published_revealed_ids(frame.state, parent, frame.reader()) else {
        // A parent that writes `last_revealed_ids` with a set state does not fix
        // (a choice-keeping Dig, `RevealUntil`, `Clash`, an unbound `Reveal`, an
        // empty library) leaves the runtime's hand-off unknown.
        if effect_writes_last_revealed_ids(&parent.effect) {
            return unbound();
        }
        return match inherited_targets(frame, parent, child, None) {
            Some(inherited) => (Cow::Borrowed(frame.state), Some(inherited)),
            None => unbound(),
        };
    };

    // Read at most once, and only by a decision that needs it: the scan of
    // the continuous reveal statics is not free.
    let public = LazyCell::new(|| {
        let continuous = continuously_revealed_cards(frame.state);
        published
            .iter()
            .all(|id| identity_is_public_now(frame.state, &continuous, *id))
    });
    // The two arms below need the parent's after-state, so decide first
    // whether either can bind. A publication some seat may not see binds
    // nothing through the look hand-off, and a parent holding no objects an
    // earlier instruction established hands nothing on by inheritance (the
    // precondition `inherited_targets` checks first).
    let may_inherit = frame.bindings.targets
        == HypotheticalTargets::EstablishedByEarlierInstruction
        && !parent.targets.is_empty();
    if !may_inherit && !*public {
        return unbound();
    }

    // The state the parent's handler leaves behind: the field the hand-off
    // predicates read.
    let mut scratch = frame.state.clone();
    scratch.last_revealed_ids = published.clone();

    if receives_last_revealed(&scratch, parent, child) {
        let handed = inject_last_revealed_targets(&scratch, parent, child);
        let gate_reads_handed = child.condition.as_ref().is_none_or(|condition| {
            gate_pre_read_targets(&scratch, parent, child, condition) == handed
        });
        if handed.is_empty() || !*public || !gate_reads_handed {
            return unbound();
        }
        return (Cow::Owned(scratch), Some(handed));
    }

    match inherited_targets(frame, parent, child, Some(&scratch)) {
        Some(inherited) => (Cow::Owned(scratch), Some(inherited)),
        None => unbound(),
    }
}

/// CR 608.2c: the targets `child` inherits from `parent` after the parent's
/// effect ran, when the parent's own objects were established by an earlier
/// instruction (`effects::inherited_parent_occurrences`, the runtime's rule).
/// `after_parent` is the state the parent's handler leaves when it published
/// to `last_revealed_ids`.
fn inherited_targets(
    frame: &Frame<'_>,
    parent: &ResolvedAbility,
    child: &ResolvedAbility,
    after_parent: Option<&GameState>,
) -> Option<Vec<TargetRef>> {
    if frame.bindings.targets != HypotheticalTargets::EstablishedByEarlierInstruction
        || parent.targets.is_empty()
    {
        return None;
    }
    // The walker's nodes carry no incarnation pins (the root is built from the
    // printed definition and every hand-off binds unpinned), so projecting the
    // runtime's inherited occurrences to their targets loses nothing.
    let inherited: Vec<TargetRef> = inherited_parent_occurrences(parent, child)
        .into_iter()
        .map(|(target, _)| target)
        .collect();
    if inherited.is_empty() {
        return None;
    }
    if let Some(condition) = &child.condition {
        let state = after_parent.unwrap_or(frame.state);
        if gate_pre_read_targets(state, parent, child, condition) != inherited {
            return None;
        }
    }
    Some(inherited)
}

/// CR 608.2c: the targets `effects::resolve_chain_body`'s gate block reads a
/// gated child's `condition` against before dispatching the child.
///
/// The runtime reads the gate twice: first in the gate block, against the
/// parent — or against the parent with the objects it just published injected
/// as its targets, when the parent has no targets of its own or the gate reads
/// the result object (`effects::condition_depends_on_result_object`) — and
/// then again on the child, against the targets the dispatch handed it. The
/// child performs only if both pass. The walker reads the gate once, on the
/// bound child, so it binds a gated child only when this pre-read and the
/// hand-off name the same objects in the same order (the first object target
/// is what most target readers read).
///
/// `state_after_parent` carries `last_revealed_ids` as the parent's handler
/// leaves it.
fn gate_pre_read_targets(
    state_after_parent: &GameState,
    parent: &ResolvedAbility,
    child: &ResolvedAbility,
    condition: &AbilityCondition,
) -> Vec<TargetRef> {
    let injects_published = effect_writes_last_revealed_ids(&parent.effect)
        && !state_after_parent.last_revealed_ids.is_empty()
        && (parent.targets.is_empty() || condition_depends_on_result_object(condition));
    if injects_published {
        inject_last_revealed_targets(state_after_parent, parent, child)
    } else {
        parent.targets.clone()
    }
}

/// CR 608.2c: the hand-off after `parent`'s own gate was false, mirroring
/// `effects::resolve_chain_body`'s own-top false path: the effect never ran
/// and published nothing, and the else branch or escaped sub receives the
/// parent's targets as `effects::should_propagate_parent_targets` allows.
fn skipped_hand_off<'s>(
    frame: &Frame<'s>,
    parent: &ResolvedAbility,
    child: &ResolvedAbility,
) -> HandOff<'s> {
    let established =
        frame.bindings.targets == HypotheticalTargets::EstablishedByEarlierInstruction;
    let handed = (established
        && chain_carries_mana(child)
        && should_propagate_parent_targets(parent, child))
    .then(|| parent.targets.clone());
    (Cow::Borrowed(frame.state), handed)
}

/// Whether resolving `node` could reach an `Effect::Mana` — in its own effect
/// or down its `sub_ability` / `else_ability` links, exactly the nodes the
/// walk can reach and read.
fn chain_carries_mana(node: &ResolvedAbility) -> bool {
    let adds_mana = visit_own_resolution_effects(&node.effect, &mut |effect| {
        if matches!(effect, Effect::Mana { .. }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break();
    adds_mana
        || node.sub_ability.as_deref().is_some_and(chain_carries_mana)
        || node.else_ability.as_deref().is_some_and(chain_carries_mana)
}

/// CR 400.2 + CR 401.2 + CR 401.5 + CR 701.20a: whether every seated player
/// may know `id`'s identity now.
///
/// The objects the publishing family identifies are library cards (Dig and
/// RevealTop select from a library), where only a reveal shown to every seat
/// makes identity public: a momentary reveal (`revealed_cards`), or a
/// continuous "play with the top card revealed" static (`continuous`, the
/// rules authority — the display carrier is refilled only by
/// `derived::derive_display_state`). Both go through
/// `visibility::revealed_to_every_seat`, the rule the seated projection uses,
/// so a controller-only reveal (a pending manifest dread) never counts. A
/// private look, or one seat's knowledge, is not publicity: a bound answer
/// would disclose the card through mana prompts, `available_mana_pips`,
/// castability and the AI.
fn identity_is_public_now(state: &GameState, continuous: &HashSet<ObjectId>, id: ObjectId) -> bool {
    let in_public_zone = state
        .objects
        .get(&id)
        .is_some_and(|object| object.zone.is_public() && !object.face_down);
    in_public_zone
        || revealed_to_every_seat(state, &state.revealed_cards, id)
        || revealed_to_every_seat(state, continuous, id)
}

/// CR 608.2c: the chain the resolver would run from `node`, read with the
/// resolver's own decisions.
///
/// Step 1, the "instead" swap (CR 608.2c + CR 614.1a + CR 614.15): membership
/// is `effects::is_instead_override`, the decision `effects::instead_swap_applies`
/// and the swap `ability_utils::apply_instead_swap` — the functions
/// `resolve_ability_chain` uses. A swap whose gate the resolution itself fixes
/// is read both ways.
fn walk_chain(
    frame: &Frame<'_>,
    node: &ResolvedAbility,
    productions: &mut Vec<HypotheticalProduction>,
) {
    let instead = node
        .sub_ability
        .as_deref()
        .filter(|sub| is_instead_override(sub));
    let Some(sub) = instead else {
        walk_performed(frame, node, productions);
        return;
    };
    let swap_is_resolution_bound = sub
        .condition
        .as_ref()
        .is_some_and(|condition| gate_binding_diverges(condition, frame.reader()));
    if swap_is_resolution_bound {
        walk_performed(frame, &apply_instead_swap(node, sub), productions);
        walk_performed(frame, node, productions);
    } else if instead_swap_applies(frame.state, node, sub) {
        walk_performed(frame, &apply_instead_swap(node, sub), productions);
    } else {
        walk_performed(frame, node, productions);
    }
}

/// CR 608.2c: steps 2–4 of the chain walk on a node whose "instead" decision is
/// made.
///
/// Step 2, the node's gate: a CR 603.12 reflexive marker makes the node a
/// separate triggered ability (not read — the boundary
/// `scope_prunes_nested_ability` draws); a gate the resolution itself fixes is
/// read both ways; any other gate is evaluated now by `evaluate_condition`.
/// Step 3, the node's effect: every `Effect::Mana` it performs, "you may"
/// included (declining is a choice). Step 4, the next link, handed off as the
/// runtime hands it ([`bind_child`]): a not-swapped "instead" sub continues as
/// `instead_declined_continuation` selects, anything else as itself.
fn walk_performed(
    frame: &Frame<'_>,
    node: &ResolvedAbility,
    productions: &mut Vec<HypotheticalProduction>,
) {
    if let Some(condition) = &node.condition {
        if condition.has_when_you_do_marker() {
            return;
        }
        if gate_binding_diverges(condition, frame.reader()) {
            walk_false_gate_path(frame, node, productions);
        } else if !evaluate_condition(condition, frame.state, node) {
            walk_false_gate_path(frame, node, productions);
            return;
        }
    }

    let _ = visit_own_resolution_effects(&node.effect, &mut |effect| {
        push_production(frame, node, effect, ManaTapState::NotFromTap, productions);
        ControlFlow::Continue(())
    });

    let Some(sub) = node.sub_ability.as_deref() else {
        return;
    };
    let next = if is_instead_override(sub) {
        match instead_declined_continuation(sub) {
            Some(InsteadDeclined::Else(continuation) | InsteadDeclined::Tail(continuation)) => {
                continuation
            }
            None => return,
        }
    } else {
        sub
    };
    bind_child(frame, node, ParentOutcome::Performed, next).walk(frame.cost_owner, productions);
}

/// CR 608.2c: what the resolver runs after `node`'s gate was false — its
/// `else_ability`, else a sub that `effects::false_gate_escape` lets through.
fn walk_false_gate_path(
    frame: &Frame<'_>,
    node: &ResolvedAbility,
    productions: &mut Vec<HypotheticalProduction>,
) {
    let next = node.else_ability.as_deref().or_else(|| {
        node.sub_ability
            .as_deref()
            .filter(|sub| false_gate_escape(sub).is_some())
    });
    if let Some(next) = next {
        bind_child(frame, node, ParentOutcome::Skipped, next).walk(frame.cost_owner, productions);
    }
}

/// Records `effect` when it is a mana instruction that would add mana, read for
/// `node`, the hypothetical resolving node that carries it (anchored to the
/// surveyed permanent's own controller and id, CR 109.5).
fn push_production(
    frame: &Frame<'_>,
    node: &ResolvedAbility,
    effect: &Effect,
    tap_state: ManaTapState,
    productions: &mut Vec<HypotheticalProduction>,
) {
    let Effect::Mana { produced, .. } = effect else {
        return;
    };
    let Some(amount) = instruction_amount(frame, produced, node) else {
        return;
    };
    productions.push(HypotheticalProduction {
        reading: production_reading(frame, produced, node),
        tap_state,
        amount,
        controller: node.controller,
    });
}

/// CR 106.5 + CR 106.7: how much mana an instruction would add, or `None` when
/// it would add none — such an instruction defines no type.
///
/// A count the ability's own activation or resolution fixes — X (CR 107.3a), a
/// storage land's removed counters, the cost referent, an unbound target, an
/// anaphor, an event amount (`triggers::quantity_expr_binding_diverges` under
/// the frame's reader) — is admitted as 1: CR 106.7 ignores whether costs could
/// be paid, and the choices made during the resolution are the player's to
/// make. Any other count is state at that time, resolved against `node` (Gaea's
/// Cradle counts its controller's creatures; a target an earlier instruction
/// handed over is read as handed). A production with no count carries its
/// amount in its option list.
fn instruction_amount(
    frame: &Frame<'_>,
    produced: &ManaProduction,
    node: &ResolvedAbility,
) -> Option<u32> {
    let mut amount = Some(1);
    produced.for_each_quantity_expr(&mut |count: &QuantityExpr| {
        if quantity_expr_binding_diverges(count, frame.reader()) {
            return;
        }
        let resolved = super::quantity::resolve_quantity_with_targets(frame.state, count, node);
        amount = amount.and(
            u32::try_from(resolved)
                .ok()
                .filter(|resolved| *resolved > 0),
        );
    });
    amount
}

/// How `produced` is read: a could-produce clause becomes a census of its
/// population, anchored to `node`'s controller and source; every other
/// production is read for `node` as the frame has bound it. Exhaustive, so a
/// new variant must decide what its output depends on.
fn production_reading(
    frame: &Frame<'_>,
    produced: &ManaProduction,
    node: &ResolvedAbility,
) -> ProductionReading {
    let state = frame.state;
    let enumerated = || {
        ProductionReading::Direct(mana_options_from_production(
            state,
            node.controller,
            node.source_id,
            produced,
        ))
    };
    match produced {
        // CR 608.2k + CR 106.7: an unbound cost referent ranges over the legal
        // choices of `cost_owner`'s cost. For a triggered ability that is its
        // execute definition's own cost, so the population is empty unless that
        // definition carries a sacrifice cost.
        ManaProduction::AnyTypeProduceableBy {
            land_filter: TargetFilter::CostPaidObject,
            ..
        } => ProductionReading::Referential {
            population: population_members(
                state,
                CouldProducePopulation::CostCandidates {
                    controller: node.controller,
                    source: node.source_id,
                    ability: frame.cost_owner,
                },
            ),
            measure: CouldProduceMeasure::Types,
        },
        ManaProduction::AnyTypeProduceableBy { land_filter, .. } => {
            ProductionReading::Referential {
                population: population_members(
                    state,
                    CouldProducePopulation::Matching {
                        land_filter,
                        controller: node.controller,
                        source: node.source_id,
                    },
                ),
                measure: CouldProduceMeasure::Types,
            }
        }
        ManaProduction::OpponentLandColors { .. } => ProductionReading::Referential {
            population: population_members(
                state,
                CouldProducePopulation::OpponentLands {
                    controller: node.controller,
                },
            ),
            measure: CouldProduceMeasure::Colors,
        },
        // CR 202.2c + CR 106.5: the colors of the object `scope` binds, read by
        // the runtime resolver against the node's targets. An object no earlier
        // instruction handed over (or one not every seat may see) is unbound,
        // so there are no colors — never every color.
        ManaProduction::AnyCombinationOfObjectColors { scope, .. } => ProductionReading::Direct(
            object_colors_for_scope(state, Some(node), *scope)
                .into_iter()
                .map(ManaType::from)
                .collect(),
        ),
        // CR 106.1 + CR 109.5: colors among the permanents a filter matches,
        // evaluated in the resolving ability's context — as `effects::mana`
        // resolves it — when the frame decides every binding the filter reads.
        // A filter the frame cannot decide keeps the source-anchored reading.
        ManaProduction::AnyOneColorAmongPermanents { filter, .. }
        | ManaProduction::DistinctColorsAmongPermanents { filter } => {
            if filter_binding_diverges(filter, frame.reader()) {
                enumerated()
            } else {
                ProductionReading::Direct(
                    distinct_colors_among_permanents(state, Some(node), node.source_id, filter)
                        .into_iter()
                        .map(ManaType::from)
                        .collect(),
                )
            }
        }
        // CR 603.2 + CR 106.7: "that land produced" names the trigger's own
        // event, which a hypothetical resolution does not have —
        // `state.current_trigger_event` may hold an unrelated one — so no type
        // can be defined this way.
        ManaProduction::TriggerEventManaType => ProductionReading::Direct(Vec::new()),
        // State keyed by source or controller (a persisted choice — else the
        // runtime's every-color prompt — a noted type, an exiled card's colors,
        // a commander's identity) or nothing at all: the same answer whatever
        // the frame has bound.
        ManaProduction::Fixed { .. }
        | ManaProduction::Colorless { .. }
        | ManaProduction::AnyOneColor { .. }
        | ManaProduction::AnyCombination { .. }
        | ManaProduction::ChosenColor { .. }
        | ManaProduction::NotedType { .. }
        | ManaProduction::AnyInCommandersColorIdentity { .. }
        | ManaProduction::Mixed { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::ChoiceAmongCombinations { .. } => enumerated(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::layers::flush_layers;
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityUseTally, Comparator, ControllerRef, EffectOutcomeSignal,
        ManaContribution, QuantityRef, SacrificeCost, SiblingCondition, SubAbilityLink,
        TriggerCondition, TriggerDefinition, TypedFilter, UnlessPayModifier,
    };
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCostShard};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    const P0_ID: PlayerId = PlayerId(0);
    const P1_ID: PlayerId = PlayerId(1);

    // Verbatim Oracle text (MTGJSON), reminder text omitted.
    const REFLECTING_POOL: &str =
        "{T}: Add one mana of any type that a land you control could produce.";
    const EXOTIC_ORCHARD: &str =
        "{T}: Add one mana of any color that a land an opponent controls could produce.";
    const CONTAMINATION: &str = "At the beginning of your upkeep, sacrifice this enchantment \
         unless you sacrifice a creature.\nIf a land is tapped for mana, it produces {B} instead \
         of any other type and amount.";
    const RITUAL_OF_SUBDUAL: &str = "Cumulative upkeep {2}\nIf a land is tapped for mana, it \
         produces colorless mana instead of any other type.";
    const MANA_REFLECTION: &str =
        "If you tap a permanent for mana, it produces twice as much of that mana instead.";
    const GAEAS_CRADLE: &str = "{T}: Add {G} for each creature you control.";
    const CALCIFORM_POOLS: &str = "{T}: Add {C}.\n{1}, {T}: Put a storage counter on this \
         land.\n{1}, Remove X storage counters from this land: Add X mana in any combination of \
         {W} and/or {U}.";
    const BOTTOMLESS_VAULT: &str = "This land enters tapped.\nYou may choose not to untap this \
         land during your untap step.\nAt the beginning of your upkeep, if this land is tapped, \
         put a storage counter on it.\n{T}, Remove any number of storage counters from this land: \
         Add {B} for each storage counter removed this way.";
    const SOLDEVI_ADNATE: &str = "{T}, Sacrifice a black or artifact creature: Add an amount of \
         {B} equal to the sacrificed creature's mana value.";
    const DRESS_DOWN: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose \
         all abilities.\nAt the beginning of the end step, sacrifice this enchantment.";

    fn main_phase_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = P0_ID;
        state.priority_player = P0_ID;
        state.waiting_for = crate::types::game_state::WaitingFor::Priority { player: P0_ID };
        state.turn_number = 2;
        state
    }

    fn mana(produced: ManaProduction) -> Effect {
        Effect::Mana {
            produced,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        }
    }

    fn fixed(color: ManaColor) -> ManaProduction {
        ManaProduction::Fixed {
            colors: vec![color],
            contribution: ManaContribution::Base,
        }
    }

    fn colorless() -> ManaProduction {
        ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        }
    }

    /// "{T}: Add <produced>."
    fn tap_mana_ability(produced: ManaProduction) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, mana(produced)).cost(AbilityCost::Tap)
    }

    fn reflecting_pool_production(land_filter: TargetFilter) -> ManaProduction {
        ManaProduction::AnyTypeProduceableBy {
            count: QuantityExpr::Fixed { value: 1 },
            land_filter,
        }
    }

    fn lands_you_control() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You))
    }

    fn add_object(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        zone: Zone,
        core_type: CoreType,
        abilities: Vec<AbilityDefinition>,
    ) -> ObjectId {
        let id = create_object(state, CardId(700), controller, name.to_string(), zone);
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(core_type);
        object.base_card_types = object.card_types.clone();
        object.summoning_sick = false;
        Arc::make_mut(&mut object.abilities).extend(abilities.iter().cloned());
        Arc::make_mut(&mut object.base_abilities).extend(abilities);
        id
    }

    fn add_land(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        abilities: Vec<AbilityDefinition>,
    ) -> ObjectId {
        add_object(
            state,
            controller,
            name,
            Zone::Battlefield,
            CoreType::Land,
            abilities,
        )
    }

    /// A Squandered Resources-shaped enchantment: "Sacrifice a land: Add one mana
    /// of any type the sacrificed land could produce."
    fn add_squandered(state: &mut GameState) -> ObjectId {
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            mana(reflecting_pool_production(TargetFilter::CostPaidObject)),
        )
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::land()),
            1,
        )));
        add_object(
            state,
            P0_ID,
            "Squandered Resources",
            Zone::Battlefield,
            CoreType::Enchantment,
            vec![ability],
        )
    }

    fn scenario() -> GameScenario {
        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
        scenario
    }

    fn flushed(mut runner: GameRunner) -> GameRunner {
        runner.state_mut().layers_dirty.mark_full();
        flush_layers(runner.state_mut());
        runner
    }

    // ---- U-a: the least fixed point -------------------------------------

    /// U-a: two Reflecting Pools alone could produce nothing (Reflecting Pool
    /// ruling: "won't help each other"); adding a Forest anchors both.
    #[test]
    fn pools_help_each_other_only_once_a_land_anchors_a_type() {
        let mut state = main_phase_state();
        let pool = add_land(
            &mut state,
            P0_ID,
            "Reflecting Pool",
            vec![tap_mana_ability(reflecting_pool_production(
                lands_you_control(),
            ))],
        );
        add_land(
            &mut state,
            P0_ID,
            "Reflecting Pool",
            vec![tap_mana_ability(reflecting_pool_production(
                lands_you_control(),
            ))],
        );
        assert!(could_produce(&state, pool).is_empty());

        add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        assert_eq!(could_produce(&state, pool), vec![ManaType::Green]);
    }

    /// U-a: a three-hop chain (each clause names the next land) reaches its
    /// anchor, and the answer does not depend on battlefield order.
    #[test]
    fn a_three_hop_chain_reaches_its_anchor_in_any_order() {
        let mut state = main_phase_state();
        let named = |name: &str| TargetFilter::Named {
            name: name.to_string(),
        };
        let first = add_land(
            &mut state,
            P0_ID,
            "Hop A",
            vec![tap_mana_ability(reflecting_pool_production(named("Hop B")))],
        );
        add_land(
            &mut state,
            P1_ID,
            "Hop B",
            vec![tap_mana_ability(reflecting_pool_production(named("Hop C")))],
        );
        add_land(
            &mut state,
            P0_ID,
            "Hop C",
            vec![tap_mana_ability(reflecting_pool_production(named(
                "Anchor",
            )))],
        );
        add_land(
            &mut state,
            P1_ID,
            "Anchor",
            vec![tap_mana_ability(fixed(ManaColor::Red))],
        );
        assert_eq!(could_produce(&state, first), vec![ManaType::Red]);

        state.battlefield = state.battlefield.iter().rev().copied().collect();
        assert_eq!(could_produce(&state, first), vec![ManaType::Red]);
    }

    // ---- U-b: population filters -----------------------------------------

    /// U-b: "a land you control" admits neither a non-land producer nor an
    /// opponent's land.
    #[test]
    fn a_you_control_census_excludes_non_lands_and_opponents_lands() {
        let mut state = main_phase_state();
        let pool = add_land(
            &mut state,
            P0_ID,
            "Reflecting Pool",
            vec![tap_mana_ability(reflecting_pool_production(
                lands_you_control(),
            ))],
        );
        add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        add_object(
            &mut state,
            P0_ID,
            "Red Mana Creature",
            Zone::Battlefield,
            CoreType::Creature,
            vec![tap_mana_ability(fixed(ManaColor::Red))],
        );
        add_land(
            &mut state,
            P1_ID,
            "Opponent Swamp",
            vec![tap_mana_ability(fixed(ManaColor::Black))],
        );

        assert_eq!(
            census(
                &state,
                CouldProducePopulation::Matching {
                    land_filter: &lands_you_control(),
                    controller: P0_ID,
                    source: pool,
                },
                CouldProduceMeasure::Types,
            ),
            vec![ManaType::Green]
        );
    }

    /// CR 702.26b + CR 702.26d: a phased-out opponent land stays on the
    /// battlefield under its controller but anchors nothing — the Orchard
    /// census skips it, and so does a Reflecting Pool reading that Orchard.
    #[test]
    fn a_phased_out_opponent_land_anchors_no_census() {
        let mut state = main_phase_state();
        let pool = add_land(
            &mut state,
            P0_ID,
            "Reflecting Pool",
            vec![tap_mana_ability(reflecting_pool_production(
                lands_you_control(),
            ))],
        );
        let orchard = add_land(
            &mut state,
            P0_ID,
            "Exotic Orchard",
            vec![tap_mana_ability(ManaProduction::OpponentLandColors {
                count: QuantityExpr::Fixed { value: 1 },
            })],
        );
        let forest = add_land(
            &mut state,
            P1_ID,
            "Opponent Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        assert_eq!(could_produce(&state, pool), vec![ManaType::Green]);

        let mut events = Vec::new();
        crate::game::phasing::phase_out_object(
            &mut state,
            forest,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut events,
        );
        assert!(state.objects[&forest].is_phased_out(), "reach-guard");
        assert!(state.battlefield.contains(&forest), "reach-guard");
        assert!(could_produce(&state, orchard).is_empty());
        assert!(could_produce(&state, pool).is_empty());
    }

    // ---- U-c: replacements in every order ---------------------------------

    fn forest_under(enchantments: &[(&str, &str)]) -> (GameRunner, ObjectId) {
        let mut scenario = scenario();
        for (name, oracle) in enchantments {
            scenario.add_enchantment_from_oracle(P0, name, oracle);
        }
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        (scenario.build(), forest)
    }

    fn tapped_green(runner: &GameRunner, forest: ObjectId, tap_state: ManaTapState) -> ManaTypeSet {
        crate::game::replacement::hypothetical_produced_mana_types(
            runner.state(),
            forest,
            P0,
            ManaType::Green,
            1,
            tap_state,
        )
    }

    /// U-c (CR 616.1e + CR 616.1f): Contamination and Ritual of Subdual on one
    /// Forest — whichever applies last wins, so both {B} and {C} are possible.
    #[test]
    fn two_replacements_are_read_in_every_order() {
        let (runner, forest) = forest_under(&[
            ("Contamination", CONTAMINATION),
            ("Ritual of Subdual", RITUAL_OF_SUBDUAL),
        ]);
        assert_eq!(
            tapped_green(&runner, forest, ManaTapState::FromTap),
            ManaTypeSet::of(ManaType::Black).with(ManaType::Colorless)
        );
    }

    /// U-c: a doubler keeps the type; a production that is not "tapped for
    /// mana" escapes a `TappedForMana` replacement (CR 106.12b); a parse-only
    /// definition passes production through.
    #[test]
    fn replacement_scope_and_modification_shape_are_respected() {
        let (runner, forest) = forest_under(&[("Mana Reflection", MANA_REFLECTION)]);
        assert_eq!(
            tapped_green(&runner, forest, ManaTapState::FromTap),
            ManaTypeSet::of(ManaType::Green)
        );

        let (mut runner, forest) = forest_under(&[("Contamination", CONTAMINATION)]);
        assert_eq!(
            tapped_green(&runner, forest, ManaTapState::FromTap),
            ManaTypeSet::of(ManaType::Black),
            "reach-guard: Contamination applies to a Forest tapped for mana"
        );
        assert_eq!(
            tapped_green(&runner, forest, ManaTapState::NotFromTap),
            ManaTypeSet::of(ManaType::Green)
        );
        let object_ids: Vec<ObjectId> = runner.state().objects.keys().copied().collect();
        for object_id in object_ids {
            let object = runner.state_mut().objects.get_mut(&object_id).unwrap();
            for definition in Arc::make_mut(&mut object.replacement_definitions.0) {
                definition.mana_modification = None;
            }
        }
        assert_eq!(
            tapped_green(&runner, forest, ManaTapState::FromTap),
            ManaTypeSet::of(ManaType::Green)
        );
    }

    // ---- U-d: layered abilities only ------------------------------------

    fn add_plains_island(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let land = add_land(state, controller, "Plains Island", vec![]);
        let object = state.objects.get_mut(&land).unwrap();
        object
            .card_types
            .subtypes
            .extend(["Plains".to_string(), "Island".to_string()]);
        object.base_card_types = object.card_types.clone();
        land
    }

    /// U-d (CR 305.6): each basic land type grants its own intrinsic ability
    /// once layers have run, so a Plains Island could produce {W} and {U}, for
    /// its own reading and for an opponent's Orchard census.
    #[test]
    fn every_intrinsic_basic_land_ability_is_read_from_the_layered_list() {
        let mut state = main_phase_state();
        let dual = add_plains_island(&mut state, P1_ID);
        assert!(
            could_produce(&state, dual).is_empty(),
            "before layers run, the subtype alone grants nothing"
        );
        state.layers_dirty.mark_full();
        flush_layers(&mut state);
        assert_eq!(
            could_produce(&state, dual),
            vec![ManaType::White, ManaType::Blue]
        );
        assert_eq!(
            census(
                &state,
                CouldProducePopulation::OpponentLands { controller: P0_ID },
                CouldProduceMeasure::Colors,
            ),
            vec![ManaType::White, ManaType::Blue]
        );
    }

    /// U-d (CR 613.1f): an ability-removing effect takes the intrinsic
    /// abilities away though the subtypes stay.
    #[test]
    fn a_removed_intrinsic_ability_is_not_resurrected_by_the_subtype() {
        let mut scenario = scenario();
        scenario.add_enchantment_from_oracle(P0, "Dress Down", DRESS_DOWN);
        let dual = scenario
            .add_land_from_oracle(P0, "Plains Island Creature", "")
            .with_subtypes(vec!["Plains", "Island"])
            .id();
        let mut runner = scenario.build();
        {
            let object = runner.state_mut().objects.get_mut(&dual).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.base_card_types = object.card_types.clone();
        }
        let runner = flushed(runner);
        let object = &runner.state().objects[&dual];
        assert!(object
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Island"));
        assert!(
            object.abilities.is_empty(),
            "reach-guard: Dress Down blanked it"
        );
        assert!(could_produce(runner.state(), dual).is_empty());
    }

    // ---- U-e: quantity ------------------------------------------------------

    fn could_produce_on(oracle_name: &str, oracle: &str, creatures: usize) -> Vec<ManaType> {
        let mut scenario = scenario();
        let land = scenario.add_land_from_oracle(P0, oracle_name, oracle).id();
        for _ in 0..creatures {
            scenario.add_vanilla(P0, 1, 1);
        }
        let runner = scenario.build();
        could_produce(runner.state(), land)
    }

    /// U-e (CR 106.5): a state count of zero means no mana and no type; a count
    /// the ability's own activation fixes (X, removed storage counters, the cost
    /// referent) is admitted whatever it would be.
    #[test]
    fn state_counts_gate_production_and_activation_fixed_counts_are_admitted() {
        assert!(could_produce_on("Gaea's Cradle", GAEAS_CRADLE, 0).is_empty());
        assert_eq!(
            could_produce_on("Gaea's Cradle", GAEAS_CRADLE, 1),
            vec![ManaType::Green]
        );
        assert_eq!(
            could_produce_on("Calciform Pools", CALCIFORM_POOLS, 0),
            vec![ManaType::White, ManaType::Blue, ManaType::Colorless]
        );
        assert_eq!(
            could_produce_on("Bottomless Vault", BOTTOMLESS_VAULT, 0),
            vec![ManaType::Black]
        );

        let mut scenario = scenario();
        let adnate = scenario
            .add_creature_from_oracle(P0, "Soldevi Adnate", 1, 2, SOLDEVI_ADNATE)
            .id();
        let runner = scenario.build();
        assert_eq!(could_produce(runner.state(), adnate), vec![ManaType::Black]);
    }

    // ---- U-f: the chain walk --------------------------------------------

    fn creatures_you_control() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    }

    /// A land whose single ability is `root`.
    fn land_with(state: &mut GameState, root: AbilityDefinition) -> ObjectId {
        add_land(state, P0_ID, "Shaped Land", vec![root])
    }

    fn add_creature(state: &mut GameState) {
        add_object(
            state,
            P0_ID,
            "Bear",
            Zone::Battlefield,
            CoreType::Creature,
            vec![],
        );
    }

    /// "{T}: Add {C}." with `sub` chained after it.
    fn colorless_then(sub: AbilityDefinition) -> AbilityDefinition {
        tap_mana_ability(colorless()).sub_ability(sub)
    }

    fn green_sub() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Green)))
    }

    /// U-f (i) + (ii) (CR 608.2c): a state gate is evaluated now — the gated
    /// type is there iff the gate holds, and the `else` branch iff it fails.
    #[test]
    fn a_state_gate_is_evaluated_and_its_else_read_only_when_false() {
        let gated = green_sub()
            .condition(AbilityCondition::ControllerControlsMatching {
                filter: creatures_you_control(),
            })
            .with_else_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                mana(fixed(ManaColor::Red)),
            ));

        let mut state = main_phase_state();
        let land = land_with(&mut state, colorless_then(gated.clone()));
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Red, ManaType::Colorless]
        );
        add_creature(&mut state);
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless]
        );
    }

    /// U-f (iii) + (iv): an "if you do" gate (CR 608.2c, same resolution) is a
    /// choice, so its mana is read; a "when you do" reflexive ability (CR 603.12)
    /// is a separate ability and is not, though the same mana as a plain sub is.
    #[test]
    fn if_you_do_is_read_and_when_you_do_is_a_separate_ability() {
        let mut state = main_phase_state();
        let if_you_do = green_sub().condition(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        let land = land_with(&mut state, colorless_then(if_you_do));
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless]
        );

        let mut state = main_phase_state();
        let when_you_do = green_sub().condition(AbilityCondition::WhenYouDo);
        let land = land_with(&mut state, colorless_then(when_you_do));
        assert_eq!(could_produce(&state, land), vec![ManaType::Colorless]);

        let mut state = main_phase_state();
        let land = land_with(&mut state, colorless_then(green_sub()));
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless]
        );
    }

    /// U-f (v) (CR 106.7 "ignore … costs" + CR 702.24a): mana inside a cost —
    /// a Braid of Fire-shaped "unless you pay: add {R}" on a chain link, or a
    /// cost on an inline branch the resolution chooses — is a cost, not what the
    /// ability produces; the same mana as a sub is read. The chain-link shape is
    /// excluded because the walk reads each node's effect, never its definition's
    /// costs; the branch shape is the one only `CostCarriers::Skip` excludes.
    #[test]
    fn mana_inside_a_cost_is_not_read() {
        let red_cost = AbilityCost::EffectCost {
            effect: Box::new(mana(fixed(ManaColor::Red))),
        };
        let mut state = main_phase_state();
        let land = land_with(
            &mut state,
            colorless_then(green_sub().unless_pay(UnlessPayModifier {
                cost: red_cost,
                payer: TargetFilter::Controller,
            })),
        );
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless],
            "the {{R}} in the unless cost is not read"
        );

        let branch_with_red_cost =
            AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Green))).cost(
                AbilityCost::EffectCost {
                    effect: Box::new(mana(fixed(ManaColor::Red))),
                },
            );
        let choose = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: crate::types::ability::PlayerFilter::Controller,
                branches: vec![branch_with_red_cost],
            },
        );
        let mut state = main_phase_state();
        let land = land_with(&mut state, colorless_then(choose));
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless],
            "the branch's {{G}} is read and its {{R}} cost is not"
        );

        let mut state = main_phase_state();
        let land = land_with(
            &mut state,
            colorless_then(AbilityDefinition::new(
                AbilityKind::Spell,
                mana(fixed(ManaColor::Red)),
            )),
        );
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Red, ManaType::Colorless]
        );
    }

    /// U-f (vi) (CR 607.2a): the linked-exile store is state at that time, so an
    /// "instead" gated on it is decided now — exactly {C} with nothing linked,
    /// exactly {B} with one card linked, never both.
    #[test]
    fn a_linked_exile_instead_is_decided_now() {
        let instead = AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Black)))
            .condition(AbilityCondition::ConditionInstead {
                inner: Box::new(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::CardsExiledBySource,
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            });
        let mut state = main_phase_state();
        let land = land_with(&mut state, colorless_then(instead));
        assert_eq!(could_produce(&state, land), vec![ManaType::Colorless]);

        let exiled = create_object(
            &mut state,
            CardId(701),
            P0_ID,
            "Exiled Card".to_string(),
            Zone::Exile,
        );
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: land,
            kind: ExileLinkKind::TrackedBySource,
        });
        assert_eq!(
            crate::game::players::linked_exile_cards_for_source(&state, land).len(),
            1,
            "reach-guard: the card is linked to the land"
        );
        assert_eq!(could_produce(&state, land), vec![ManaType::Black]);
    }

    /// U-f (vii): a gate composed over a count the resolution fixes (X) is read
    /// both ways; the same gate over a state count is decided now.
    #[test]
    fn a_quantity_gate_is_read_by_its_operands() {
        let gate = |lhs: QuantityRef| AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref { qty: lhs },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };
        let mut state = main_phase_state();
        let land = land_with(
            &mut state,
            colorless_then(green_sub().condition(gate(QuantityRef::Variable {
                name: "X".to_string(),
            }))),
        );
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless]
        );

        let lands_played = || QuantityRef::LandsPlayedThisTurn {
            player: crate::types::ability::PlayerScope::Controller,
            from_zones: None,
        };
        let mut state = main_phase_state();
        let land = land_with(
            &mut state,
            colorless_then(green_sub().condition(gate(lands_played()))),
        );
        assert_eq!(could_produce(&state, land), vec![ManaType::Colorless]);
        state.players[0].lands_played_this_turn = 1;
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Green, ManaType::Colorless]
        );
    }

    /// A non-mana activated ability, "{R}: Draw a card.", whose last sub adds
    /// {G} — gated on `gate` when one is given — `nesting` draw links below the
    /// root (0: the gated sub is the root's own sub; 1: a sub of a sub).
    fn ordinal_ability(gate: Option<AbilityCondition>, nesting: usize) -> AbilityDefinition {
        let draw = || Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        };
        let mut chain = green_sub();
        if let Some(gate) = gate {
            chain = chain.condition(gate);
        }
        chain.sub_link = SubAbilityLink::SequentialSibling;
        chain.sibling_condition = SiblingCondition::Dependent;
        for _ in 0..nesting {
            chain = AbilityDefinition::new(AbilityKind::Spell, draw()).sub_ability(chain);
        }
        AbilityDefinition::new(AbilityKind::Activated, draw())
            .cost(AbilityCost::Mana {
                cost: crate::types::mana::ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 0,
                },
            })
            .sub_ability(chain)
    }

    fn resolved_count_is(n: u32) -> AbilityCondition {
        AbilityCondition::AbilityUseCountThisTurn {
            tally: AbilityUseTally::Resolved,
            comparator: Comparator::EQ,
            n,
        }
    }

    /// Reads `land` with `(land, 0)` stamped to each of `prior` resolutions
    /// (`None`: no entry), reading the stamp back as the reach-guard.
    fn could_produce_after_resolutions(
        state: &mut GameState,
        land: ObjectId,
        prior: Option<u32>,
    ) -> Vec<ManaType> {
        match prior {
            Some(count) => {
                state.ability_resolutions_this_turn.insert((land, 0), count);
            }
            None => {
                state.ability_resolutions_this_turn.remove(&(land, 0));
            }
        }
        assert_eq!(
            state.ability_resolutions_this_turn.get(&(land, 0)).copied(),
            prior,
            "reach-guard: the ledger reads the stamped count"
        );
        could_produce(state, land)
    }

    /// U-f′ (viii) (CR 106.7 + CR 608.2c): an ordinal gate is evaluated
    /// against the ledger the next resolution would read — the live count plus
    /// the one resolution being asked about — so "the third time" holds only
    /// after exactly two prior resolutions.
    #[test]
    fn an_ordinal_gate_reads_the_next_resolutions_count() {
        let mut state = main_phase_state();
        let ungated = land_with(&mut state, ordinal_ability(None, 0));
        assert_eq!(
            could_produce(&state, ungated),
            vec![ManaType::Green],
            "reach-guard: the sub is reached"
        );

        let mut state = main_phase_state();
        let land = land_with(&mut state, ordinal_ability(Some(resolved_count_is(3)), 0));
        for (prior, expected) in [
            (None, vec![]),
            (Some(1), vec![]),
            (Some(2), vec![ManaType::Green]),
            (Some(3), vec![]),
        ] {
            assert_eq!(
                could_produce_after_resolutions(&mut state, land, prior),
                expected,
                "{prior:?} prior resolutions"
            );
        }
    }

    /// U-f′ (b) (CR 608.2c): the printed index reaches a gate two links below
    /// the root, as `apply_parent_chain_context` hands it down one hop at a
    /// time.
    #[test]
    fn an_ordinal_gate_two_links_deep_reads_the_root_index() {
        let mut state = main_phase_state();
        let ungated = land_with(&mut state, ordinal_ability(None, 1));
        assert_eq!(
            could_produce(&state, ungated),
            vec![ManaType::Green],
            "reach-guard: the nested sub is reached"
        );

        let mut state = main_phase_state();
        let land = land_with(&mut state, ordinal_ability(Some(resolved_count_is(3)), 1));
        for (prior, expected) in [
            (None, vec![]),
            (Some(1), vec![]),
            (Some(2), vec![ManaType::Green]),
            (Some(3), vec![]),
        ] {
            assert_eq!(
                could_produce_after_resolutions(&mut state, land, prior),
                expected,
                "{prior:?} prior resolutions"
            );
        }
    }

    /// U-f′ (c) (CR 602.2a): an activated ability that resolves was announced
    /// first, so the projection counts that activation too — and only for the
    /// ability's own key.
    #[test]
    fn an_activation_count_gate_counts_the_projected_activation() {
        let activated_at_least_twice = AbilityCondition::AbilityUseCountThisTurn {
            tally: AbilityUseTally::Activated,
            comparator: Comparator::GE,
            n: 2,
        };
        let mut state = main_phase_state();
        let land = land_with(
            &mut state,
            ordinal_ability(Some(activated_at_least_twice.clone()), 0),
        );
        let other = land_with(
            &mut state,
            ordinal_ability(Some(activated_at_least_twice), 0),
        );
        assert!(
            could_produce(&state, land).is_empty(),
            "no activation yet: the projected activation is the first"
        );

        state.activated_abilities_this_turn.insert((land, 0), 1);
        assert_eq!(
            state.activated_abilities_this_turn.get(&(land, 0)).copied(),
            Some(1),
            "reach-guard: the ledger reads the stamped count"
        );
        assert_eq!(could_produce(&state, land), vec![ManaType::Green]);
        assert!(
            could_produce(&state, other).is_empty(),
            "another permanent's ledger key is its own"
        );
    }

    /// U-f′ (e) (CR 106.7 + CR 605.3b): a mana ability's root is built
    /// index-less, as the runtime builds it, so its ledger is not projected and
    /// an ordinal gate on it keeps the both-ways reading at every count.
    #[test]
    fn an_unprojected_mana_ability_reads_its_ordinal_both_ways() {
        let ability = colorless_then(green_sub().condition(resolved_count_is(1)));
        let mut state = main_phase_state();
        let land = land_with(&mut state, ability.clone());

        let root = build_resolved_from_def(&ability, land, P0_ID);
        let (_, ledger) =
            resolution_entry_state(&state, &root, &ability, HypotheticalEntry::Activation);
        assert_eq!(
            ledger,
            HypotheticalLedger::Live,
            "reach-guard: the index-less root is not projected"
        );
        for prior in [None, Some(0), Some(1)] {
            assert_eq!(
                could_produce_after_resolutions(&mut state, land, prior),
                vec![ManaType::Green, ManaType::Colorless],
                "{prior:?} prior resolutions"
            );
        }
    }

    // ---- U-g: triggered abilities -----------------------------------------

    /// U-g (CR 603.4): a triggered ability's mana counts only while its
    /// intervening "if" holds.
    #[test]
    fn a_triggered_production_follows_its_intervening_if() {
        let mut state = main_phase_state();
        let land = add_land(&mut state, P0_ID, "Trigger Land", vec![]);
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone).execute(
            AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Green))),
        );
        trigger.condition = Some(TriggerCondition::ControlsType {
            filter: creatures_you_control(),
        });
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        assert!(could_produce(&state, land).is_empty());
        add_creature(&mut state);
        assert_eq!(could_produce(&state, land), vec![ManaType::Green]);
    }

    // ---- The LKI capture and the cost referent (migrated) -----------------

    /// The cost-paid capture is filled for a permanent and empty for a card in
    /// another zone ("could produce" is defined for permanents).
    #[test]
    fn capture_is_filled_on_the_battlefield_only() {
        let mut state = main_phase_state();
        let forest = add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        let graveyard_forest = add_object(
            &mut state,
            P0_ID,
            "Graveyard Forest",
            Zone::Graveyard,
            CoreType::Land,
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );

        let on_battlefield = crate::game::mana_sources::snapshot_with_produceable_mana_types(
            &state,
            &state.objects[&forest],
        );
        assert_eq!(on_battlefield.produceable_mana_types, vec![ManaType::Green]);
        let in_graveyard = crate::game::mana_sources::snapshot_with_produceable_mana_types(
            &state,
            &state.objects[&graveyard_forest],
        );
        assert!(in_graveyard.produceable_mana_types.is_empty());
    }

    /// The captured field is omitted when empty, absent keys deserialize to
    /// empty, and a populated set round-trips.
    #[test]
    fn produceable_mana_types_serde_is_backward_compatible() {
        let mut state = main_phase_state();
        let forest = add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );

        let empty = state.objects[&forest].snapshot_for_mana_spent();
        let empty_json = serde_json::to_value(&empty).unwrap();
        assert!(empty_json.get("produceable_mana_types").is_none());
        let restored: crate::types::game_state::LKISnapshot =
            serde_json::from_value(empty_json).unwrap();
        assert!(restored.produceable_mana_types.is_empty());

        let populated = crate::game::mana_sources::snapshot_with_produceable_mana_types(
            &state,
            &state.objects[&forest],
        );
        let round_trip: crate::types::game_state::LKISnapshot =
            serde_json::from_value(serde_json::to_value(&populated).unwrap()).unwrap();
        assert_eq!(round_trip.produceable_mana_types, vec![ManaType::Green]);
    }

    /// Before activation the cost referent ranges over the cost's legal choices —
    /// your own lands only (CR 701.21a) — and a land that could produce nothing
    /// contributes nothing (CR 106.5).
    #[test]
    fn an_unbound_referent_unions_the_legal_sacrifice_choices() {
        let mut state = main_phase_state();
        let squandered = add_squandered(&mut state);
        add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        add_land(
            &mut state,
            P0_ID,
            "Island",
            vec![tap_mana_ability(fixed(ManaColor::Blue))],
        );
        add_land(
            &mut state,
            P1_ID,
            "Opponent Swamp",
            vec![tap_mana_ability(fixed(ManaColor::Black))],
        );
        let ability = state.objects[&squandered].abilities[0].clone();
        let referent = |state: &GameState, ability: &AbilityDefinition| {
            census(
                state,
                CouldProducePopulation::CostCandidates {
                    controller: P0_ID,
                    source: squandered,
                    ability,
                },
                CouldProduceMeasure::Types,
            )
        };

        assert_eq!(
            referent(&state, &ability),
            vec![ManaType::Blue, ManaType::Green],
            "an opponent's Swamp is not a legal sacrifice, so Black is excluded"
        );
        let tap_only = ability.clone().cost(AbilityCost::Tap);
        assert!(
            referent(&state, &tap_only).is_empty(),
            "a cost with no non-self sacrifice has no referent candidates"
        );

        let mut wilds_only = main_phase_state();
        let squandered = add_squandered(&mut wilds_only);
        add_land(&mut wilds_only, P0_ID, "Evolving Wilds", vec![]);
        let ability = wilds_only.objects[&squandered].abilities[0].clone();
        assert!(census(
            &wilds_only,
            CouldProducePopulation::CostCandidates {
                controller: P0_ID,
                source: squandered,
                ability: &ability,
            },
            CouldProduceMeasure::Types,
        )
        .is_empty());
    }

    /// Both castability estimators credit an unbound referent on the same
    /// boards — one mana from a tapped Forest, nothing from a land that could
    /// produce nothing.
    #[test]
    fn castability_estimators_credit_an_unbound_referent() {
        use crate::game::mana_sources::{
            can_cover_shards_with_activatable_mana, feasible_mana_capacity,
        };

        let mut state = main_phase_state();
        let squandered = add_squandered(&mut state);
        let forest = add_land(
            &mut state,
            P0_ID,
            "Forest",
            vec![tap_mana_ability(fixed(ManaColor::Green))],
        );
        state.objects.get_mut(&forest).unwrap().tapped = true;
        assert_eq!(feasible_mana_capacity(&state, squandered, P0_ID, None), 1);
        assert_eq!(
            can_cover_shards_with_activatable_mana(
                &state,
                P0_ID,
                None,
                None,
                &[ManaCostShard::Green]
            ),
            (true, 1)
        );

        let mut wilds_only = main_phase_state();
        let squandered = add_squandered(&mut wilds_only);
        add_land(&mut wilds_only, P0_ID, "Evolving Wilds", vec![]);
        assert_eq!(
            feasible_mana_capacity(&wilds_only, squandered, P0_ID, None),
            0
        );
        assert_eq!(
            can_cover_shards_with_activatable_mana(
                &wilds_only,
                P0_ID,
                None,
                None,
                &[ManaCostShard::Green],
            ),
            (false, 0)
        );
    }

    #[test]
    fn reflecting_pool_and_orchard_cards_lower_to_referential_clauses() {
        let mut scenario = scenario();
        let pool = scenario
            .add_land_from_oracle(P0, "Reflecting Pool", REFLECTING_POOL)
            .id();
        let orchard = scenario
            .add_land_from_oracle(P1, "Exotic Orchard", EXOTIC_ORCHARD)
            .id();
        scenario.add_basic_land(P0, ManaColor::Green);
        let runner = scenario.build();
        assert_eq!(could_produce(runner.state(), pool), vec![ManaType::Green]);
        assert_eq!(
            could_produce(runner.state(), orchard),
            vec![ManaType::Green]
        );
    }

    // ---- F1: the ordinal ledger through real resolutions ------------------

    // Verbatim Oracle text (MTGJSON), reminder text omitted.
    const ASHAYA: &str = "Ashaya's power and toughness are each equal to the number of lands \
         you control.\nNontoken creatures you control are Forest lands in addition to their \
         other types.";
    const SOULBRIGHT_SEEKER: &str = "As an additional cost to cast this spell, behold an \
         Elemental or pay {2}.\n{R}: Target creature you control gains trample until end of \
         turn. If this is the third time this ability has resolved this turn, add {R}{R}{R}{R}.";
    const OMNATH_LOCUS_OF_ALL: &str = "If you would lose unspent mana, that mana becomes black \
         instead.\nAt the beginning of your first main phase, look at the top card of your \
         library. You may reveal that card if it has three or more colored mana symbols in its \
         mana cost. If you do, add three mana in any combination of its colors and put it into \
         your hand. If you don't reveal it, put it into your hand.";
    const COURSER_OF_KRUPHIX: &str = "Play with the top card of your library revealed.\nYou may \
         play lands from the top of your library.\nLandfall — Whenever a land you control \
         enters, you gain 1 life.";
    const MOX_AMBER: &str =
        "{T}: Add one mana of any color among legendary creatures and planeswalkers you control.";

    /// `controller`'s Soulbright Seeker, made a Forest land by its Ashaya.
    fn seeker_board(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
        scenario.add_creature_from_oracle(controller, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
        scenario
            .add_creature_from_oracle(controller, "Soulbright Seeker", 2, 1, SOULBRIGHT_SEEKER)
            .id()
    }

    /// Resolves the Seeker's ability `times` times through the real activation
    /// pipeline, each paid with {R} put into its controller's pool and
    /// targeting the Seeker itself.
    fn resolve_seeker(runner: &mut GameRunner, seeker: ObjectId, times: u32) {
        let controller = runner.state().objects[&seeker].controller;
        for _ in 0..times {
            let state = runner.state_mut();
            state.priority_player = controller;
            state.waiting_for =
                crate::types::game_state::WaitingFor::Priority { player: controller };
            state.players[controller.0 as usize]
                .mana_pool
                .add(crate::types::mana::ManaUnit::new(
                    ManaType::Red,
                    ObjectId(0),
                    false,
                    vec![],
                ));
            runner.activate(seeker, 0).target_object(seeker).resolve();
        }
        for player in &mut runner.state_mut().players {
            player.mana_pool.mana.clear();
        }
        let state = runner.state_mut();
        state.priority_player = P0;
        state.waiting_for = crate::types::game_state::WaitingFor::Priority { player: P0 };
        state.layers_dirty.mark_full();
        flush_layers(state);
    }

    /// Reach-guards for a Seeker read after `prior` real resolutions: the
    /// ledger holds exactly that count, and Ashaya made the Seeker a land.
    fn assert_seeker_after(runner: &GameRunner, seeker: ObjectId, prior: u32) {
        let ledger = runner
            .state()
            .ability_resolutions_this_turn
            .get(&(seeker, 0))
            .copied();
        assert_eq!(
            ledger,
            (prior > 0).then_some(prior),
            "reach-guard: the ledger"
        );
        assert!(
            runner.state().objects[&seeker]
                .card_types
                .core_types
                .contains(&CoreType::Land),
            "reach-guard: Ashaya made the Seeker a land"
        );
    }

    /// U-census (CR 106.7 + CR 608.2c): the Orchard-class census reads the
    /// Seeker's ordinal against its next resolution — {R} only after exactly
    /// two real resolutions; its intrinsic Forest {G} always.
    #[test]
    fn a_census_reads_the_seekers_ordinal_through_real_resolutions() {
        for prior in 0..=3 {
            let mut scenario = scenario();
            let seeker = seeker_board(&mut scenario, P0);
            let mut runner = flushed(scenario.build());
            resolve_seeker(&mut runner, seeker, prior);
            assert_seeker_after(&runner, seeker, prior);

            let expected = if prior == 2 {
                vec![ManaType::Red, ManaType::Green]
            } else {
                vec![ManaType::Green]
            };
            assert_eq!(
                census(
                    runner.state(),
                    CouldProducePopulation::OpponentLands { controller: P1 },
                    CouldProduceMeasure::Colors,
                ),
                expected,
                "{prior} prior resolutions"
            );
        }
    }

    /// Activates `source`'s first activated ability and reports the type
    /// prompt's options, or the mana that arrived without one.
    fn tap_for_mana(
        runner: &mut GameRunner,
        source: ObjectId,
    ) -> (Option<Vec<ManaType>>, Vec<ManaType>) {
        use crate::types::game_state::{ManaChoicePrompt, WaitingFor};
        let ability_index = runner.state().objects[&source]
            .abilities
            .iter()
            .position(|ability| ability.kind == AbilityKind::Activated)
            .expect("reach-guard: the source has an activated ability");
        runner
            .act(crate::types::actions::GameAction::ActivateAbility {
                source_id: source,
                ability_index,
            })
            .expect("reach-guard: the mana ability is activatable");
        let prompt = match runner.state().waiting_for.clone() {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => Some(options),
            _ => None,
        };
        let pool = runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .iter()
            .map(|unit| unit.color)
            .collect();
        (prompt, pool)
    }

    /// U-orchard (CR 106.7 + CR 608.2c): a real Exotic Orchard activation reads
    /// the opponent's Seeker after the opponent's own real resolutions.
    #[test]
    fn an_exotic_orchard_reads_an_opponents_seeker_ordinal() {
        for (prior, expected_prompt, expected_pool) in [
            (0, None, vec![ManaType::Green]),
            (2, Some(vec![ManaType::Red, ManaType::Green]), vec![]),
        ] {
            let mut scenario = scenario();
            let seeker = seeker_board(&mut scenario, P1);
            let orchard = scenario
                .add_land_from_oracle(P0, "Exotic Orchard", EXOTIC_ORCHARD)
                .id();
            let mut runner = flushed(scenario.build());
            resolve_seeker(&mut runner, seeker, prior);
            assert_seeker_after(&runner, seeker, prior);
            assert_eq!(runner.state().objects[&seeker].controller, P1);

            let (prompt, pool) = tap_for_mana(&mut runner, orchard);
            assert!(
                prompt.is_some() || !pool.is_empty(),
                "reach-guard: the Orchard's activation ran"
            );
            assert_eq!(prompt, expected_prompt, "{prior} prior resolutions");
            assert_eq!(pool, expected_pool, "{prior} prior resolutions");
        }
    }

    // ---- F2: the objects an earlier instruction looks at ------------------

    fn mana_cost(shards: Vec<ManaCostShard>, generic: u32) -> crate::types::mana::ManaCost {
        crate::types::mana::ManaCost::Cost { shards, generic }
    }

    fn doomsday() -> crate::types::mana::ManaCost {
        mana_cost(vec![ManaCostShard::Black; 3], 0)
    }

    fn esper_charm() -> crate::types::mana::ManaCost {
        mana_cost(
            vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            0,
        )
    }

    /// Ashaya + Omnath, Locus of All (+ Courser of Kruphix when `courser`), with
    /// a card costing `top_cost` on top of a filler card.
    fn omnath_board(
        courser: bool,
        top_cost: crate::types::mana::ManaCost,
    ) -> (GameRunner, ObjectId, ObjectId) {
        let mut scenario = scenario();
        scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
        if courser {
            scenario.add_creature_from_oracle(P0, "Courser of Kruphix", 2, 4, COURSER_OF_KRUPHIX);
        }
        let omnath = scenario
            .add_creature_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH_LOCUS_OF_ALL)
            .id();
        scenario.add_spell_to_library_top(P0, "Filler", false);
        let top = scenario
            .add_spell_to_library_top(P0, "Doomsday", false)
            .with_mana_cost(top_cost)
            .id();
        (flushed(scenario.build()), omnath, top)
    }

    /// T33c (CR 401.5): the publicity gate reads the continuous-reveal rules
    /// authority, not the display carrier `revealed_cards`, which is empty at
    /// this seam.
    #[test]
    fn a_continuously_revealed_top_card_binds_though_the_carrier_is_empty() {
        let (runner, omnath, top) = omnath_board(true, doomsday());
        let state = runner.state();
        assert!(
            !state.revealed_cards.contains(&top),
            "reach-guard: the display carrier does not hold the top card here"
        );
        assert!(
            continuously_revealed_cards(state).contains(&top),
            "reach-guard: Courser keeps the top card revealed"
        );
        assert_eq!(
            could_produce(state, omnath),
            vec![ManaType::Black, ManaType::Green]
        );
    }

    /// T33d (CR 400.2 + CR 401.2): a private look known to the controller is not
    /// publicity — the top card stays unbound.
    #[test]
    fn a_private_look_does_not_bind_the_top_card() {
        let (mut runner, omnath, top) = omnath_board(false, doomsday());
        let audience = crate::game::turn_control::decision_audience_for_player(runner.state(), P0);
        runner
            .state_mut()
            .remember_card_identities(audience, &[top]);
        let state = runner.state();
        assert!(
            state.viewer_knows_card_identity(P0, top),
            "reach-guard: the controller knows the card"
        );
        assert!(!state.viewer_knows_card_identity(P1, top));
        assert_eq!(could_produce(state, omnath), vec![ManaType::Green]);
    }

    /// Resolves the real manifest dread handler for P0, so its two looked-at
    /// cards sit in `revealed_cards` for P0 only while the choice is pending.
    fn pend_manifest_dread(runner: &mut GameRunner, source: ObjectId) {
        let ability = ResolvedAbility::new(Effect::ManifestDread, vec![], source, P0);
        let mut events = Vec::new();
        crate::game::effects::manifest_dread::resolve(runner.state_mut(), &ability, &mut events)
            .expect("manifest dread resolves");
    }

    /// T33e (CR 701.62a): the cards a pending manifest dread looked at are in
    /// `revealed_cards`, but for their controller only — not publicity.
    #[test]
    fn a_manifest_dread_look_does_not_bind_the_top_card() {
        let (mut runner, omnath, top) = omnath_board(false, doomsday());
        pend_manifest_dread(&mut runner, omnath);
        let state = runner.state();
        assert!(
            matches!(
                &state.waiting_for,
                crate::types::game_state::WaitingFor::ManifestDreadChoice { cards, .. }
                    if cards.contains(&top)
            ),
            "reach-guard: manifest dread is pending over the top card"
        );
        assert!(
            state.revealed_cards.contains(&top),
            "reach-guard: the disjunct a bare `revealed_cards` gate would accept"
        );
        assert_eq!(could_produce(state, omnath), vec![ManaType::Green]);
    }

    /// T33e+ (CR 701.20a): the paired positive — the same top card in a plain
    /// reveal of this action binds.
    #[test]
    fn a_momentary_reveal_binds_the_top_card() {
        let (mut runner, omnath, top) = omnath_board(false, doomsday());
        runner.state_mut().revealed_cards.insert(top);
        let state = runner.state();
        assert!(state.revealed_cards.contains(&top), "reach-guard");
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));
        assert_eq!(
            could_produce(state, omnath),
            vec![ManaType::Black, ManaType::Green]
        );
    }

    // ---- The gated-child hand-off -----------------------------------------

    /// The parsed nodes of Omnath, Locus of All's trigger: its `Dig`, its
    /// optional `Reveal` (gated on the top card's mana symbols) and its gated
    /// `Mana`.
    struct OmnathNodes {
        dig: AbilityDefinition,
        reveal: AbilityDefinition,
        symbols_gate: AbilityCondition,
    }

    fn omnath_nodes(runner: &GameRunner, omnath: ObjectId) -> OmnathNodes {
        let execute = runner.state().objects[&omnath]
            .trigger_definitions
            .iter_all()
            .find_map(|entry| entry.definition.execute.clone())
            .expect("reach-guard: Omnath's trigger has an execute");
        let dig = (*execute).clone();
        assert!(
            matches!(&*dig.effect, Effect::Dig { .. }),
            "reach-guard: the trigger opens with a look"
        );
        let reveal = dig
            .sub_ability
            .as_deref()
            .cloned()
            .expect("reach-guard: the reveal follows the look");
        assert!(
            matches!(&*reveal.effect, Effect::Reveal { .. }),
            "reach-guard: the second node is the reveal"
        );
        let symbols_gate = reveal
            .condition
            .clone()
            .expect("reach-guard: the reveal is gated on the mana symbols");
        OmnathNodes {
            dig: AbilityDefinition {
                sub_ability: None,
                ..dig
            },
            reveal: AbilityDefinition {
                sub_ability: None,
                condition: None,
                optional: false,
                ..reveal
            },
            symbols_gate,
        }
    }

    /// "Add one mana in any combination of its colors" — the target's colors.
    fn target_colors_mana() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            mana(ManaProduction::AnyCombinationOfObjectColors {
                count: QuantityExpr::Fixed { value: 1 },
                scope: crate::types::ability::ObjectScope::Target,
            }),
        )
    }

    /// `nodes` chained in order, each the previous one's sub.
    fn chain(nodes: Vec<AbilityDefinition>) -> AbilityDefinition {
        nodes
            .into_iter()
            .rev()
            .reduce(|tail, head| head.sub_ability(tail))
            .expect("a chain has a node")
    }

    /// A land with no ability but a triggered one that executes `execute`.
    fn land_with_trigger(state: &mut GameState, execute: AbilityDefinition) -> ObjectId {
        let land = add_land(state, P0_ID, "Shaped Land", vec![]);
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .trigger_definitions
            .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(execute));
        land
    }

    /// Courser + Omnath (for its parsed nodes) over the given library, top
    /// first; returns the runner, Omnath and the library ids top first.
    fn public_library_board(
        library: Vec<(&str, crate::types::mana::ManaCost)>,
    ) -> (GameRunner, ObjectId, Vec<ObjectId>) {
        let mut scenario = scenario();
        scenario.add_creature_from_oracle(P0, "Courser of Kruphix", 2, 4, COURSER_OF_KRUPHIX);
        let omnath = scenario
            .add_creature_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH_LOCUS_OF_ALL)
            .id();
        let mut ids: Vec<ObjectId> = library
            .into_iter()
            .rev()
            .map(|(name, cost)| {
                scenario
                    .add_spell_to_library_top(P0, name, false)
                    .with_mana_cost(cost)
                    .id()
            })
            .collect();
        ids.reverse();
        let runner = flushed(scenario.build());
        assert_eq!(
            runner
                .state()
                .library_of(P0_ID)
                .iter()
                .copied()
                .take(ids.len())
                .collect::<Vec<_>>(),
            ids,
            "reach-guard: the library order"
        );
        (runner, omnath, ids)
    }

    /// A last-revealed scratch of `state` after `parent` published `ids`.
    fn after_publishing(state: &GameState, ids: &[ObjectId]) -> GameState {
        let mut scratch = state.clone();
        scratch.last_revealed_ids = ids.to_vec();
        scratch
    }

    /// U-gate (a) (CR 608.2c): a gated child the runtime's look hand-off does
    /// not accept is handed nothing, so it stays unbound and its target's
    /// colors are not read — though its gate would pass against the injected
    /// parent.
    #[test]
    fn a_gated_child_the_hand_off_skips_stays_unbound() {
        let (mut runner, omnath, library) = public_library_board(vec![("Doomsday", doomsday())]);
        let nodes = omnath_nodes(&runner, omnath);
        let gated_mana = target_colors_mana().condition(nodes.symbols_gate.clone());
        let execute = chain(vec![nodes.dig.clone(), gated_mana]);

        let dig = build_resolved_from_def(&execute, omnath, P0_ID);
        let mana_child = dig.sub_ability.as_deref().unwrap();
        let scratch = after_publishing(runner.state(), &library[..1]);
        assert!(
            !crate::game::effects::receives_last_revealed(&scratch, &dig, mana_child),
            "reach-guard: the runtime hands the mana child nothing"
        );

        let land = land_with_trigger(runner.state_mut(), execute);
        assert!(could_produce(runner.state(), land).is_empty());
    }

    /// U-gate (a+) (CR 608.2c + CR 701.20e): the paired positive — the same gate
    /// on a child the hand-off accepts (Omnath's reveal) binds, and the mana
    /// below inherits the revealed card.
    #[test]
    fn a_gated_child_the_hand_off_accepts_binds() {
        let (mut runner, omnath, library) = public_library_board(vec![("Doomsday", doomsday())]);
        let nodes = omnath_nodes(&runner, omnath);
        let gated_reveal = AbilityDefinition {
            condition: Some(nodes.symbols_gate.clone()),
            ..nodes.reveal.clone()
        };
        let execute = chain(vec![nodes.dig.clone(), gated_reveal, target_colors_mana()]);

        let dig = build_resolved_from_def(&execute, omnath, P0_ID);
        let reveal_child = dig.sub_ability.as_deref().unwrap();
        let scratch = after_publishing(runner.state(), &library[..1]);
        assert!(
            crate::game::effects::receives_last_revealed(&scratch, &dig, reveal_child),
            "reach-guard: the runtime hands the reveal the looked-at card"
        );
        assert_eq!(
            inject_last_revealed_targets(&scratch, &dig, reveal_child),
            vec![TargetRef::Object(library[0])]
        );

        let land = land_with_trigger(runner.state_mut(), execute);
        assert_eq!(could_produce(runner.state(), land), vec![ManaType::Black]);
    }

    /// U-gate (b) (CR 608.2c): a gated child of a parent whose own targets are
    /// exactly the handed objects binds — the gate block's pre-read and the
    /// child's own read see the same objects.
    #[test]
    fn a_gated_child_of_a_parent_holding_the_handed_objects_binds() {
        let (mut runner, omnath, library) = public_library_board(vec![("Doomsday", doomsday())]);
        let nodes = omnath_nodes(&runner, omnath);
        let gated_reveal = AbilityDefinition {
            condition: Some(nodes.symbols_gate.clone()),
            ..nodes.reveal.clone()
        };
        let execute = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            gated_reveal,
            target_colors_mana(),
        ]);

        let mut middle = build_resolved_from_def(&execute, omnath, P0_ID)
            .sub_ability
            .map(|middle| *middle)
            .unwrap();
        middle.set_unpinned_targets(vec![TargetRef::Object(library[0])]);
        let gated = middle.sub_ability.as_deref().unwrap();
        let scratch = after_publishing(runner.state(), &library[..1]);
        assert_eq!(
            inject_last_revealed_targets(&scratch, &middle, gated),
            middle.targets,
            "reach-guard: the handed objects are the parent's own targets"
        );

        let land = land_with_trigger(runner.state_mut(), execute);
        assert_eq!(could_produce(runner.state(), land), vec![ManaType::Black]);
    }

    /// U-gate (b′) (CR 608.2c): a gated child handed objects other than the ones
    /// its gate is pre-read against stays unbound. The second look inherits
    /// the top card as its targets but publishes the top two, so the gated
    /// reveal is handed both while the gate block pre-reads the top card alone.
    /// (Unbound is the conservative reading: no object, so no colors.)
    #[test]
    fn a_gated_child_whose_pre_read_differs_from_its_hand_off_stays_unbound() {
        let (mut runner, omnath, library) = public_library_board(vec![
            ("Doomsday", doomsday()),
            ("Esper Charm", esper_charm()),
        ]);
        // Both looked-at cards are public, so only the pre-read clause decides.
        runner.state_mut().revealed_cards.insert(library[1]);
        let nodes = omnath_nodes(&runner, omnath);
        let look_two = {
            let mut look = nodes.dig.clone();
            if let Effect::Dig { count, .. } = &mut *look.effect {
                *count = QuantityExpr::Fixed { value: 2 };
            }
            look
        };
        let gated_reveal = AbilityDefinition {
            condition: Some(nodes.symbols_gate.clone()),
            ..nodes.reveal.clone()
        };
        let execute = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            look_two,
            gated_reveal,
            target_colors_mana(),
        ]);

        let mut second_look = build_resolved_from_def(&execute, omnath, P0_ID)
            .sub_ability
            .and_then(|reveal| reveal.sub_ability)
            .map(|look| *look)
            .unwrap();
        second_look.set_unpinned_targets(vec![TargetRef::Object(library[0])]);
        let gated = second_look.sub_ability.as_deref().unwrap();
        let scratch = after_publishing(runner.state(), &library);
        assert_eq!(
            inject_last_revealed_targets(&scratch, &second_look, gated),
            vec![TargetRef::Object(library[0]), TargetRef::Object(library[1])],
            "reach-guard: the gated reveal is handed both cards"
        );
        assert_ne!(
            inject_last_revealed_targets(&scratch, &second_look, gated),
            second_look.targets,
            "reach-guard: the pre-read differs from the hand-off"
        );

        let land = land_with_trigger(runner.state_mut(), execute);
        assert!(could_produce(runner.state(), land).is_empty());
    }

    /// T1 (CR 608.2c): the inheritance hop applies the same pre-read rule. The
    /// mana child is not a look hand-off target, so it inherits the second
    /// look's own targets (the top card); but its gate reads the result object,
    /// which the gate block pre-reads against everything the look published.
    /// The two differ, so the child stays unbound.
    #[test]
    fn an_inheriting_gated_child_whose_pre_read_differs_stays_unbound() {
        let (mut runner, omnath, library) = public_library_board(vec![
            ("Doomsday", doomsday()),
            ("Esper Charm", esper_charm()),
        ]);
        let nodes = omnath_nodes(&runner, omnath);
        let look_two = {
            let mut look = nodes.dig.clone();
            if let Effect::Dig { count, .. } = &mut *look.effect {
                *count = QuantityExpr::Fixed { value: 2 };
            }
            look
        };
        let result_gate = AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Any,
            use_lki: false,
            subject_slot: None,
        };
        assert!(
            crate::game::effects::condition_depends_on_result_object(&result_gate),
            "reach-guard: the gate reads the result object"
        );
        let execute = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            look_two,
            target_colors_mana().condition(result_gate),
        ]);

        let mut second_look = build_resolved_from_def(&execute, omnath, P0_ID)
            .sub_ability
            .and_then(|reveal| reveal.sub_ability)
            .map(|look| *look)
            .unwrap();
        second_look.set_unpinned_targets(vec![TargetRef::Object(library[0])]);
        let mana_child = second_look.sub_ability.as_deref().unwrap();
        let scratch = after_publishing(runner.state(), &library);
        assert!(
            !crate::game::effects::receives_last_revealed(&scratch, &second_look, mana_child),
            "reach-guard: the mana child takes the inheritance hop"
        );
        assert_eq!(
            inherited_parent_occurrences(&second_look, mana_child),
            vec![(TargetRef::Object(library[0]), None)],
            "reach-guard: it inherits the top card"
        );
        assert_eq!(
            gate_pre_read_targets(
                &scratch,
                &second_look,
                mana_child,
                mana_child.condition.as_ref().unwrap()
            ),
            vec![TargetRef::Object(library[0]), TargetRef::Object(library[1])],
            "reach-guard: the gate block pre-reads both published cards"
        );

        let land = land_with_trigger(runner.state_mut(), execute);
        assert!(could_produce(runner.state(), land).is_empty());
    }

    /// U-mana-skip: a subtree that adds no mana is walked without a scratch
    /// state; the same look with mana below binds.
    #[test]
    fn a_mana_free_subtree_is_walked_unbound() {
        let (mut runner, omnath, _library) = public_library_board(vec![("Doomsday", doomsday())]);
        let nodes = omnath_nodes(&runner, omnath);

        let mana_free = chain(vec![nodes.dig.clone(), nodes.reveal.clone()]);
        let reveal_child = build_resolved_from_def(&mana_free, omnath, P0_ID)
            .sub_ability
            .unwrap();
        assert!(!chain_carries_mana(&reveal_child));
        let land = land_with_trigger(runner.state_mut(), mana_free);
        assert!(could_produce(runner.state(), land).is_empty());

        let with_mana = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            target_colors_mana(),
        ]);
        let reveal_child = build_resolved_from_def(&with_mana, omnath, P0_ID)
            .sub_ability
            .unwrap();
        assert!(chain_carries_mana(&reveal_child));
        let land = land_with_trigger(runner.state_mut(), with_mana);
        assert_eq!(could_produce(runner.state(), land), vec![ManaType::Black]);
    }

    /// U-skip (CR 608.2c): a look whose own gate is false never ran, so the
    /// reveal that escapes past it is handed nothing; the twin whose gate
    /// holds hands the reveal the looked-at card.
    #[test]
    fn a_skipped_look_publishes_nothing() {
        let lands_played_gate = AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LandsPlayedThisTurn {
                    player: crate::types::ability::PlayerScope::Controller,
                    from_zones: None,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        };
        for (lands_played, expected) in [(0, vec![]), (1, vec![ManaType::Black])] {
            let (mut runner, omnath, _library) =
                public_library_board(vec![("Doomsday", doomsday())]);
            let nodes = omnath_nodes(&runner, omnath);
            let if_you_do_reveal = AbilityDefinition {
                condition: Some(AbilityCondition::EffectOutcome {
                    signal: EffectOutcomeSignal::OptionalEffectPerformed,
                }),
                ..nodes.reveal.clone()
            };
            let execute = chain(vec![
                nodes.dig.clone().condition(lands_played_gate.clone()),
                if_you_do_reveal,
                target_colors_mana(),
            ]);
            let reveal_child = build_resolved_from_def(&execute, omnath, P0_ID)
                .sub_ability
                .unwrap();
            assert!(
                false_gate_escape(&reveal_child).is_some(),
                "reach-guard: the reveal escapes a false look"
            );
            runner.state_mut().players[0].lands_played_this_turn = lands_played;
            let land = land_with_trigger(runner.state_mut(), execute);
            assert_eq!(
                could_produce(runner.state(), land),
                expected,
                "{lands_played} lands played"
            );
        }
    }

    /// U-o (CR 608.2c + CR 608.2d): below a "you may" whose own gate is false,
    /// "if you do" reads false and "if you don't" reads true; below one whose
    /// gate holds, the player chooses, so both are read.
    #[test]
    fn a_false_gated_you_may_reads_if_you_do_as_not_performed() {
        let creature_gate = AbilityCondition::ControllerControlsMatching {
            filter: creatures_you_control(),
        };
        let if_you_do = AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        };
        let execute = chain(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .optional()
            .condition(creature_gate),
            AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Black)))
                .condition(if_you_do.clone()),
            AbilityDefinition::new(AbilityKind::Spell, mana(fixed(ManaColor::Blue))).condition(
                AbilityCondition::Not {
                    condition: Box::new(if_you_do),
                },
            ),
        ]);

        let mut state = main_phase_state();
        let land = land_with_trigger(&mut state, execute);
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Blue],
            "the gate is false: not performed"
        );
        add_creature(&mut state);
        assert_eq!(
            could_produce(&state, land),
            vec![ManaType::Blue, ManaType::Black],
            "the gate holds: either way"
        );
    }

    /// C-9 (CR 106.7): the hypothetical answer equals what the real first-main
    /// trigger resolution offers, for an eligible mono- and multicolored top
    /// card, an ineligible multicolored one and a colorless one.
    #[test]
    fn omnaths_hypothetical_matches_its_real_resolution() {
        use crate::types::actions::GameAction;
        use crate::types::game_state::{ManaChoicePrompt, WaitingFor};

        for (cost, expected) in [
            (doomsday(), vec![ManaType::Black]),
            (
                esper_charm(),
                vec![ManaType::White, ManaType::Blue, ManaType::Black],
            ),
            (
                mana_cost(vec![ManaCostShard::Blue, ManaCostShard::Black], 0),
                vec![],
            ),
            (mana_cost(vec![], 1), vec![]),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(crate::types::phase::Phase::Upkeep);
            let omnath = scenario
                .add_creature_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH_LOCUS_OF_ALL)
                .id();
            scenario.add_creature_from_oracle(P0, "Courser of Kruphix", 2, 4, COURSER_OF_KRUPHIX);
            let probe = scenario
                .add_spell_to_library_top(P0, "Probe", false)
                .with_mana_cost(cost.clone())
                .id();
            // The draw step takes this card, leaving the probe on top.
            scenario.add_spell_to_library_top(P0, "Drawn", false);
            let mut runner = scenario.build();
            runner.advance_to_phase(crate::types::phase::Phase::PreCombatMain);
            assert_eq!(
                runner.stack_names(),
                vec!["Omnath, Locus of All".to_string()],
                "reach-guard: the real trigger is on the stack"
            );
            assert_eq!(
                runner.state().library_of(P0_ID).front().copied(),
                Some(probe),
                "reach-guard: the probe card is on top"
            );

            let hypothetical = could_produce(runner.state(), omnath);

            let mut runtime: Vec<ManaType> = Vec::new();
            for _ in 0..8 {
                match runner.state().waiting_for.clone() {
                    WaitingFor::OptionalEffectChoice { .. } => {
                        runner
                            .act(GameAction::DecideOptionalEffect { accept: true })
                            .unwrap();
                    }
                    WaitingFor::ChooseManaColor {
                        choice: ManaChoicePrompt::AnyCombination { options, .. },
                        ..
                    } => {
                        runtime = options;
                        break;
                    }
                    WaitingFor::Priority { .. } if !runner.stack_names().is_empty() => {
                        runner.act(GameAction::PassPriority).unwrap();
                    }
                    _ => break,
                }
            }
            if runtime.is_empty() {
                runtime = runner.state().players[0]
                    .mana_pool
                    .mana
                    .iter()
                    .map(|unit| unit.color)
                    .collect();
            }
            let runtime: Vec<ManaType> = runtime
                .into_iter()
                .fold(ManaTypeSet::EMPTY, |set, mana_type| set.with(mana_type))
                .iter()
                .collect();
            assert_eq!(runtime, expected, "{cost:?}: the real resolution");
            assert_eq!(hypothetical, runtime, "{cost:?}: hypothetical == real");
        }
    }

    // ---- U5: the filter-context class ----------------------------------------

    /// U-b (CR 106.1 + CR 109.5): Mox Amber's colors among your legendary
    /// permanents, read in the resolving ability's context, match the
    /// source-anchored enumeration for a filter that reads no binding.
    #[test]
    fn colors_among_permanents_match_the_enumerator_for_a_state_filter() {
        let mut scenario = scenario();
        let mox = scenario
            .add_artifact_from_oracle(P0, "Mox Amber", MOX_AMBER)
            .id();
        scenario
            .add_creature(P0, "Legend", 2, 2)
            .as_legendary()
            .with_color(vec![ManaColor::Red]);
        let runner = flushed(scenario.build());
        let production = match &*runner.state().objects[&mox].abilities[0].effect {
            Effect::Mana { produced, .. } => produced.clone(),
            other => panic!("reach-guard: Mox Amber adds mana, got {other:?}"),
        };
        let ManaProduction::AnyOneColorAmongPermanents { filter, .. } = &production else {
            panic!("reach-guard: Mox Amber reads colors among permanents, got {production:?}");
        };
        assert!(!filter_binding_diverges(
            filter,
            BindingReader::HypotheticalResolution(HypotheticalBindings::NONE)
        ));
        assert_eq!(could_produce(runner.state(), mox), vec![ManaType::Red]);
        assert_eq!(
            could_produce(runner.state(), mox),
            mana_options_from_production(runner.state(), P0, mox, &production)
        );
    }

    /// U-b′ (CR 106.7): a filter the hypothetical node cannot decide keeps the
    /// source-anchored enumeration. The node is unbound, so both readings
    /// coincide here; U-b″ is the row that discriminates the guard.
    #[test]
    fn colors_among_permanents_fall_back_for_an_undecidable_filter() {
        let shares_a_name_with_the_target =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                crate::types::ability::FilterProp::SharesQuality {
                    quality: crate::types::ability::SharedQuality::Name,
                    reference: Some(Box::new(TargetFilter::ParentTarget)),
                    relation: Default::default(),
                },
            ]));
        assert!(filter_binding_diverges(
            &shares_a_name_with_the_target,
            BindingReader::HypotheticalResolution(HypotheticalBindings::NONE)
        ));
        let production = ManaProduction::DistinctColorsAmongPermanents {
            filter: shares_a_name_with_the_target,
        };
        let mut state = main_phase_state();
        let land = land_with(&mut state, tap_mana_ability(production.clone()));
        for color in [ManaColor::Green, ManaColor::Red] {
            let creature = add_object(
                &mut state,
                P0_ID,
                "Bear",
                Zone::Battlefield,
                CoreType::Creature,
                vec![],
            );
            state.objects.get_mut(&creature).unwrap().color = vec![color];
        }
        assert_eq!(
            could_produce(&state, land),
            mana_options_from_production(&state, P0_ID, land, &production)
        );
    }

    /// U-b″, a typed-contract row (CR 106.7 + CR 608.2c): a filter that still
    /// diverges on a BOUND node keeps the source-anchored enumeration.
    ///
    /// No printed card reaches this shape. Omnath's look and reveal hand the
    /// public top card (Doomsday) to the mana node, which reads "colors among
    /// creatures named like the parent target". Bound, that is the Red
    /// creature named Doomsday; source-anchored, it is nothing. The classifier
    /// keeps `SameNameAsParentTarget` diverging under every reader, so the
    /// walker must answer with the enumeration, not the bound reading.
    #[test]
    fn colors_among_permanents_fall_back_for_a_filter_diverging_on_a_bound_node() {
        let (mut runner, omnath, library) = public_library_board(vec![("Doomsday", doomsday())]);
        let nodes = omnath_nodes(&runner, omnath);
        let named_like_the_target = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            crate::types::ability::FilterProp::SameNameAsParentTarget,
        ]));
        let production = ManaProduction::DistinctColorsAmongPermanents {
            filter: named_like_the_target.clone(),
        };
        let bound = HypotheticalBindings {
            targets: HypotheticalTargets::EstablishedByEarlierInstruction,
            ..HypotheticalBindings::NONE
        };
        assert!(
            filter_binding_diverges(
                &named_like_the_target,
                BindingReader::HypotheticalResolution(bound)
            ),
            "reach-guard: the filter diverges even on a bound node"
        );
        let namesake = add_object(
            runner.state_mut(),
            P0_ID,
            "Doomsday",
            Zone::Battlefield,
            CoreType::Creature,
            vec![],
        );
        runner.state_mut().objects.get_mut(&namesake).unwrap().color = vec![ManaColor::Red];

        // Control: the same chain shape binds its mana node to the top card.
        let target_colors = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            target_colors_mana(),
        ]);
        let control_land = land_with_trigger(runner.state_mut(), target_colors);
        assert_eq!(
            could_produce(runner.state(), control_land),
            vec![ManaType::Black],
            "reach-guard: the mana node of this chain is bound to the top card"
        );

        let execute = chain(vec![
            nodes.dig.clone(),
            nodes.reveal.clone(),
            AbilityDefinition::new(AbilityKind::Spell, mana(production.clone())),
        ]);
        let mut reveal = build_resolved_from_def(&execute, omnath, P0_ID)
            .sub_ability
            .map(|reveal| *reveal)
            .unwrap();
        reveal.set_unpinned_targets(vec![TargetRef::Object(library[0])]);
        let mut bound_mana = reveal.sub_ability.as_deref().cloned().unwrap();
        assert_eq!(
            inherited_parent_occurrences(&reveal, &bound_mana),
            vec![(TargetRef::Object(library[0]), None)],
            "reach-guard: the mana node inherits the revealed top card"
        );
        bound_mana.set_unpinned_targets(vec![TargetRef::Object(library[0])]);

        let land = land_with_trigger(runner.state_mut(), execute);
        let state = runner.state();
        let source_anchored = mana_options_from_production(state, P0_ID, land, &production);
        assert_eq!(
            distinct_colors_among_permanents(
                state,
                Some(&bound_mana),
                land,
                &named_like_the_target
            ),
            vec![ManaColor::Red],
            "reach-guard: the bound reading names the Red namesake"
        );
        assert!(
            source_anchored.is_empty(),
            "reach-guard: the source-anchored reading names nothing"
        );
        assert_eq!(could_produce(state, land), source_anchored);
    }
}
