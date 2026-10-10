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
//!   resolution would fix (a target, X, the cost referent, its trigger event, a
//!   ledger it publishes) is classified by `triggers::gate_binding_diverges` /
//!   `quantity_expr_binding_diverges` under
//!   [`BindingReader::HypotheticalResolution`] and read both ways.
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

use std::collections::{HashMap, VecDeque};
use std::ops::ControlFlow;

use super::ability_utils::{apply_instead_swap, build_resolved_from_def};
use super::effects::{
    evaluate_condition, false_gate_escape, instead_declined_continuation, instead_swap_applies,
    is_instead_override, InsteadDeclined,
};
use super::filter::{matches_target_filter, FilterContext};
use super::game_object::GameObject;
use super::mana_abilities::{
    apply_condition_instead_mana_swap, condition_instead_mana_branch, is_mana_ability,
    sacrifice_cost_choice,
};
use super::mana_sources::{has_tap_component, mana_options_from_production};
use super::replacement::{hypothetical_produced_mana_types, produced_mana_may_be_replaced};
use super::triggers::{
    build_triggered_ability_from_context, check_trigger_condition_with_source,
    gate_binding_diverges, quantity_expr_binding_diverges, trigger_source_context_for_latch,
    BindingReader,
};
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
    ResolvedAbility, TargetFilter, TriggerDefinitionRef,
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
    /// An option set independent of other permanents, read through
    /// `mana_sources::mana_options_from_production`.
    Direct(ManaProduction),
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
                ProductionReading::Direct(produced) => {
                    let options = mana_options_from_production(
                        state,
                        production.controller,
                        object_id,
                        &produced,
                    );
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
                // The runtime activation: build, then stamp the printed index.
                // X stays unbound — an activation choice.
                let mut resolved = build_resolved_from_def(root, object.id, object.controller);
                resolved.ability_index = Some(ability_index);
                walk_chain(state, &resolved, definition, &mut productions);
            }
        }
    }
    read_triggered_abilities(state, object, &mut productions);
    productions
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
    let branches = match unswapped.sub_ability.as_deref() {
        // CR 608.2c + CR 106.7: a wrapped condition the resolution itself fixes
        // could go either way, so both branches are read.
        Some(sub)
            if matches!(
                sub.condition.as_ref(),
                Some(AbilityCondition::ConditionInstead { inner })
                    if gate_binding_diverges(inner, BindingReader::HypotheticalResolution)
            ) =>
        {
            vec![
                condition_instead_mana_branch(&unswapped, sub, true),
                condition_instead_mana_branch(&unswapped, sub, false),
            ]
        }
        _ => vec![apply_condition_instead_mana_swap(state, &unswapped)],
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
            push_production(state, branch, effect, tap_state, definition, productions);
            ControlFlow::Continue(())
        });
        // CR 605.3b: the rest of the chain resolves through
        // `resolve_mana_ability_sub_chain` → `effects::resolve_ability_chain`.
        if let Some(sub) = branch.sub_ability.as_deref() {
            walk_chain(state, sub, definition, productions);
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
        walk_chain(state, &resolved, execute, productions);
    }
}

/// CR 608.2c: the chain the resolver would run from `node`, read with the
/// resolver's own decisions. `cost_owner` is the definition whose cost a
/// cost-referent production ranges over: the activated ability's own
/// definition, or a triggered ability's execute definition.
///
/// Step 1, the "instead" swap (CR 608.2c + CR 614.1a + CR 614.15): membership
/// is `effects::is_instead_override`, the decision `effects::instead_swap_applies`
/// and the swap `ability_utils::apply_instead_swap` — the functions
/// `resolve_ability_chain` uses. A swap whose gate the resolution itself fixes
/// is read both ways.
fn walk_chain(
    state: &GameState,
    node: &ResolvedAbility,
    cost_owner: &AbilityDefinition,
    productions: &mut Vec<HypotheticalProduction>,
) {
    let instead = node
        .sub_ability
        .as_deref()
        .filter(|sub| is_instead_override(sub));
    let Some(sub) = instead else {
        walk_performed(state, node, cost_owner, productions);
        return;
    };
    let swap_is_resolution_bound = sub.condition.as_ref().is_some_and(|condition| {
        gate_binding_diverges(condition, BindingReader::HypotheticalResolution)
    });
    if swap_is_resolution_bound {
        walk_performed(
            state,
            &apply_instead_swap(node, sub),
            cost_owner,
            productions,
        );
        walk_performed(state, node, cost_owner, productions);
    } else if instead_swap_applies(state, node, sub) {
        walk_performed(
            state,
            &apply_instead_swap(node, sub),
            cost_owner,
            productions,
        );
    } else {
        walk_performed(state, node, cost_owner, productions);
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
/// included (declining is a choice). Step 4, the next link: a not-swapped
/// "instead" sub continues as `instead_declined_continuation` selects, anything
/// else as itself.
fn walk_performed(
    state: &GameState,
    node: &ResolvedAbility,
    cost_owner: &AbilityDefinition,
    productions: &mut Vec<HypotheticalProduction>,
) {
    if let Some(condition) = &node.condition {
        if condition.has_when_you_do_marker() {
            return;
        }
        if gate_binding_diverges(condition, BindingReader::HypotheticalResolution) {
            walk_false_gate_path(state, node, cost_owner, productions);
        } else if !evaluate_condition(condition, state, node) {
            walk_false_gate_path(state, node, cost_owner, productions);
            return;
        }
    }

    let _ = visit_own_resolution_effects(&node.effect, &mut |effect| {
        push_production(
            state,
            node,
            effect,
            ManaTapState::NotFromTap,
            cost_owner,
            productions,
        );
        ControlFlow::Continue(())
    });

    let Some(sub) = node.sub_ability.as_deref() else {
        return;
    };
    if !is_instead_override(sub) {
        walk_chain(state, sub, cost_owner, productions);
        return;
    }
    match instead_declined_continuation(sub) {
        Some(InsteadDeclined::Else(continuation) | InsteadDeclined::Tail(continuation)) => {
            walk_chain(state, continuation, cost_owner, productions)
        }
        None => {}
    }
}

/// CR 608.2c: what the resolver runs after `node`'s gate was false — its
/// `else_ability`, else a sub that `effects::false_gate_escape` lets through.
fn walk_false_gate_path(
    state: &GameState,
    node: &ResolvedAbility,
    cost_owner: &AbilityDefinition,
    productions: &mut Vec<HypotheticalProduction>,
) {
    if let Some(else_ability) = node.else_ability.as_deref() {
        walk_chain(state, else_ability, cost_owner, productions);
        return;
    }
    if let Some(sub) = node
        .sub_ability
        .as_deref()
        .filter(|sub| false_gate_escape(sub).is_some())
    {
        walk_chain(state, sub, cost_owner, productions);
    }
}

/// Records `effect` when it is a mana instruction that would add mana, read for
/// `node`, the hypothetical resolving node that carries it (anchored to the
/// surveyed permanent's own controller and id, CR 109.5).
fn push_production(
    state: &GameState,
    node: &ResolvedAbility,
    effect: &Effect,
    tap_state: ManaTapState,
    cost_owner: &AbilityDefinition,
    productions: &mut Vec<HypotheticalProduction>,
) {
    let Effect::Mana { produced, .. } = effect else {
        return;
    };
    let Some(amount) = instruction_amount(state, produced, node) else {
        return;
    };
    productions.push(HypotheticalProduction {
        reading: production_reading(state, produced, node, cost_owner),
        tap_state,
        amount,
        controller: node.controller,
    });
}

/// CR 106.5 + CR 106.7: how much mana an instruction would add, or `None` when
/// it would add none — such an instruction defines no type.
///
/// A count the ability's own activation or resolution fixes — X (CR 107.3a), a
/// storage land's removed counters, the cost referent, a target, an anaphor, an
/// event amount, a resolution ledger (`triggers::quantity_expr_binding_diverges`
/// under `BindingReader::HypotheticalResolution`) — is admitted as 1: CR 106.7
/// ignores whether costs could be paid, and the choices made during the
/// resolution are the player's to make. Any other count is state at that time,
/// resolved against `node` (Gaea's Cradle counts its controller's creatures).
/// A production with no count carries its amount in its option list.
fn instruction_amount(
    state: &GameState,
    produced: &ManaProduction,
    node: &ResolvedAbility,
) -> Option<u32> {
    let mut amount = Some(1);
    produced.for_each_quantity_expr(&mut |count: &QuantityExpr| {
        if quantity_expr_binding_diverges(count, BindingReader::HypotheticalResolution) {
            return;
        }
        let resolved = super::quantity::resolve_quantity_with_targets(state, count, node);
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
/// production is read on its own. Exhaustive, so a new referential variant
/// cannot slip into `mana_options_from_production` inside a solve.
fn production_reading(
    state: &GameState,
    produced: &ManaProduction,
    node: &ResolvedAbility,
    cost_owner: &AbilityDefinition,
) -> ProductionReading {
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
                    ability: cost_owner,
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
        ManaProduction::Fixed { .. }
        | ManaProduction::Colorless { .. }
        | ManaProduction::AnyOneColor { .. }
        | ManaProduction::AnyCombination { .. }
        | ManaProduction::ChosenColor { .. }
        | ManaProduction::NotedType { .. }
        | ManaProduction::AnyCombinationOfObjectColors { .. }
        | ManaProduction::AnyInCommandersColorIdentity { .. }
        | ManaProduction::AnyOneColorAmongPermanents { .. }
        | ManaProduction::Mixed { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::ChoiceAmongCombinations { .. }
        | ManaProduction::DistinctColorsAmongPermanents { .. }
        | ManaProduction::TriggerEventManaType => ProductionReading::Direct(produced.clone()),
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

    /// U-f (viii) (CR 608.2c): an ordinal gate on the ability's own resolution
    /// count is read both ways whatever the ledger says, because the resolution
    /// that asks it bumps the ledger first.
    #[test]
    fn an_ordinal_gate_is_read_whatever_the_ledger_says() {
        let ordinal_green = |gated: bool| {
            let mut sub = green_sub();
            if gated {
                sub = sub.condition(AbilityCondition::AbilityUseCountThisTurn {
                    tally: AbilityUseTally::Resolved,
                    comparator: Comparator::EQ,
                    n: 3,
                });
            }
            sub.sub_link = SubAbilityLink::SequentialSibling;
            sub.sibling_condition = SiblingCondition::Dependent;
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Mana {
                cost: crate::types::mana::ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 0,
                },
            })
            .sub_ability(sub)
        };

        let mut state = main_phase_state();
        let ungated = land_with(&mut state, ordinal_green(false));
        assert_eq!(
            could_produce(&state, ungated),
            vec![ManaType::Green],
            "reach-guard: the sub is reached"
        );

        let mut state = main_phase_state();
        let land = land_with(&mut state, ordinal_green(true));
        assert_eq!(could_produce(&state, land), vec![ManaType::Green]);
        for resolutions in [2, 3] {
            state
                .ability_resolutions_this_turn
                .insert((land, 0), resolutions);
            assert_eq!(
                state.ability_resolutions_this_turn.get(&(land, 0)).copied(),
                Some(resolutions),
                "reach-guard: the ledger reads the stamped count"
            );
            assert_eq!(could_produce(&state, land), vec![ManaType::Green]);
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
}
