#[cfg(test)]
use crate::types::ability::TapStateChange;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CardTypeSetSource, CastManaSpentMetric,
    CombatRelationSubject, ControllerRef, CounterMoveSelection, Effect, EffectScope, FilterProp,
    GameRestriction, ModalChoice, ModalSelectionCondition, ModalSelectionConstraint,
    MultiTargetSpec, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility,
    RestrictionPlayerScope, SpellContext, SubAbilityLink, TargetChoiceTiming, TargetFilter,
    TargetRef, TypeFilter, TypedFilter,
};
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::game_state::{
    GameState, TargetSelectionConstraint, TargetSelectionProgress, TargetSelectionSlot,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::EngineError;
use super::players;
use super::quantity::resolve_quantity_with_targets;
use super::targeting;
use super::triggers;

fn move_counter_stack_target_filters<'a>(
    source: &'a TargetFilter,
    target: &'a TargetFilter,
    selection: CounterMoveSelection,
) -> Vec<&'a TargetFilter> {
    match selection {
        CounterMoveSelection::StackTarget | CounterMoveSelection::StackTargetAnyNumber => {
            vec![source, target]
        }
        CounterMoveSelection::ResolutionDistributionAnyNumber => vec![source],
    }
}

/// CR 113.1a: Build a resolved ability from its definition, preserving sub-ability chains,
/// conditions, durations, and targeting configuration.
pub fn build_resolved_from_def(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    build_resolved_from_def_with_targets(def, source_id, controller, Vec::new())
}

/// CR 113.1a + CR 608.2c: Build a resolved ability from its definition while
/// supplying the already selected root targets. Sub-abilities intentionally
/// start without targets so `resolve_ability_chain` can apply the standard
/// parent-target propagation rules.
pub fn build_resolved_from_def_with_targets(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
    targets: Vec<TargetRef>,
) -> ResolvedAbility {
    let mut resolved =
        ResolvedAbility::new(*def.effect.clone(), targets, source_id, controller).kind(def.kind);
    resolved.context.ability_tag = def.ability_tag;
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(build_resolved_from_def(sub, source_id, controller));
    }
    if let Some(else_ab) = &def.else_ability {
        resolved.else_ability = Some(Box::new(build_resolved_from_def(
            else_ab, source_id, controller,
        )));
    }
    if let Some(duration) = def.duration.clone() {
        resolved = resolved.duration(duration);
    }
    if let Some(condition) = def.condition.clone() {
        resolved = resolved.condition(condition);
    }
    resolved.optional_targeting = def.optional_targeting;
    resolved.optional = def.optional;
    resolved.optional_for = def.optional_for;
    resolved.multi_target = def.multi_target.clone();
    // CR 115.1 + CR 601.2c: Carry the target-set constraints (e.g. combined
    // mana-value cap) through so the resolution-time validator can enforce them
    // against the announced/selected targets. Without this copy the parsed
    // `AbilityDefinition.target_constraints` never reaches the resolved sub and
    // the validator reads an empty constraint list.
    resolved.target_constraints = def.target_constraints.clone();
    resolved.target_choice_timing = def.target_choice_timing;
    resolved.repeat_for = def.repeat_for.clone();
    // CR 608.2c + CR 107.1c: Carry the loop-continuation predicate through so the
    // `repeat_until` dispatch in `resolve_ability_chain` can re-follow the chain.
    resolved.repeat_until = def.repeat_until.clone();
    resolved.min_x_value = def.min_x_value;
    resolved.cant_be_copied = def.cant_be_copied;
    resolved.description = def.description.clone();
    resolved.forward_result = def.forward_result;
    resolved.unless_pay = def.unless_pay.clone();
    resolved.player_scope = def.player_scope.clone();
    // CR 101.4 + CR 800.4: Propagate the turn-order override for `player_scope`
    // iteration. The iteration driver in `effects/mod.rs` reads this and calls
    // `players::apnap_order_from(state, starting_with, controller)` so Join
    // Forces ("Starting with you, each player may pay any amount of mana")
    // prompts the controller first regardless of whose turn it is.
    resolved.starting_with = def.starting_with.clone();
    // CR 115.1 + CR 701.9b: Carry the parser-stamped target selection mode
    // through to the resolved ability so target-selection sites can short-circuit
    // `WaitingFor::TargetSelection` for `Random` abilities.
    resolved.target_selection_mode = def.target_selection_mode;
    // CR 601.2c + CR 603.3d: Carry the parser-stamped target chooser through so the
    // trigger target-selection site can route a targeted "of their choice" to the
    // scoped (upkeep) player instead of the source's controller.
    resolved.target_chooser = def.target_chooser.clone();
    // CR 608.2c: Carry the parent-link kind through so the decline classifier can
    // distinguish a separate-sentence sibling from a within-clause continuation.
    resolved.sub_link = def.sub_link;
    // CR 700.2b + CR 603.3c: Carry the reflexive modal choice + per-mode abilities
    // through so try_begin_reflexive_target_selection can route a gated modal
    // trigger (Caesar) to AbilityModeChoice instead of resolving the modes
    // unconditionally.
    resolved.modal = def.modal.clone();
    resolved.mode_abilities = def.mode_abilities.clone();
    resolved
}

/// CR 608.2c + CR 608.2e: Apply an "instead" swap from a sub-ability override
/// onto a parent `ResolvedAbility`. Produces a new `ResolvedAbility` whose
/// **identity / runtime context** comes from the parent (controller, source,
/// already-announced targets, kicker context, chosen-X, etc.) but whose
/// **effect-shape fields** come from the sub (effect, player_scope, optional,
/// description, repeat_for, …).
///
/// This is the single authority for instead-swap semantics. Adding a sibling
/// instead-shape (kicker / target-keyword / condition-instead) goes through
/// here so no field is silently dropped on the swap. Mirrors the lesson from
/// commit `4475b1939` where partial clones on the casting path silently
/// dropped `player_scope`.
///
/// Fields from `sub`: effect, duration, sub_ability, else_ability,
/// player_scope, optional, optional_for, optional_targeting, multi_target,
/// target_constraints, target_choice_timing, description, repeat_for,
/// min_x_value, forward_result, unless_pay, distribution, target_selection_mode.
///
/// Fields preserved from `parent`: controller, source_id, kind, context,
/// original_controller, scoped_player, targets, chosen_x, cost_paid_object,
/// ability_index, may_trigger_origin.
///
/// `condition` is intentionally **cleared** — the override sub's own
/// `ConditionInstead { inner }` (or AdditionalCostPaidInstead, etc.) has
/// already been evaluated by the caller; the inner condition encodes all
/// resolution checks (CR 608.2c).
pub(crate) fn apply_instead_swap(
    parent: &ResolvedAbility,
    sub: &ResolvedAbility,
) -> ResolvedAbility {
    let mut overridden = parent.clone();
    overridden.effect = sub.effect.clone();
    overridden.duration = sub.duration.clone();
    // CR 608.2c: The override sub is consumed; its own sub_ability becomes the
    // new chain tail. The else_ability mirrors that chain.
    overridden.sub_ability = sub.sub_ability.clone();
    overridden.else_ability = sub.else_ability.clone();
    // CR 608.2c: "Instead" semantics replace the entire effect clause. The
    // ConditionInstead inner condition already encodes all resolution checks
    // (e.g., Revolt + MV ≤ 4 via And). The parent's base condition (e.g.,
    // MV ≤ 2) is superseded — it only applies when the swap does NOT fire.
    overridden.condition = None;
    // CR 608.2 + CR 608.2c: Effect-shape fields belong to the swapped effect,
    // not the parent.
    overridden.player_scope = sub.player_scope.clone();
    // CR 101.4 + CR 800.4: The turn-order override is an effect-shape attribute
    // (which iteration order the scoped effect uses), so it follows the swap.
    overridden.starting_with = sub.starting_with.clone();
    overridden.optional = sub.optional;
    overridden.optional_for = sub.optional_for;
    overridden.optional_targeting = sub.optional_targeting;
    overridden.multi_target = sub.multi_target.clone();
    // CR 115.1 + CR 601.2c: Target-set constraints are an effect-shape attribute
    // of the swapped clause, so they follow the swap (no field silently dropped).
    overridden.target_constraints = sub.target_constraints.clone();
    overridden.target_choice_timing = sub.target_choice_timing;
    overridden.description = sub.description.clone();
    overridden.repeat_for = sub.repeat_for.clone();
    overridden.min_x_value = sub.min_x_value;
    overridden.forward_result = sub.forward_result;
    overridden.unless_pay = sub.unless_pay.clone();
    overridden.distribution = sub.distribution.clone();
    overridden.target_selection_mode = sub.target_selection_mode;
    overridden.target_chooser = sub.target_chooser.clone();
    overridden
}

/// CR 700.2: For modal spells/abilities, build a chained resolved ability from the
/// selected mode indices, linking them via the sub_ability chain.
///
/// CR 608.2c: "The controller of the spell or ability follows its instructions
/// in the order written." For modes chosen from a "Choose one or more —" /
/// "Choose up to N —" list, the printed (source) order is the ascending
/// ordering of the mode indices — independent of the order the player
/// announced them in. We sort the input indices here so the resulting
/// sub_ability chain always resolves in printed order. Duplicate indices are
/// preserved (CR 700.2d: "You may choose the same mode more than once"
/// repeats the mode in sequence).
pub fn build_chained_resolved(
    abilities: &[AbilityDefinition],
    indices: &[usize],
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<ResolvedAbility, EngineError> {
    if indices.is_empty() {
        // CR 700.2a: "Choose up to one" permits choosing no modes. The ability
        // still resolves, but it has no instructions to perform.
        return Ok(ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: Vec::new(),
                duration: None,
                target: None,
            },
            Vec::new(),
            source_id,
            controller,
        ));
    }

    let mut ordered: Vec<usize> = indices.to_vec();
    ordered.sort();

    let mut result: Option<ResolvedAbility> = None;
    for &idx in ordered.iter().rev() {
        let def = abilities
            .get(idx)
            .ok_or_else(|| EngineError::InvalidAction(format!("Mode index {idx} out of range")))?;
        let mut resolved = build_resolved_from_def(def, source_id, controller);
        // CR 700.2d: When chaining multiple modes, append subsequent modes after
        // the current mode's own sub_ability chain (e.g., Cathartic Pyre mode 2's
        // "discard, then draw that many" must preserve the draw sub_ability).
        if let Some(mut next_mode) = result {
            // CR 700.2d + CR 700.2f + CR 608.2c: chained modes are independent
            // instructions, not continuations. Tag the appended mode root as a
            // `SequentialSibling` so resolution treats it as its own instruction
            // (e.g. Dromoka's Command mode 3's `PutCounter` must resolve on its
            // own target, NOT as a rider of mode 1's prevention shield). Within a
            // single mode, then/comma sub-steps remain `ContinuationStep`.
            next_mode.sub_link = SubAbilityLink::SequentialSibling;
            append_to_sub_chain(&mut resolved, next_mode);
        }
        result = Some(resolved);
    }

    result.ok_or_else(|| EngineError::InvalidAction("No modes selected".to_string()))
}

/// Append `next` to the tail of `ability`'s sub_ability chain.
pub(crate) fn append_to_sub_chain(ability: &mut ResolvedAbility, next: ResolvedAbility) {
    let mut node = ability;
    while node.sub_ability.is_some() {
        node = node.sub_ability.as_mut().unwrap().as_mut();
    }
    node.sub_ability = Some(Box::new(next));
}

pub fn find_first_target_filter_in_chain(ability: &ResolvedAbility) -> Option<&TargetFilter> {
    if ability.target_choice_timing == TargetChoiceTiming::Stack {
        if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
            return Some(filter);
        }
    }
    ability
        .sub_ability
        .as_deref()
        .and_then(find_first_target_filter_in_chain)
}

/// CR 700.2 / CR 601.2b: Accumulates target slots alongside their per-slot mode
/// display labels while walking an ability chain. The single `push` entry point
/// enforces the `labels[i]` ↔ `slots[i]` invariant: every slot pushed during a
/// given mode's collection inherits that mode's `current_label`. Non-modal
/// collection leaves `current_label` `None`, so `labels` ends up all-`None`
/// (callers that don't need labels read `slots` and discard `labels`).
#[derive(Default)]
struct SlotAccumulator {
    slots: Vec<TargetSelectionSlot>,
    labels: Vec<Option<String>>,
    /// Mode label applied to every slot pushed until reset. Set by
    /// `build_target_slots_labelled` before collecting each mode; `None` for
    /// non-modal collection.
    current_label: Option<String>,
}

impl SlotAccumulator {
    /// Push a slot and its mode label together. The label is `current_label` at
    /// push time, keeping `labels` and `slots` index-parallel by construction.
    fn push(&mut self, slot: TargetSelectionSlot) {
        self.slots.push(slot);
        self.labels.push(self.current_label.clone());
    }
}

/// CR 601.2c / CR 602.2b: Collect all target slots for an ability chain. Each targeting
/// effect in the chain produces a slot whose legal targets are computed from the game state.
pub fn build_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<Vec<TargetSelectionSlot>, EngineError> {
    let mut acc = SlotAccumulator::default();
    collect_target_slots(state, ability, &mut acc)?;
    Ok(acc.slots)
}

/// CR 700.2 / CR 601.2b + CR 700.2c: Build target slots for a modal spell/ability
/// along with a per-slot mode display label, so the targeting UI can show which
/// mode the current target belongs to (CR 700.2). The label for `slots[i]` is
/// `labels[i]`; both vectors are the same length by construction.
///
/// Each chosen mode's slots are collected from its OWN resolved ability built
/// directly via `build_resolved_from_def` (rather than from the combined
/// `build_chained_resolved` chain) so each mode can be tagged independently. A
/// single shared accumulator is threaded across all modes so cross-slot
/// `existing_slots` relative-controller binding (CR 109.4) still sees earlier
/// modes' slots.
///
/// The resulting slots are slot-for-slot identical (order and count) to the
/// whole-chain `build_target_slots(&build_chained_resolved(...))` pass for every
/// current card, which the resolver relies on because it consumes the COMBINED
/// chain and maps selected targets back by slot index. There are two
/// unreachable-today divergences:
///   1. `Effect::ExchangeControl` head modes: `collect_target_slots` returns
///      unconditionally after an ExchangeControl effect without descending into
///      the sub-chain, so the whole-chain pass silently truncates any later
///      modes appended after such a mode. Collecting each mode from its own
///      resolved ability is strictly more correct there — every chosen mode
///      contributes its slots regardless of position. (0 cards.)
///   2. A deferred-effect-head mode (Scry/Dig/Surveil/Choose/ChooseCard/
///      SearchLibrary/RevealHand) immediately followed in sorted order by a
///      targeting skip-stack mode (ChangeZone/Shuffle/PutAtLibraryPosition): the
///      whole-chain pass routes the following mode through
///      `collect_target_slots_after_deferred_effect` (applying
///      `skips_stack_targets_after_deferred_effect`), but this per-mode build
///      collects it via plain `collect_target_slots`, so it may surface one
///      extra slot. (0 cards.)
///
/// A `debug_assert_eq!` below catches either case loudly should a future card
/// ever reach it.
///
/// Indices are sorted (printed order, CR 608.2c) to match
/// `build_chained_resolved`; duplicate indices (CR 700.2d) repeat the mode.
// CR 700.2 + CR 601.2b/c: Each parameter encodes a distinct, irreducible piece
// of the modal slot-build context (game state, ability definitions, chosen mode
// indices, per-mode display text, source identity, controller, spell context,
// announced X). Grouping any pair would either fabricate a transient struct
// with no other use site or hide a real semantic axis (e.g. `chosen_x` is
// timing-dependent — `None` before the X round-trip, `Some(x)` after — and
// must remain visible at every call site).
#[allow(clippy::too_many_arguments)]
pub fn build_target_slots_labelled(
    state: &GameState,
    abilities: &[AbilityDefinition],
    indices: &[usize],
    mode_descriptions: &[String],
    source_id: ObjectId,
    controller: PlayerId,
    context: &SpellContext,
    // CR 107.1b + CR 700.2: When the slot build runs AFTER the X round-trip
    // (deferred target selection — see `casting_costs::begin_deferred_target_selection`),
    // each freshly-built per-mode resolved ability needs the chosen X value
    // propagated so target legality filters referencing `X` (e.g. Kozilek's
    // Command mode 2: "mana value X or less") resolve against the announced
    // value rather than the default `0`. `None` for callers that build slots
    // BEFORE X is chosen (the common non-deferred modal path).
    chosen_x: Option<u32>,
) -> Result<(Vec<TargetSelectionSlot>, Vec<Option<String>>), EngineError> {
    let mut ordered: Vec<usize> = indices.to_vec();
    ordered.sort();

    let mut acc = SlotAccumulator::default();
    for idx in ordered {
        let def = abilities
            .get(idx)
            .ok_or_else(|| EngineError::InvalidAction(format!("Mode index {idx} out of range")))?;
        let mut resolved = build_resolved_from_def(def, source_id, controller);
        resolved.set_context_recursive(context.clone());
        if let Some(x) = chosen_x {
            resolved.set_chosen_x_recursive(x);
        }
        acc.current_label = mode_descriptions.get(idx).cloned();
        collect_target_slots(state, &resolved, &mut acc)?;
        acc.current_label = None;
    }

    // CR 700.2c: The resolver consumes the COMBINED chain and maps selected
    // targets back by slot index, so this per-mode slot count MUST equal the
    // whole-chain `build_target_slots(&build_chained_resolved(...))` count. The
    // two documented divergences (ExchangeControl head; deferred-effect head
    // followed by a skip-stack mode) are unreachable today; this detection-only
    // assert makes any future card that reaches them fail loudly in test/debug
    // builds rather than surfacing an extra slot at runtime. Confined to
    // debug_assertions so release builds don't pay the double-build cost, and
    // any Err from the comparison build is swallowed so it can never change
    // release-observable behavior (the returned slots/labels are unaffected).
    #[cfg(debug_assertions)]
    {
        if let Ok(mut combined) = build_chained_resolved(abilities, indices, source_id, controller)
        {
            combined.set_context_recursive(context.clone());
            if let Some(x) = chosen_x {
                combined.set_chosen_x_recursive(x);
            }
            if let Ok(combined_slots) = build_target_slots(state, &combined) {
                debug_assert_eq!(
                    acc.slots.len(),
                    combined_slots.len(),
                    "build_target_slots_labelled slot count diverged from whole-chain build — a modal mode combination (ExchangeControl, or deferred-effect + skip-stack) is now reachable; see CR 700.2 slot-mapping invariant"
                );
            }
        }
    }

    Ok((acc.slots, acc.labels))
}

/// CR 109.4 + CR 608.2c: Resolve the controller of an ability's first parent target.
///
/// This is the canonical lookup for `ControllerRef::ParentTargetController` and
/// `TargetFilter::ParentTargetController` — used by sub-effects whose subject is
/// "its controller" / "that creature's controller" relative to a previously
/// chosen target. Returns the player target directly, or the controller of an
/// object target (CR 109.4 — controller of an object), in target-list order.
/// Returns `None` if the ability has no targets.
pub fn parent_target_controller(ability: &ResolvedAbility, state: &GameState) -> Option<PlayerId> {
    if let Some(player) = ability.targets.iter().find_map(|t| match t {
        // CR 608.2h (issue #1582): If the parent target has left the
        // battlefield — e.g. a token Recoil bounced to hand, which then ceases
        // to exist per CR 704.5d before the chained "that player discards"
        // resolves — fall back to last-known information so the player anaphor
        // still resolves.
        TargetRef::Object(id) => state
            .stack
            .iter()
            .find(|entry| entry.id == *id || entry.source_id == *id)
            .map(|entry| entry.controller)
            .or_else(|| {
                state
                    .objects
                    .get(id)
                    .map(|obj| obj.controller)
                    .or_else(|| state.lki_cache.get(id).map(|lki| lki.controller))
            }),
        TargetRef::Player(pid) => Some(*pid),
    }) {
        return Some(player);
    }

    // CR 608.2c + CR 608.2h + CR 400.7j (issue #2890): A chained instruction
    // may inherit the parent effect's singular referent only through
    // `effect_context_object` — e.g. Reality Shift's manifest after the
    // exiled creature left the battlefield and parent targets were not copied
    // onto the sub-ability. The propagated snapshot carries the at-departure
    // controller per CR 608.2h.
    ability
        .effect_context_object
        .as_ref()
        .map(|snapshot| snapshot.lki.controller)
}

/// CR 108.3 + CR 608.2c: Resolve the owner of an ability's first parent target.
///
/// Mirrors `parent_target_controller` but returns the *owner* of an object target
/// per CR 108.3 (owner is the player who started the game with the card in their
/// deck). Used by `TargetFilter::ParentTargetOwner` for "its owner" anaphors —
/// e.g., Enslave's "enchanted creature deals 1 damage to its owner" once a
/// parent-target slot has been bound. Falls back to last-known information (CR
/// 608.2h) when the object has ceased to exist. Returns `None` only if the
/// ability has no targets, or an object target is absent from both the live
/// object map and the LKI cache.
pub fn parent_target_owner(ability: &ResolvedAbility, state: &GameState) -> Option<PlayerId> {
    if let Some(player) = ability.targets.iter().find_map(|t| match t {
        // CR 608.2h (issue #1582): Mirror the controller lookup — fall back to
        // last-known information so "its owner" still resolves after the
        // referenced object (e.g. a bounced token) has ceased to exist.
        TargetRef::Object(id) => state
            .objects
            .get(id)
            .map(|obj| obj.owner)
            .or_else(|| state.lki_cache.get(id).map(|lki| lki.owner)),
        TargetRef::Player(_) => None,
    }) {
        return Some(player);
    }

    // CR 608.2c + CR 400.7j: Mirror the controller fallback for owner anaphors.
    ability
        .effect_context_object
        .as_ref()
        .map(|snapshot| snapshot.lki.owner)
}

pub fn target_constraints_from_modal(modal: &ModalChoice) -> Vec<TargetSelectionConstraint> {
    modal
        .constraints
        .iter()
        .filter_map(|constraint| match constraint {
            ModalSelectionConstraint::DifferentTargetPlayers => {
                Some(TargetSelectionConstraint::DifferentTargetPlayers)
            }
            // ConditionalMaxChoices/NoRepeatThisTurn/NoRepeatThisGame are mode-selection
            // constraints, not target constraints.
            _ => None,
        })
        .collect()
}

pub fn modal_choice_for_player(
    state: &GameState,
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    modal: &ModalChoice,
    context: &SpellContext,
) -> ModalChoice {
    let mut effective = modal.clone();
    for constraint in &modal.constraints {
        if let ModalSelectionConstraint::ConditionalMaxChoices {
            condition,
            max_choices,
            otherwise_max_choices,
        } = constraint
        {
            let cap = if modal_selection_condition_matches(
                state, player, source_id, condition, context,
            ) {
                *max_choices
            } else {
                *otherwise_max_choices
            };
            effective.max_choices = cap;
        }
    }
    effective
}

fn modal_selection_condition_matches(
    state: &GameState,
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    condition: &ModalSelectionCondition,
    context: &SpellContext,
) -> bool {
    match condition {
        ModalSelectionCondition::Static { condition } => {
            super::layers::evaluate_condition(state, condition, player, source_id)
        }
        ModalSelectionCondition::AdditionalCostPaid {
            source,
            origin,
            origin_ordinal,
            variant,
            kicker_cost,
            min_count,
        } => {
            if let Some(origin) = origin {
                let count = origin_ordinal.map_or_else(
                    || context.instance_payment_count(*origin),
                    |ordinal| context.instance_payment_count_for_ordinal(*origin, ordinal),
                );
                count >= (*min_count).max(1)
            } else {
                context.additional_cost_paid_matches(
                    *source,
                    *variant,
                    kicker_cost.as_ref(),
                    *min_count,
                )
            }
        }
    }
}

/// Returns mode indices unavailable due to NoRepeatThisTurn/NoRepeatThisGame constraints.
/// CR 700.2: Checks per-turn and per-game tracking maps for previously chosen modes.
pub fn compute_unavailable_modes(
    state: &GameState,
    source_id: ObjectId,
    modal: &ModalChoice,
) -> Vec<usize> {
    let mut unavailable = Vec::new();
    for constraint in &modal.constraints {
        match constraint {
            ModalSelectionConstraint::NoRepeatThisTurn => {
                for mode_idx in 0..modal.mode_count {
                    if state
                        .modal_modes_chosen_this_turn
                        .contains(&(source_id, mode_idx))
                    {
                        unavailable.push(mode_idx);
                    }
                }
            }
            ModalSelectionConstraint::NoRepeatThisGame => {
                for mode_idx in 0..modal.mode_count {
                    if state
                        .modal_modes_chosen_this_game
                        .contains(&(source_id, mode_idx))
                    {
                        unavailable.push(mode_idx);
                    }
                }
            }
            ModalSelectionConstraint::ConditionalMaxChoices { .. } => {}
            _ => {} // Other constraints (e.g. DifferentTargetPlayers) are handled elsewhere
        }
    }
    unavailable.sort_unstable();
    unavailable.dedup();
    unavailable
}

/// CR 700.2a-b: Mode indices a modal spell cannot choose — repeat constraints
/// plus modes whose targeting requirements have no legal assignment.
pub fn spell_modal_unavailable_modes(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    modal: &ModalChoice,
    mode_abilities: &[AbilityDefinition],
) -> Vec<usize> {
    let mut unavailable_modes = compute_unavailable_modes(state, source_id, modal);
    let x_dependent_modal_targets = state
        .objects
        .get(&source_id)
        .map(|obj| super::casting_costs::cost_has_x(&obj.mana_cost))
        .unwrap_or(false)
        && mode_abilities.iter().any(|mode| {
            let resolved = build_resolved_from_def(mode, source_id, controller);
            ability_target_legality_needs_chosen_x(&resolved, mode.distribute.as_ref())
        });
    // CR 601.2b/c: When modal spell target legality depends on announced X,
    // modes cannot be pre-disabled before ChooseXValue — same deferral as
    // activated modal abilities (casting.rs AbilityModeChoice path).
    if !x_dependent_modal_targets {
        filter_modes_by_target_legality(
            state,
            source_id,
            controller,
            mode_abilities,
            modal,
            &mut unavailable_modes,
        );
    }
    unavailable_modes
}

/// Spell-kind abilities on a modal spell object — one entry per printed mode.
pub fn modal_spell_mode_abilities(
    obj: &crate::game::game_object::GameObject,
) -> Vec<AbilityDefinition> {
    obj.abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell)
        .cloned()
        .collect()
}

/// CR 700.2a-b + CR 700.2f: Extends `unavailable_modes` with mode indices
/// whose targeting requirements cannot be satisfied on the current board. For
/// each mode not already marked unavailable, builds the resolved ability for
/// that single mode, computes its target slots, and checks whether a legal
/// target assignment exists. Modes that require targets but have no legal
/// assignment are appended to `unavailable_modes`.
///
/// This prevents the softlock where a player (or AI) selects a mode with no
/// legal targets, causing `pending_trigger` to be consumed and then the
/// targeting step to fail irrecoverably.
pub fn filter_modes_by_target_legality(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    mode_abilities: &[AbilityDefinition],
    modal: &ModalChoice,
    unavailable_modes: &mut Vec<usize>,
) {
    let target_constraints = target_constraints_from_modal(modal);
    for mode_idx in 0..modal.mode_count {
        if unavailable_modes.contains(&mode_idx) {
            continue;
        }
        let Some(def) = mode_abilities.get(mode_idx) else {
            continue;
        };
        let resolved = build_resolved_from_def(def, source_id, controller);
        let target_slots = match build_target_slots(state, &resolved) {
            Ok(slots) => slots,
            Err(_) => {
                // build_target_slots returns Err when no legal targets exist
                // for a required targeting slot — mark mode unavailable.
                unavailable_modes.push(mode_idx);
                continue;
            }
        };
        // A mode with no target slots does not require targeting — always legal.
        if target_slots.is_empty() {
            continue;
        }
        if !has_legal_target_assignment_for_ability(
            state,
            &resolved,
            &target_slots,
            &target_constraints,
        ) {
            unavailable_modes.push(mode_idx);
        }
    }
    unavailable_modes.sort_unstable();
    unavailable_modes.dedup();
}

/// Records chosen mode indices for NoRepeat constraint enforcement.
/// CR 700.2: Inserts into per-turn and/or per-game tracking maps.
pub fn record_modal_mode_choices(
    state: &mut GameState,
    source_id: ObjectId,
    modal: &ModalChoice,
    indices: &[usize],
) {
    for constraint in &modal.constraints {
        match constraint {
            ModalSelectionConstraint::NoRepeatThisTurn => {
                for &idx in indices {
                    state.modal_modes_chosen_this_turn.insert((source_id, idx));
                }
            }
            ModalSelectionConstraint::NoRepeatThisGame => {
                for &idx in indices {
                    state.modal_modes_chosen_this_game.insert((source_id, idx));
                }
            }
            _ => {}
        }
    }
}

pub enum TargetSelectionAdvance {
    InProgress(TargetSelectionProgress),
    Complete(Vec<Option<TargetRef>>),
}

/// CR 601.2c + CR 115.3: Identifies one instance of the word "target" on an
/// ability. Slots sharing a `TargetInstanceId` are the SAME "target" (all slots
/// of one `multi_target` "up to N target creatures" run) and must be mutually
/// distinct objects; slots with DIFFERENT ids are separate instances that may
/// reuse the same object ("Destroy target artifact and target land").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TargetInstanceId(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetSlotSpec {
    filter: TargetFilter,
    optional: bool,
    instance: TargetInstanceId,
}

struct AbilityTargetingView<'a> {
    state: &'a GameState,
    ability: &'a ResolvedAbility,
    specs: &'a [TargetSlotSpec],
    target_slots: &'a [TargetSelectionSlot],
    constraints: &'a [TargetSelectionConstraint],
}

/// CR 601.2c: Begin target selection by computing legal targets for the first slot.
pub fn begin_target_selection(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<TargetSelectionProgress, EngineError> {
    build_target_selection_progress(target_slots, constraints, 0, Vec::new())
}

pub fn begin_target_selection_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<TargetSelectionProgress, EngineError> {
    build_target_selection_progress_for_ability(
        state,
        ability,
        target_slots,
        constraints,
        0,
        Vec::new(),
    )
}

/// CR 115.1: Targets are declared as part of putting a spell or ability on the stack.
/// CR 115.3: The same target can't be chosen multiple times for one instance of "target".
pub fn choose_target(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    progress: &TargetSelectionProgress,
    target: Option<TargetRef>,
) -> Result<TargetSelectionAdvance, EngineError> {
    if progress.current_slot >= target_slots.len() {
        return Err(EngineError::InvalidAction(
            "No target slot is currently active".to_string(),
        ));
    }
    if progress.selected_slots.len() != progress.current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }

    let slot = &target_slots[progress.current_slot];
    let mut selected_slots = progress.selected_slots.clone();
    match target {
        Some(target) => {
            if !progress.current_legal_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Illegal target selected".to_string(),
                ));
            }
            selected_slots.push(Some(target));
        }
        None => {
            if !slot.optional {
                return Err(EngineError::InvalidAction(
                    "Cannot skip a required target".to_string(),
                ));
            }
            selected_slots.push(None);
        }
    }

    let next_slot = progress.current_slot + 1;
    if next_slot == target_slots.len() {
        validate_selected_slot_prefix(target_slots, &selected_slots, constraints)?;
        return Ok(TargetSelectionAdvance::Complete(selected_slots));
    }

    Ok(TargetSelectionAdvance::InProgress(
        build_target_selection_progress(target_slots, constraints, next_slot, selected_slots)?,
    ))
}

pub fn choose_target_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    progress: &TargetSelectionProgress,
    target: Option<TargetRef>,
) -> Result<TargetSelectionAdvance, EngineError> {
    if progress.current_slot >= target_slots.len() {
        return Err(EngineError::InvalidAction(
            "No target slot is currently active".to_string(),
        ));
    }
    if progress.selected_slots.len() != progress.current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }

    let slot = &target_slots[progress.current_slot];
    let mut selected_slots = progress.selected_slots.clone();
    match target {
        Some(target) => {
            if !progress.current_legal_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Illegal target selected".to_string(),
                ));
            }
            selected_slots.push(Some(target));
        }
        None => {
            if !slot.optional {
                return Err(EngineError::InvalidAction(
                    "Cannot skip a required target".to_string(),
                ));
            }
            selected_slots.push(None);
        }
    }

    let next_slot = progress.current_slot + 1;
    if next_slot == target_slots.len() {
        validate_selected_slots_for_ability(
            state,
            ability,
            target_slots,
            &selected_slots,
            constraints,
        )?;
        return Ok(TargetSelectionAdvance::Complete(selected_slots));
    }

    Ok(TargetSelectionAdvance::InProgress(
        build_target_selection_progress_for_ability(
            state,
            ability,
            target_slots,
            constraints,
            next_slot,
            selected_slots,
        )?,
    ))
}

pub fn auto_select_targets(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<Option<Vec<TargetRef>>, EngineError> {
    let assignments = generate_target_assignments_with_limit(target_slots, constraints, Some(2));
    match assignments.as_slice() {
        [] => Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        )),
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

pub fn auto_select_targets_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<Option<Vec<TargetRef>>, EngineError> {
    let assignments = build_target_assignments_for_ability_with_limit(
        state,
        ability,
        target_slots,
        constraints,
        Some(2),
    );
    match assignments.as_slice() {
        [] if has_legal_target_assignment_for_ability(
            state,
            ability,
            target_slots,
            constraints,
        ) =>
        {
            Ok(None)
        }
        [] => Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        )),
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

pub fn has_legal_target_assignment_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> bool {
    let specs = target_slot_specs(state, ability);
    has_legal_completion_with_specs(state, ability, &specs, target_slots, constraints, 0, &[])
}

/// CR 115.1 + CR 701.9b: Resolve a `Random`-mode ability's target slots by
/// uniformly choosing from each slot's legal-target set using the engine's
/// seeded RNG (`state.rng`). The game (not the controller) makes the selection;
/// no `WaitingFor::TargetSelection` is emitted. Used by casting/activation
/// dispatchers to short-circuit target prompting for "random target X" cards
/// (Goblin Polka Band, Orcish Catapult, Power Struggle, etc.).
///
/// Determinism: uses `state.rng` (`ChaCha20Rng`, seeded per game), so given the
/// same RNG state and legal-target set, the same target is chosen on every run.
/// This preserves replay/test reproducibility.
///
/// Errors out if any slot has no legal target — the caller has already verified
/// `target_slots.is_empty()` does not hold.
///
/// Limitation (out of scope for the H1 audit fix): when an ability has a
/// `multi_target` spec ("any number of random target creatures") the slot
/// builder produces one slot per max-target. This helper picks one random
/// target per slot, effectively choosing `max` targets. A future enhancement
/// would prompt the controller for the count N first, then pick N random
/// targets — but the current single-slot single-pick behaviour matches
/// Mana-Clash-style cards and the audit's primary bug (silent strip).
pub fn random_select_targets_for_ability(
    state: &mut GameState,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<Vec<TargetRef>, EngineError> {
    use rand::seq::IndexedRandom; // rand 0.9: `choose` on `[T]` lives here.

    let mut chosen: Vec<TargetRef> = Vec::with_capacity(target_slots.len());
    for slot in target_slots {
        // CR 115.3: The same target can't be chosen multiple times for one
        // instance of "target". The interactive `legal_targets_for_slot`
        // enforces this by filtering already-selected targets from each
        // subsequent slot's legal pool; mirror that filter here so the random
        // picker honours the same uniqueness rule.
        let candidate_targets: Vec<TargetRef> = slot
            .legal_targets
            .iter()
            .filter(|t| !chosen.contains(t))
            .cloned()
            .collect();
        if candidate_targets.is_empty() {
            // CR 115.6: A spell or ability that requires targets may allow zero
            // targets to be chosen only when the slot is optional. For random
            // selection there is no controller to skip, so an empty legal-target
            // set (after CR 115.3 uniqueness filtering) cannot be satisfied
            // unless the slot is optional.
            if slot.optional {
                continue;
            }
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available for random selection".to_string(),
            ));
        }
        let pick = candidate_targets.choose(&mut state.rng).cloned().ok_or(
            EngineError::ActionNotAllowed("Random selection failed to draw a target".to_string()),
        )?;
        chosen.push(pick);
    }
    // Multi-slot constraints (e.g., DifferentTargetPlayers) — reuse the same
    // validator the controller-choice path uses so random selection respects
    // every constraint declared on the ability.
    validate_target_constraints(Some(state), &chosen, constraints, None)?;
    Ok(chosen)
}

/// CR 608.2b: When resolving, check that targets are still legal. If all targets are illegal,
/// the spell or ability doesn't resolve.
pub fn validate_selected_targets(
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    validate_selected_targets_inner(None, target_slots, targets, constraints)
}

pub fn validate_selected_targets_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    validate_selected_targets_inner(Some((state, ability)), target_slots, targets, constraints)
}

/// Shared body for the two `validate_selected_targets*` entry points —
/// count-window validation lives here exactly once. With an ability context
/// the prefix check is the spec-aware CR 608.2b re-validation against current
/// game state; without one it checks against the stored slot snapshots.
fn validate_selected_targets_inner(
    ability_ctx: Option<(&GameState, &ResolvedAbility)>,
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let minimum_targets = target_slots.iter().filter(|slot| !slot.optional).count();
    if targets.len() < minimum_targets || targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(format!(
            "Expected between {minimum_targets} and {} targets, got {}",
            target_slots.len(),
            targets.len()
        )));
    }

    match ability_ctx {
        Some((state, ability)) => {
            validate_target_prefix_for_ability(state, ability, target_slots, targets, constraints)
        }
        None => validate_target_prefix(target_slots, targets, constraints),
    }
}

fn validate_target_prefix(
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    for (index, target) in targets.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };
        if !slot.legal_targets.contains(target) {
            return Err(EngineError::InvalidAction(
                "Illegal target selected".to_string(),
            ));
        }
    }

    validate_target_constraints(None, targets, constraints, None)
}

pub fn generate_target_assignments(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Vec<Vec<TargetRef>> {
    generate_target_assignments_with_limit(target_slots, constraints, None)
}

fn generate_target_assignments_with_limit(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    limit: Option<usize>,
) -> Vec<Vec<TargetRef>> {
    let mut current = Vec::with_capacity(target_slots.len());
    let mut out = Vec::new();
    build_target_assignments(target_slots, constraints, 0, &mut current, &mut out, limit);
    out
}

/// CR 601.2c: Assign chosen targets to the correct effects in the ability chain.
pub fn assign_targets_in_chain(
    state: &GameState,
    ability: &mut ResolvedAbility,
    targets: &[TargetRef],
) -> Result<(), EngineError> {
    if is_per_opponent_target_fanout(ability) {
        ability.targets = targets.to_vec();
        return Ok(());
    }
    if !chain_has_target_sink(ability) {
        ability.targets = targets.to_vec();
        return Ok(());
    }
    let mut next_target = 0usize;
    assign_targets_recursive(state, ability, targets, &mut next_target)?;
    if next_target != targets.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected targets".to_string(),
        ));
    }
    Ok(())
}

pub fn assign_selected_slots_in_chain(
    state: &GameState,
    ability: &mut ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
) -> Result<(), EngineError> {
    if is_per_opponent_target_fanout(ability) {
        ability.targets = selected_slots.iter().flatten().cloned().collect();
        return Ok(());
    }
    if !chain_has_target_sink(ability) {
        ability.targets = selected_slots.iter().flatten().cloned().collect();
        return Ok(());
    }
    let mut next_slot = 0usize;
    assign_selected_slots_recursive(state, ability, selected_slots, &mut next_slot)?;
    if next_slot != selected_slots.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected target slots".to_string(),
        ));
    }
    Ok(())
}

pub fn flatten_targets_in_chain(ability: &ResolvedAbility) -> Vec<TargetRef> {
    let mut targets = if is_per_opponent_target_fanout(ability) {
        object_targets_only(&ability.targets)
    } else {
        ability.targets.clone()
    };
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        targets.extend(flatten_targets_in_chain(sub_ability));
    }
    if let Some(else_ability) = ability.else_ability.as_deref() {
        targets.extend(flatten_targets_in_chain(else_ability));
    }
    targets
}

/// CR 608.2b: Re-validate targets on resolution — remove any that are no longer legal.
pub fn validate_targets_in_chain(state: &GameState, ability: &ResolvedAbility) -> ResolvedAbility {
    let mut validated = ability.clone();
    validated.targets = if is_per_opponent_target_fanout(&validated) {
        validate_per_opponent_target_fanout_targets(state, &validated)
    } else if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &validated.effect
    {
        move_counter_stack_target_filters(source, target, *selection)
            .into_iter()
            .filter(|filter| !filter.is_context_ref())
            .zip(validated.targets.iter())
            .filter_map(|(filter, target_ref)| {
                let legal = targeting::validate_targets_for_ability(
                    state,
                    std::slice::from_ref(target_ref),
                    filter,
                    &validated,
                );
                legal.into_iter().next()
            })
            .collect()
    } else if let Effect::Attach { attachment, target } = &validated.effect {
        let mut kept = Vec::new();
        let mut target_iter = validated.targets.iter();
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if !attach_side_needs_target_slot(filter, is_attachment) {
                continue;
            }
            let Some(target_ref) = target_iter.next() else {
                continue;
            };
            if let Some(legal) = targeting::validate_targets_for_ability(
                state,
                std::slice::from_ref(target_ref),
                filter,
                &validated,
            )
            .into_iter()
            .next()
            {
                kept.push(legal);
            }
        }
        kept
    } else if let Some(src_leaf) = prevent_damage_source_slot_filter(&validated.effect).cloned() {
        // CR 608.2b + CR 609.7a: A source-scoped `PreventDamage` carries its
        // chosen source spell in `targets[0]`. `extract_target_filter_from_effect`
        // returns `None` for its `Any` recipient, so the generic `None` arm below
        // would fizzle-filter the spell to battlefield presence and drop it
        // (the spell lives on the STACK). Re-validate against the source leaf
        // (`InZone Stack`-aware) instead, preserving the spell target.
        targeting::validate_targets_for_ability(state, &validated.targets, &src_leaf, &validated)
    } else {
        match triggers::extract_target_filter_from_effect(&validated.effect) {
            Some(filter) if matches!(validated.effect, Effect::PairWith { .. }) => {
                let legal_choices = pair_with_legal_choices(state, &validated, filter);
                validated
                    .targets
                    .iter()
                    .filter(|target| legal_choices.contains(target))
                    .cloned()
                    .collect()
            }
            Some(filter) => targeting::validate_targets_for_ability(
                state,
                &validated.targets,
                filter,
                &validated,
            ),
            // CR 608.2b: A context-ref filter (`ParentTarget`,
            // `TriggeringSource`, etc.) carries a resolution-time *snapshot*,
            // not a player-chosen target. `extract_target_filter_from_effect`
            // returns `None` for it via the `is_context_ref` guard, but unlike
            // a genuinely target-less effect its `targets` must NOT be fizzle-
            // filtered: CR 608.2b's "no longer in the zone" check applies only
            // to abilities that *specify targets* (use the word "target"). A
            // delayed-return trigger (Flickerwisp) deliberately references an
            // exiled card — filtering it to battlefield presence would wrongly
            // fizzle the return.
            //
            // NOTE: CR 603.7c's resolution-time zone check ("if that object is
            // no longer in the zone it's expected to be in ... the ability
            // won't affect it") is NOT yet enforced for `origin: None` delayed
            // returns. `change_zone::resolve`'s CR 400.7 guard only runs under
            // `if let Some(expected_origin) = origin`, so a Flickerwisp victim
            // that leaves Exile before the end step would still be moved.
            // Tracked as a separate, broader follow-up issue (touches the
            // parser + `change_zone.rs`) — out of scope here.
            None if validated
                .effect
                .target_filter()
                .is_some_and(|f| f.is_context_ref()) =>
            {
                validated.targets.clone()
            }
            None => validated
                .targets
                .iter()
                .filter(|target| match target {
                    TargetRef::Object(object_id) => state.battlefield.contains(object_id),
                    TargetRef::Player(_) => true,
                })
                .cloned()
                .collect(),
        }
    };
    if let Some(sub_ability) = validated.sub_ability.as_mut() {
        **sub_ability = validate_targets_in_chain(state, sub_ability);
    }
    if let Some(else_ability) = validated.else_ability.as_mut() {
        **else_ability = validate_targets_in_chain(state, else_ability);
    }
    validated
}

/// CR 609.7 + CR 601.2c: For a source-scoped `PreventDamage`
/// ("prevent all damage target instant or sorcery spell would deal this turn"),
/// surface the choosable source object as a target slot.
///
/// The effect's `damage_source_filter` is an `And` pairing a
/// `ParentTargetSlot { index }` sentinel (which captures the chosen object at
/// resolution, CR 609.7a) with the choosable `Typed`/stack-spell leaf. The
/// sentinel cannot be enumerated by `find_legal_targets`, so we return the
/// SIBLING leaf — the actual "instant or sorcery spell" filter that
/// `targeting.rs::filter_targets_stack_spells` can enumerate on the stack.
///
/// Returns `None` for recipient-scoped or `ChosenDamageSource`/`IsChosenColor`
/// ("by …" Arachnogenesis) prevents, so those are NOT diverted into a source
/// target slot.
fn prevent_damage_source_slot_filter(effect: &Effect) -> Option<&TargetFilter> {
    let Effect::PreventDamage {
        damage_source_filter: Some(TargetFilter::And { filters }),
        ..
    } = effect
    else {
        return None;
    };
    // Only an `And` that carries the `ParentTargetSlot` sentinel is a
    // source-scoped capture; return the sibling choosable leaf.
    if !filters
        .iter()
        .any(|f| matches!(f, TargetFilter::ParentTargetSlot { .. }))
    {
        return None;
    }
    filters
        .iter()
        .find(|f| !matches!(f, TargetFilter::ParentTargetSlot { .. }))
}

/// CR 120.3a + CR 603.7c: Constrain a companion `ControllerRef::TargetPlayer`
/// slot to the damaged player(s) of the triggering damage event.
///
/// "Whenever … deals combat damage to a player, [destroy/goad] target creature
/// that player controls" binds "that player" to the player the event damaged,
/// not to a free choice. While the trigger declares its targets on the stack,
/// `current_trigger_event` is not yet set (it is populated at resolution), so
/// the damaged player is read from `pending_trigger_event_batch`.
///
/// Returns `None` — preserving the unconstrained all-players slot — unless every
/// event in the batch is damage dealt to a player. That keeps genuine
/// free-choice "target player" filters (the `PutCounterAll` "each creature
/// target player controls" spell shape, ETB triggers that target a player)
/// unconstrained: those carry no damage-to-player event here.
fn damaged_player_targets_for_companion_slot(state: &GameState) -> Option<Vec<TargetRef>> {
    let batch = &state.pending_trigger_event_batch;
    if batch.is_empty() {
        return None;
    }
    let mut players: Vec<TargetRef> = Vec::new();
    for event in batch {
        let is_damage_to_player = matches!(
            event,
            crate::types::events::GameEvent::CombatDamageDealtToPlayer { .. }
                | crate::types::events::GameEvent::DamageDealt {
                    target: TargetRef::Player(_),
                    ..
                }
        );
        if !is_damage_to_player {
            return None;
        }
        if let Some(pid) = targeting::extract_player_from_event(event, state) {
            let target = TargetRef::Player(pid);
            if !players.contains(&target) {
                players.push(target);
            }
        }
    }
    (!players.is_empty()).then_some(players)
}

/// Legal targets for the companion `TargetFilter::Player` slot — the player
/// whose permanents a `ControllerRef::TargetPlayer` ("that player controls")
/// filter scopes to. Single authority shared by the static slot build
/// (`collect_target_slots`) and the dynamic selection-time recompute
/// (`legal_targets_for_selected_slot`); the two MUST agree or selection-time
/// recomputation would re-offer every player and reintroduce the hang.
///
/// For a damage-to-player trigger the slot is bound to the damaged player(s) of
/// the triggering event (CR 120.3a). Gated on `source_incarnation` (carried only
/// by triggered abilities) so a stale event batch never constrains a spell's
/// genuine free-choice "target player". Otherwise every legal player is offered.
fn companion_target_player_legal_targets(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    ability
        .source_incarnation
        .and_then(|_| damaged_player_targets_for_companion_slot(state))
        .unwrap_or_else(|| {
            targeting::find_legal_targets(
                state,
                &TargetFilter::Player,
                ability.controller,
                ability.source_id,
            )
        })
}

fn collect_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    acc: &mut SlotAccumulator,
) -> Result<(), EngineError> {
    if let Some(sub_ability) = ability.sub_ability.as_deref().filter(|sub| {
        matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    }) {
        if ability.context.additional_cost_paid {
            collect_target_slots(state, sub_ability, acc)?;
            return Ok(());
        }
    }

    // CR 609.7 + CR 601.2c: A source-scoped `PreventDamage` ("prevent all damage
    // target instant or sorcery spell would deal this turn") surfaces the
    // choosable source spell as a target slot. Declared FIRST (CR 601.2c
    // declaration order). The generic path below cannot reach it —
    // `target_filter()` returns the `Any` recipient and short-circuits to `None`
    // — so we surface it here, mirroring the `CreateDamageReplacement` arm. We
    // do NOT `return`: the generic recipient logic still runs, but for the
    // source-scoped form `target == Any` so it adds nothing.
    if ability.target_choice_timing == TargetChoiceTiming::Stack {
        if let Some(src_leaf) = prevent_damage_source_slot_filter(&ability.effect) {
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, src_leaf, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
    }

    // CR 701.12a: ExchangeControl carries two distinct per-slot filters. SelfRef
    // slots (e.g. "this artifact and target …") are filled by the resolver from
    // ability.source_id and don't require a player choice. Surface one slot per
    // non-SelfRef filter, in declaration order.
    if let Effect::ExchangeControl { target_a, target_b } = &ability.effect {
        for filter in [target_a, target_b] {
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
        return Ok(());
    }

    // CR 701.12a: ExchangeLifeTotals carries two distinct per-slot player filters.
    // Context-ref filters (Controller / "you") are filled by the resolver from
    // ability.controller and don't require a player choice. Surface one slot per
    // non-context-ref filter, in declaration order. (Keep in sync with
    // `build_target_slot_specs` or the slot-count invariant at ~408 fires.)
    if let Effect::ExchangeLifeTotals { player_a, player_b } = &ability.effect {
        for filter in [player_a, player_b] {
            if filter.is_context_ref() {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
        return Ok(());
    }

    if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &ability.effect
    {
        for filter in move_counter_stack_target_filters(source, target, *selection) {
            if filter.is_context_ref() {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
    } else if let Effect::Attach { attachment, target } = &ability.effect {
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if !attach_side_needs_target_slot(filter, is_attachment) {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
    } else if let Effect::CreateDamageReplacement {
        recipient_object_filter,
        redirect_object_filter,
        ..
    } = &ability.effect
    {
        // CR 115.1 + CR 614.9: Surface up to two object target slots for the
        // one-shot damage replacement — `target_filter()` returns None for this
        // effect, so the generic path below never reaches it.
        //
        // ORDER IS LOAD-BEARING: the *original-recipient* slot ("would deal
        // damage to target creature" — Jade Monolith) is declared FIRST, then
        // the *redirect-destination* slot ("...to target creature instead" —
        // Soltari Guerrillas). The resolver reads `recipient_host` from
        // `chosen_target_object(ability, 0)` and the redirect from
        // `chosen_redirect_object` (which skips the recipient slot when present),
        // so the surfacing order here must match that indexing exactly.
        for filter in [recipient_object_filter, redirect_object_filter]
            .into_iter()
            .flatten()
        {
            // CR 614.9: a `SelfRef` original-recipient ("...dealt to ~" — the
            // en-Kor cycle) is the ability's own source, not a chosen target, so
            // it surfaces no target slot. The resolver hosts the shield on the
            // source directly.
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
    } else {
        if is_per_opponent_target_fanout(ability) {
            collect_per_opponent_target_fanout_slots(state, ability, acc)?;
            if let Some(sub_ability) = ability.sub_ability.as_deref() {
                if !defers_conditional_target_selection(sub_ability) {
                    collect_target_slots(state, sub_ability, acc)?;
                }
            }
            return Ok(());
        }
        // CR 109.4 + CR 115.1: If the effect contains a filter referencing
        // `ControllerRef::TargetPlayer` (e.g. "each creature target player controls"
        // on `PutCounterAll`), surface a companion `TargetFilter::Player` slot
        // BEFORE the effect's primary filter slot. The chosen player is read back
        // at filter-evaluation time via `ability.targets`. Runs before the primary
        // filter so the player is chosen first (target declaration order matches
        // Oracle text order).
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && ability_needs_companion_target_player_slot(ability)
        {
            // CR 120.3a + CR 603.7c: For a damage-to-player trigger ("…deals
            // combat damage to a player, [destroy/goad] target creature that
            // player controls"), "that player" is the DAMAGED player carried by
            // the triggering event — not a free choice among every player at the
            // table. In two-player games an all-players slot happens to work
            // (one opponent), but in multiplayer it offers wrong players (and
            // even the source's controller), and the dependent creature slot
            // ("creatures that player controls") then has no satisfiable
            // combination, collapsing legal-action generation to empty and
            // hanging the controller. Bind the companion slot to the damaged
            // player(s) when this is a damage-to-player trigger. Shared with the
            // selection-time recompute so both paths agree.
            let player_targets = companion_target_player_legal_targets(state, ability);
            if player_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets: player_targets,
                optional: ability.optional_targeting,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_target_creature_quantity_slot(&ability.effect)
        {
            let filter = target_creature_quantity_slot_filter();
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, &filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        {
            let filter = parent_target_combat_relation_slot_filter();
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, &filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && !effect_target_filter_references_chosen_player(&ability.effect)
        {
            if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
                let legal_targets =
                    legal_choices_for_ability_filter(state, ability, filter, &acc.slots);
                // CR 601.2c: An "up to N" ability (`multi_target.min == 0`) — or an
                // ability-wide "up to one" (`optional_targeting`) — may legally
                // choose zero targets, so an empty legal-target set is acceptable.
                // Only abilities that require at least one target error out here.
                if let Some(spec) = ability.multi_target.as_ref() {
                    let bounds =
                        resolve_multi_target_bounds(state, ability, spec, legal_targets.len())?;
                    for slot_index in 0..bounds.max {
                        acc.push(TargetSelectionSlot {
                            legal_targets: legal_targets.clone(),
                            optional: slot_index >= bounds.min,
                        });
                    }
                } else {
                    if legal_targets.is_empty() && !ability.optional_targeting {
                        return Err(EngineError::ActionNotAllowed(
                            "No legal targets available".to_string(),
                        ));
                    }
                    acc.push(TargetSelectionSlot {
                        legal_targets,
                        optional: ability.optional_targeting,
                    });
                }
            }
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        collect_target_slots_after_deferred_effect(state, ability.sub_ability.as_deref(), acc)?;
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        // CR 700.2c: Conditional sub-mode targets are chosen only if the
        // condition holds at resolution time (CR 601.2c), not when the parent
        // goes on the stack — so they are pre-collected later by
        // `resolve_ability_chain`, not here. They are intentionally left
        // UNLABELLED for the modal targeting banner: no slot is surfaced at
        // mode-selection time, so there is no slot to attach a mode label to.
        if !defers_conditional_target_selection(sub_ability) {
            collect_target_slots(state, sub_ability, acc)?;
        }
    }
    Ok(())
}

fn legal_choices_for_ability_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    existing_slots: &[TargetSelectionSlot],
) -> Vec<TargetRef> {
    if matches!(ability.effect, Effect::PairWith { .. }) {
        return pair_with_legal_choices(state, ability, filter);
    }
    legal_targets_for_ability_filter(state, ability, filter, existing_slots)
}

fn pair_with_legal_choices(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<TargetRef> {
    super::pairing::legal_pair_choice_refs(state, ability.source_id, ability.controller, filter)
}

fn resolve_multi_target_max(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &MultiTargetSpec,
) -> Option<usize> {
    spec.max
        .as_ref()
        .map(|expr| resolve_quantity_with_targets(state, expr, ability).max(0) as usize)
}

/// CR 601.2c: A spell with a variable number of targets announces how many
/// targets it will choose before choosing them.
fn resolve_multi_target_min(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &MultiTargetSpec,
) -> usize {
    resolve_quantity_with_targets(state, &spec.min, ability).max(0) as usize
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MultiTargetBounds {
    pub min: usize,
    pub max: usize,
}

/// CR 601.2d: When a spell or ability divides an effect (damage, counters)
/// among its targets, each chosen target must receive at least one unit. The
/// pool to divide is therefore an upper bound on how many targets may legally be
/// chosen — picking more targets than units leaves at least one target with
/// nothing, which the rules forbid. Returns the resolved pool size for a
/// distributing ability, peeling any outer "up to" wrapper so the structural
/// maximum (not the cap) drives the bound. Returns `None` when the pool amount
/// is not a damage/counter count (e.g. life-distribution stubs that don't
/// surface a divisible amount), in which case no pool cap applies.
///
/// `distribute` is the distribution-unit flag carried on the originating
/// `AbilityDefinition` / `PendingCast` (the runtime `ResolvedAbility` does not
/// itself carry it), so callers in the cast/trigger pipeline pass it through.
pub(crate) fn distribution_pool_cap(
    state: &GameState,
    ability: &ResolvedAbility,
    distribute: Option<&crate::types::game_state::DistributionUnit>,
) -> Option<usize> {
    distribute?;
    let amount = match &ability.effect {
        Effect::DealDamage { amount, .. } => amount,
        Effect::PutCounter { count, .. } => count,
        _ => return None,
    };
    // CR 601.2d: "up to N divided as you choose" still divides the *resolved*
    // amount; peel the cap so the pool is the concrete number to distribute.
    let (inner, _) = amount.peel_up_to();
    Some(resolve_quantity_with_targets(state, inner, ability).max(0) as usize)
}

/// CR 601.2c + CR 601.2d: Truncate `target_slots` so a divided spell offers at
/// most one slot per unit of its divisible pool. Each chosen target must receive
/// ≥1 (CR 601.2d), so a pool of N can be split among at most N targets; offering
/// more slots lets the controller pick a target set that can never be legally
/// divided (the Shatterskull Smashing X=1 / two-slot softlock, issue #2856).
///
/// Required slots (the leading `!optional` prefix) are preserved — only the
/// optional "up to" tail beyond the pool size is dropped. A no-op when the
/// ability does not distribute, the pool is not a countable amount, or the pool
/// already meets/exceeds the slot count (the common case, e.g. Lathiel whose
/// printed cap already equals the pool).
pub(crate) fn cap_distribution_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    distribute: Option<&crate::types::game_state::DistributionUnit>,
    target_slots: &mut Vec<TargetSelectionSlot>,
) {
    let Some(pool) = distribution_pool_cap(state, ability, distribute) else {
        return;
    };
    let required = target_slots.iter().filter(|slot| !slot.optional).count();
    // Never drop a required slot: if the pool somehow underruns the structural
    // minimum, keep the minimum (a malformed spec, not reachable for well-formed
    // "up to N" distribution where min == 0).
    let keep = pool.max(required);
    if target_slots.len() > keep {
        target_slots.truncate(keep);
    }
}

/// CR 115.1d: A triggered ability's targets are chosen as it is put on the stack.
/// CR 601.2c: Resolve a multi-target count after any required quantity choices
/// have been announced, then cap optional slots at the live legal-target set
/// while preserving the required minimum.
pub(crate) fn resolve_multi_target_bounds(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &MultiTargetSpec,
    legal_target_count: usize,
) -> Result<MultiTargetBounds, EngineError> {
    if multi_target_needs_quantity_choice(state, ability, spec) {
        return Err(EngineError::ActionNotAllowed(
            "Target count requires a resolved quantity before target selection".to_string(),
        ));
    }

    let raw_min = resolve_multi_target_min(state, ability, spec);
    let raw_max = resolve_multi_target_max(state, ability, spec).unwrap_or(legal_target_count);
    // CR 601.2c: A resolved variable maximum can legitimately fall below the
    // spec's structural minimum. For "distribute X counters among any number of
    // target creatures" (Grove's Bounty) the floor of 1 expresses "each chosen
    // target must receive a counter", but that floor only applies when there is
    // something to distribute — casting for X=0 distributes nothing, so the
    // required target count collapses to 0. Clamping `min` to `raw_max` yields
    // exactly `min(1, X)`: 1 when X >= 1, 0 when X = 0. A genuinely malformed
    // static spec never reaches here (constructors keep min <= max).
    let min = raw_min.min(raw_max);
    if legal_target_count < min {
        return Err(EngineError::ActionNotAllowed(
            "Not enough legal targets available".to_string(),
        ));
    }

    Ok(MultiTargetBounds {
        min,
        max: raw_max.min(legal_target_count),
    })
}

fn multi_target_needs_quantity_choice(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &MultiTargetSpec,
) -> bool {
    quantity_expr_has_unresolved_variable(state, ability, &spec.min)
        || spec
            .max
            .as_ref()
            .is_some_and(|expr| quantity_expr_has_unresolved_variable(state, ability, expr))
}

fn quantity_expr_has_unresolved_variable(
    state: &GameState,
    ability: &ResolvedAbility,
    expr: &QuantityExpr,
) -> bool {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if name == "X" => ability.chosen_x.is_none(),
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { .. },
        } => state.last_named_choice.is_none(),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_has_unresolved_variable(state, ability, inner),
        QuantityExpr::Sum { exprs } => exprs
            .iter()
            .any(|expr| quantity_expr_has_unresolved_variable(state, ability, expr)),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_has_unresolved_variable(state, ability, left)
                || quantity_expr_has_unresolved_variable(state, ability, right)
        }
        QuantityExpr::Fixed { .. } | QuantityExpr::Ref { .. } => false,
    }
}

pub fn ability_target_legality_needs_chosen_x(
    ability: &ResolvedAbility,
    distribute: Option<&crate::types::game_state::DistributionUnit>,
) -> bool {
    if ability.chosen_x.is_some() {
        return false;
    }
    ability_target_legality_needs_chosen_x_inner(ability)
        // CR 601.2c + CR 601.2d: A divided spell's legal target count is bounded
        // by the divisible pool (each target needs ≥1). When that pool is an
        // X-dependent amount divided among "up to N" targets (Shatterskull
        // Smashing: "X damage divided among up to two target creatures"), the
        // effective target ceiling `min(N, X)` can't be computed until X is
        // announced — so defer target selection to ChooseXValue even though the
        // printed `multi_target.max` is a fixed value (issue #2856).
        || ability_distribution_pool_needs_chosen_x(ability, distribute)
}

fn ability_target_legality_needs_chosen_x_inner(ability: &ResolvedAbility) -> bool {
    triggers::extract_target_filter_from_effect(&ability.effect)
        .is_some_and(|filter| target_filter_needs_chosen_x(ability, filter))
        || ability.multi_target.as_ref().is_some_and(|spec| {
            quantity_expr_has_unresolved_x(ability, &spec.min)
                || spec
                    .max
                    .as_ref()
                    .is_some_and(|expr| quantity_expr_has_unresolved_x(ability, expr))
        })
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_target_legality_needs_chosen_x_inner)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_target_legality_needs_chosen_x_inner)
}

fn target_filter_needs_chosen_x(ability: &ResolvedAbility, filter: &TargetFilter) -> bool {
    ability.chosen_x.is_none() && target_filter_contains_chosen_x_ref(filter)
}

fn target_filter_contains_chosen_x_ref(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(filter_prop_contains_chosen_x_ref),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            target_filter_contains_chosen_x_ref(filter)
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_contains_chosen_x_ref)
        }
        _ => false,
    }
}

/// CR 601.2c: A negated prop (`FilterProp::Not`) can wrap an X-bearing prop
/// (e.g. `Not(Cmc { value: X })`), so X resolution must descend into it just
/// like the `CanEnchant` filter-bearing arm — otherwise an unannounced X in a
/// negated relative clause would be missed when deciding to route through
/// `ChooseXValue` ahead of target selection.
fn filter_prop_contains_chosen_x_ref(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::Cmc { value, .. } | FilterProp::Counters { count: value, .. } => {
            value.contains_x()
        }
        FilterProp::CanEnchant { target } => target_filter_contains_chosen_x_ref(target),
        FilterProp::Not { prop } => filter_prop_contains_chosen_x_ref(prop),
        _ => false,
    }
}

fn quantity_expr_has_unresolved_x(ability: &ResolvedAbility, expr: &QuantityExpr) -> bool {
    ability.chosen_x.is_none() && expr.contains_x()
}

/// CR 601.2c + CR 601.2d: True when `ability` divides a damage/counter pool
/// whose amount still references an unannounced X. The number of targets such a
/// spell may have is `min(printed cap, pool)`, so the pool — and therefore X —
/// must be known before target slots are built. Used to route Shatterskull-class
/// X-divided spells through `ChooseXValue` ahead of target selection even though
/// their `multi_target.max` is a fixed printed value.
fn ability_distribution_pool_needs_chosen_x(
    ability: &ResolvedAbility,
    distribute: Option<&crate::types::game_state::DistributionUnit>,
) -> bool {
    if distribute.is_none() {
        return false;
    }
    let amount = match &ability.effect {
        Effect::DealDamage { amount, .. } => amount,
        Effect::PutCounter { count, .. } => count,
        _ => return false,
    };
    let (inner, _) = amount.peel_up_to();
    quantity_expr_has_unresolved_x(ability, inner)
}

/// CR 109.4 + CR 115.1: Returns true if `effect` needs a companion
/// `TargetFilter::Player` target slot. This covers filters that reference
/// `ControllerRef::TargetPlayer` and restriction effects whose affected player
/// scope is the declared "target player".
fn effect_references_target_player(effect: &Effect) -> bool {
    if let Effect::AddRestriction {
        restriction:
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::TargetedPlayer,
                ..
            },
    } = effect
    {
        return true;
    }

    if let Effect::Attach { attachment, target } | Effect::UnattachAll { attachment, target } =
        effect
    {
        return filter_references_target_player(attachment)
            || filter_references_target_player(target);
    }

    if let Effect::GenericEffect {
        static_abilities,
        target: None,
        ..
    } = effect
    {
        if static_abilities.iter().any(|static_def| {
            static_def
                .affected
                .as_ref()
                .is_some_and(filter_references_target_player)
        }) {
            return true;
        }
    }

    match effect.target_filter() {
        Some(f) if filter_references_target_player(f) => return true,
        _ => {}
    }
    // Also inspect mass-placement `target` fields that are NOT surfaced as
    // target slots (PutCounterAll, DestroyAll, PumpAll, DamageAll, etc. —
    // their `target_filter()` returns None because the field is a mass
    // filter, not a targeting filter).
    //
    // CR 115.1 + CR 404 + CR 406: A mass filter set to `TargetFilter::Player`
    // (e.g. `ChangeZoneAll { origin: Graveyard, target: Player }` for
    // "exile target player's graveyard" — Nihil Spellbomb, Bojuka Bog,
    // Tormod's Crypt class) parameterizes the scan by a player target. Surface
    // the companion player slot so the resolver's `player_scope` branch
    // reads the chosen target out of `ability.targets` instead of falling
    // back to the activator's own graveyard.
    match effect {
        Effect::PutCounterAll { target, .. }
        | Effect::DestroyAll { target, .. }
        | Effect::GainControlAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::SetTapState {
            scope: EffectScope::All,
            target,
            ..
        }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::ChangeZoneAll { target, .. }
        | Effect::DoublePTAll { target, .. } => {
            matches!(target, TargetFilter::Player) || filter_references_target_player(target)
        }
        _ => false,
    }
}

fn ability_needs_companion_target_player_slot(ability: &ResolvedAbility) -> bool {
    // Triggered abilities carry source_incarnation. Hellkite-style
    // GainControlAll uses "that player" from the triggering event, not a
    // declared target player, so surfacing a stack target here makes it fizzle.
    if matches!(ability.effect, Effect::GainControlAll { .. })
        && ability.source_incarnation.is_some()
    {
        return false;
    }
    effect_references_target_player(&ability.effect)
}

/// CR 608.2c + CR 109.4: Tree-walks a `TargetFilter` and returns true if any
/// `TypedFilter` inside it is scoped to `ControllerRef::ChosenPlayer`. Such a
/// filter resolves against a player chosen *during* resolution (an earlier
/// `Effect::Choose`), so it must NOT surface a stack-push target slot — the
/// chosen player (and therefore the legal-target set) is not known when the
/// ability goes on the stack. The dependent effect selects its target during
/// resolution instead.
fn filter_references_chosen_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { controller, .. }) => {
            matches!(controller, Some(ControllerRef::ChosenPlayer { .. }))
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_chosen_player)
        }
        TargetFilter::Not { filter } => filter_references_chosen_player(filter),
        _ => false,
    }
}

/// True when the effect's primary target filter is scoped to a resolution-time
/// chosen player — see `filter_references_chosen_player`.
fn effect_target_filter_references_chosen_player(effect: &Effect) -> bool {
    effect
        .target_filter()
        .is_some_and(filter_references_chosen_player)
}

/// CR 608.2c + CR 109.4: First `ControllerRef::ChosenPlayer` index found in
/// the filter tree, if any. Used at resolution time to bind the chosen player
/// before enumerating the dependent effect's legal targets.
pub(crate) fn filter_chosen_player_index(filter: &TargetFilter) -> Option<u8> {
    match filter {
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::ChosenPlayer { index }),
            ..
        }) => Some(*index),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().find_map(filter_chosen_player_index)
        }
        TargetFilter::Not { filter } => filter_chosen_player_index(filter),
        _ => None,
    }
}

/// CR 109.4: Rewrite every `ControllerRef::ChosenPlayer` in the filter tree to
/// `ControllerRef::You` so `find_legal_targets`' source-controller plumbing
/// can enumerate the chosen player's objects by passing that player as the
/// `controller` argument. Mirrors the `TargetPlayer → You` rewrite at
/// `legal_targets_for_ability_filter`.
pub(crate) fn rewrite_chosen_player_to_you(filter: &TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf)
            if matches!(tf.controller, Some(ControllerRef::ChosenPlayer { .. })) =>
        {
            let mut rewritten = tf.clone();
            rewritten.controller = Some(ControllerRef::You);
            TargetFilter::Typed(rewritten)
        }
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(rewrite_chosen_player_to_you).collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(rewrite_chosen_player_to_you).collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(rewrite_chosen_player_to_you(filter)),
        },
        other => other.clone(),
    }
}

/// Whether the attachment operand of `Effect::Attach` consumes an explicit
/// player-chosen target. Scan-based filters (e.g. "Equipment attached to ~")
/// resolve from the battlefield/LKI and must not steal `ParentTarget` slots.
fn attach_attachment_filter_needs_target_slot(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(tf) => !tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToSource)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(attach_attachment_filter_needs_target_slot),
        TargetFilter::Not { filter } => attach_attachment_filter_needs_target_slot(filter),
        _ => false,
    }
}

/// Whether the host operand of `Effect::Attach` consumes an explicit target.
fn attach_host_filter_needs_target_slot(filter: &TargetFilter) -> bool {
    !filter.is_context_ref()
        && !matches!(
            filter,
            TargetFilter::LastCreated | TargetFilter::LastRevealed
        )
}

fn attach_side_needs_target_slot(filter: &TargetFilter, is_attachment: bool) -> bool {
    if is_attachment {
        attach_attachment_filter_needs_target_slot(filter)
    } else {
        attach_host_filter_needs_target_slot(filter)
    }
}

/// Tree-walks a `TargetFilter` and returns true if any `TypedFilter` inside
/// it binds to `ControllerRef::TargetPlayer`.
pub(crate) fn filter_references_target_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter {
            controller,
            properties,
            ..
        }) => {
            matches!(controller, Some(ControllerRef::TargetPlayer))
                || properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::Owned {
                            controller: ControllerRef::TargetPlayer,
                        }
                    )
                })
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_target_player)
        }
        TargetFilter::Not { filter } => filter_references_target_player(filter),
        _ => false,
    }
}

/// Resolve a player-scoped `TargetFilter` to the concrete set of player ids it
/// affects, for an effect whose targets live on `ability`.
///
/// Explicit `TargetRef::Player` targets win. Otherwise a player-typed mass
/// filter (`Controller`, `Player`, or a `Typed` filter with no `type_filters`
/// and an optional `controller` ref) expands to the matching player ids.
/// Returns an empty vec if the filter doesn't refer to players (the caller's
/// object branch handles those). Every `ControllerRef` variant is matched
/// exhaustively so this is the single authority for the
/// "player-typed filter → `Vec<PlayerId>`" shape (shared by phasing's
/// player path and the transient-effect player-scope binding).
pub(crate) fn collect_player_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<PlayerId> {
    let from_targets: Vec<PlayerId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    match target {
        TargetFilter::Controller => vec![ability.scoped_player.unwrap_or(ability.controller)],
        TargetFilter::Player => state.players.iter().map(|p| p.id).collect(),
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            ..
        }) if type_filters.is_empty() => state
            .players
            .iter()
            .filter(|p| match controller {
                Some(ControllerRef::You) => p.id == ability.controller,
                Some(ControllerRef::Opponent) => p.id != ability.controller,
                Some(ControllerRef::ScopedPlayer) => {
                    p.id == ability.scoped_player.unwrap_or(ability.controller)
                }
                // CR 109.4: TargetPlayer is ambiguous here (player targets are
                // resolved from ability.targets directly); fail closed.
                Some(ControllerRef::TargetPlayer) => false,
                Some(ControllerRef::ParentTargetController) => false,
                Some(ControllerRef::ParentTargetOwner) => false,
                Some(ControllerRef::DefendingPlayer) => false,
                // CR 613.1: no card scopes this shape to a persisted chosen
                // player; fail closed (mirrors DefendingPlayer).
                Some(ControllerRef::SourceChosenPlayer) => false,
                // CR 608.2c + CR 109.4: Player chosen by an earlier
                // `Choose(Player)` in this resolution.
                Some(ControllerRef::ChosenPlayer { index }) => {
                    ability.chosen_players.get(*index as usize).copied() == Some(p.id)
                }
                // CR 603.2 + CR 109.4: The triggering player. Resolved against
                // the current trigger event; fail closed when there is none.
                Some(ControllerRef::TriggeringPlayer) => {
                    state
                        .current_trigger_event
                        .as_ref()
                        .and_then(|e| targeting::extract_player_from_event(e, state))
                        == Some(p.id)
                }
                None => true,
            })
            .map(|p| p.id)
            .collect(),
        _ => Vec::new(),
    }
}

fn target_creature_quantity_slot_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn parent_target_combat_relation_slot_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn effect_needs_parent_target_combat_relation_slot(effect: &Effect) -> bool {
    effect_references_parent_target_combat_relation(effect)
}

fn effect_needs_target_creature_quantity_slot(effect: &Effect) -> bool {
    effect_references_target_creature_quantity(effect)
        && !effect_primary_target_supplies_creature_target(effect)
}

fn effect_references_parent_target_combat_relation(effect: &Effect) -> bool {
    if effect
        .target_filter()
        .is_some_and(filter_references_parent_target_combat_relation)
    {
        return true;
    }

    match effect {
        Effect::DestroyAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::SetTapState {
            scope: EffectScope::All,
            target,
            ..
        }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::ChangeZoneAll { target, .. }
        | Effect::DoublePTAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::PutCounterAll { target, .. } => {
            filter_references_parent_target_combat_relation(target)
        }
        _ => false,
    }
}

fn filter_references_parent_target_combat_relation(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties.iter().any(|prop| {
            matches!(
                prop,
                FilterProp::CombatRelation {
                    subject: CombatRelationSubject::ParentTarget,
                    ..
                }
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(filter_references_parent_target_combat_relation),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            filter_references_parent_target_combat_relation(filter)
        }
        _ => false,
    }
}

fn effect_primary_target_supplies_creature_target(effect: &Effect) -> bool {
    triggers::extract_target_filter_from_effect(effect)
        .is_some_and(target_filter_can_supply_creature_quantity)
}

fn target_filter_can_supply_creature_quantity(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Any | TargetFilter::Typed(_) | TargetFilter::SpecificObject { .. }
    )
}

fn effect_references_target_creature_quantity(effect: &Effect) -> bool {
    if effect
        .target_filter()
        .is_some_and(filter_references_target_creature_quantity)
    {
        return true;
    }

    match effect {
        Effect::GainLife { amount, .. }
        | Effect::Draw { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::Discard { count: amount, .. }
        | Effect::Scry { count: amount, .. }
        | Effect::Surveil { count: amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::SetLifeTotal { amount, .. }
        | Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::PutCounter { count: amount, .. }
        | Effect::PutCounterAll { count: amount, .. }
        | Effect::Sacrifice { count: amount, .. } => {
            quantity_expr_references_target_creature(amount)
        }
        Effect::DestroyAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::SetTapState {
            scope: EffectScope::All,
            target,
            ..
        }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::ChangeZoneAll { target, .. }
        | Effect::DoublePTAll { target, .. } => filter_references_target_creature_quantity(target),
        _ => false,
    }
}

fn filter_references_target_creature_quantity(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties
            .iter()
            .any(filter_prop_references_target_creature_quantity),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(filter_references_target_creature_quantity),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            filter_references_target_creature_quantity(filter)
        }
        _ => false,
    }
}

fn filter_prop_references_target_creature_quantity(
    prop: &crate::types::ability::FilterProp,
) -> bool {
    match prop {
        crate::types::ability::FilterProp::Counters { count, .. }
        | crate::types::ability::FilterProp::Cmc { value: count, .. }
        | crate::types::ability::FilterProp::PtComparison { value: count, .. } => {
            quantity_expr_references_target_creature(count)
        }
        crate::types::ability::FilterProp::CanEnchant { target } => {
            filter_references_target_creature_quantity(target)
        }
        crate::types::ability::FilterProp::AnyOf { props } => props
            .iter()
            .any(filter_prop_references_target_creature_quantity),
        // CR 608.2c: Negation reads the inner prop's references — recurse (mirrors AnyOf).
        crate::types::ability::FilterProp::Not { prop } => {
            filter_prop_references_target_creature_quantity(prop)
        }
        crate::types::ability::FilterProp::DifferentNameFrom { filter } => {
            filter_references_target_creature_quantity(filter)
        }
        crate::types::ability::FilterProp::SharesQuality { reference, .. } => reference
            .as_deref()
            .is_some_and(filter_references_target_creature_quantity),
        crate::types::ability::FilterProp::TargetsOnly { filter }
        | crate::types::ability::FilterProp::Targets { filter } => {
            filter_references_target_creature_quantity(filter)
        }
        _ => false,
    }
}

fn quantity_expr_references_target_creature(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => quantity_ref_references_target_creature(qty),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_references_target_creature(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_expr_references_target_creature),
        QuantityExpr::Difference { left, right } => {
            quantity_expr_references_target_creature(left)
                || quantity_expr_references_target_creature(right)
        }
        QuantityExpr::Fixed { .. } => false,
    }
}

fn quantity_ref_references_target_creature(qty: &QuantityRef) -> bool {
    match qty {
        QuantityRef::Power { scope } | QuantityRef::Toughness { scope } => {
            *scope == ObjectScope::Target
        }
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. }
        | QuantityRef::EnteredThisTurn { filter }
        | QuantityRef::SacrificedThisTurn { filter, .. }
        | QuantityRef::ZoneChangeCountThisTurn { filter, .. }
        | QuantityRef::ZoneChangeAggregateThisTurn { filter, .. }
        | QuantityRef::CounterAddedThisTurn { target: filter, .. }
        | QuantityRef::TokensCreatedThisTurn { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter }
        | QuantityRef::DistinctCounterKindsAmong { filter } => {
            filter_references_target_creature_quantity(filter)
        }
        QuantityRef::SpellsCastThisTurn { filter, .. }
        | QuantityRef::SpellsCastThisGame { filter, .. } => filter
            .as_ref()
            .is_some_and(filter_references_target_creature_quantity),
        QuantityRef::DamageDealtThisTurn { source, target, .. } => {
            filter_references_target_creature_quantity(source)
                || filter_references_target_creature_quantity(target)
        }
        QuantityRef::DistinctCardTypes { source } => match source {
            CardTypeSetSource::Objects { filter } => {
                filter_references_target_creature_quantity(filter)
            }
            CardTypeSetSource::Zone { .. }
            | CardTypeSetSource::ExiledBySource
            | CardTypeSetSource::TrackedSet { .. } => false,
        },
        QuantityRef::ManaSpentToCast { metric, .. } => match metric {
            CastManaSpentMetric::FromSource { source_filter } => {
                filter_references_target_creature_quantity(source_filter)
            }
            CastManaSpentMetric::Total | CastManaSpentMetric::DistinctColors => false,
        },
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::ControlsCount { filter, .. },
        } => filter_references_target_creature_quantity(filter),
        // CR 402.1 / 119.1 / 122.1f / 404.1: a player-scalar predicate is read
        // off each candidate player, never off a target creature, so it cannot
        // reference the resolving ability's target-creature slot.
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::PlayerAttribute { .. },
        } => false,
        _ => false,
    }
}

fn collect_target_slot_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &mut Vec<TargetSlotSpec>,
    next_instance: &mut usize,
) {
    if let Some(sub_ability) = ability.sub_ability.as_deref().filter(|sub| {
        matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    }) {
        if ability.context.additional_cost_paid {
            collect_target_slot_specs(state, sub_ability, specs, next_instance);
            return;
        }
    }

    // CR 609.7 + CR 601.2c: Mirror the source-scoped `PreventDamage` slot from
    // `collect_target_slots` one-for-one so per-slot specs line up with the
    // surfaced TargetSelectionSlots (the choosable source spell, declared first).
    if ability.target_choice_timing == TargetChoiceTiming::Stack {
        if let Some(src_leaf) = prevent_damage_source_slot_filter(&ability.effect) {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: src_leaf.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
    }

    // CR 701.12a: Mirror the ExchangeControl branch in `collect_target_slots`
    // so per-slot specs match the surfaced TargetSelectionSlots one-for-one
    // (SelfRef slots are auto-resolved and not surfaced).
    if let Effect::ExchangeControl { target_a, target_b } = &ability.effect {
        for filter in [target_a, target_b] {
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
        return;
    }

    // CR 701.12a: Mirror the ExchangeLifeTotals branch in `collect_target_slots`
    // so per-slot specs match the surfaced TargetSelectionSlots one-for-one
    // (context-ref slots like Controller are auto-resolved and not surfaced).
    if let Effect::ExchangeLifeTotals { player_a, player_b } = &ability.effect {
        for filter in [player_a, player_b] {
            if filter.is_context_ref() {
                continue;
            }
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
        return;
    }

    if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &ability.effect
    {
        for filter in move_counter_stack_target_filters(source, target, *selection) {
            if !filter.is_context_ref() {
                let id = TargetInstanceId(*next_instance);
                *next_instance += 1;
                specs.push(TargetSlotSpec {
                    filter: filter.clone(),
                    optional: ability.optional_targeting,
                    instance: id,
                });
            }
        }
    } else if let Effect::Attach { attachment, target } = &ability.effect {
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if attach_side_needs_target_slot(filter, is_attachment) {
                let id = TargetInstanceId(*next_instance);
                *next_instance += 1;
                specs.push(TargetSlotSpec {
                    filter: filter.clone(),
                    optional: ability.optional_targeting,
                    instance: id,
                });
            }
        }
    } else if let Effect::CreateDamageReplacement {
        recipient_object_filter,
        redirect_object_filter,
        ..
    } = &ability.effect
    {
        // CR 115.1 + CR 614.9: Mirror `collect_target_slots` one-for-one — the
        // recipient slot (Jade Monolith) before the redirect slot (Soltari) — so
        // per-slot specs line up with the surfaced TargetSelectionSlots.
        for filter in [recipient_object_filter, redirect_object_filter]
            .into_iter()
            .flatten()
        {
            // CR 614.9: mirror `collect_target_slots` — a `SelfRef` self
            // recipient (en-Kor) surfaces no slot, so it gets no spec either.
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
    } else {
        if is_per_opponent_target_fanout(ability) {
            collect_per_opponent_target_fanout_specs(state, ability, specs, next_instance);
            if let Some(sub_ability) = ability.sub_ability.as_deref() {
                if !defers_conditional_target_selection(sub_ability) {
                    collect_target_slot_specs(state, sub_ability, specs, next_instance);
                }
            }
            return;
        }
        // CR 109.4 + CR 115.1: Companion TargetFilter::Player slot surfaced by
        // `collect_target_slots` must have a matching spec here so subsequent
        // slot recomputation treats it correctly.
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && ability_needs_companion_target_player_slot(ability)
        {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: TargetFilter::Player,
                optional: ability.optional_targeting,
                instance: id,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_target_creature_quantity_slot(&ability.effect)
        {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: target_creature_quantity_slot_filter(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: parent_target_combat_relation_slot_filter(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack {
            if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
                if let Some(spec) = ability.multi_target.as_ref() {
                    let legal_targets =
                        legal_targets_for_ability_filter(state, ability, filter, &[]);
                    if let Ok(bounds) =
                        resolve_multi_target_bounds(state, ability, spec, legal_targets.len())
                    {
                        // CR 601.2c + CR 115.3: all slots of one "up to N target
                        // creatures" run are ONE instance of "target" -> one shared
                        // TargetInstanceId. Allocate it once before the loop and
                        // stamp every spec in the run so the same object can't be
                        // chosen into two of these slots.
                        let id = TargetInstanceId(*next_instance);
                        *next_instance += 1;
                        for slot_index in 0..bounds.max {
                            specs.push(TargetSlotSpec {
                                filter: filter.clone(),
                                optional: slot_index >= bounds.min,
                                instance: id,
                            });
                        }
                    }
                } else {
                    let id = TargetInstanceId(*next_instance);
                    *next_instance += 1;
                    specs.push(TargetSlotSpec {
                        filter: filter.clone(),
                        optional: ability.optional_targeting,
                        instance: id,
                    });
                }
            }
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        collect_target_slot_specs_after_deferred_effect(
            state,
            ability.sub_ability.as_deref(),
            specs,
            next_instance,
        );
        return;
    }
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        if !defers_conditional_target_selection(sub_ability) {
            collect_target_slot_specs(state, sub_ability, specs, next_instance);
        }
    }
}

/// CR 601.2c / CR 602.2b: Targets are chosen before costs are paid. This
/// engine pays a non-self Sacrifice/Discard/Exile activation cost BEFORE
/// target selection as a documented architectural shortcut (see the ordering
/// note in `push_activated_ability_to_stack`), so the object that cost just
/// moved off the battlefield must not become newly eligible for an unrelated
/// target slot just because it now sits in the destination zone. Cauldron of
/// Essence's official ruling states this explicitly: "the target ... can't be
/// the creature sacrificed to pay its cost." Costs that leave the object on
/// the battlefield (Tap, Blight, RemoveCounter) never made it newly eligible
/// for a different zone, so they are correctly left untouched by this gate.
fn exclude_cost_paid_object_that_left_battlefield(
    state: &GameState,
    ability: &ResolvedAbility,
    targets: Vec<TargetRef>,
) -> Vec<TargetRef> {
    let Some(snapshot) = ability.cost_paid_object.as_ref() else {
        return targets;
    };
    let left_battlefield = match state.objects.get(&snapshot.object_id) {
        Some(obj) => obj.zone != Zone::Battlefield,
        None => true,
    };
    if !left_battlefield {
        return targets;
    }
    targets
        .into_iter()
        .filter(|target| !matches!(target, TargetRef::Object(id) if *id == snapshot.object_id))
        .collect()
}

fn legal_targets_for_ability_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    existing_slots: &[TargetSelectionSlot],
) -> Vec<TargetRef> {
    exclude_cost_paid_object_that_left_battlefield(
        state,
        ability,
        legal_targets_for_ability_filter_uncapped(state, ability, filter, existing_slots),
    )
}

fn legal_targets_for_ability_filter_uncapped(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    existing_slots: &[TargetSelectionSlot],
) -> Vec<TargetRef> {
    if let Some(targets) = damage_any_target_legal_targets(state, ability, filter) {
        return targets;
    }

    let needs_ability_context = target_filter_contains_chosen_x_ref(filter);
    let relative_kind = relative_controller_kind(filter);
    if relative_kind.is_none() {
        if needs_ability_context {
            return targeting::find_legal_targets_for_ability(state, filter, ability);
        }
        return targeting::find_legal_targets(state, filter, ability.controller, ability.source_id);
    }

    let Some(player_slot) = existing_slots.iter().rev().find(|slot| {
        !slot.legal_targets.is_empty()
            && slot
                .legal_targets
                .iter()
                .all(|target| matches!(target, TargetRef::Player(_)))
    }) else {
        if needs_ability_context {
            return targeting::find_legal_targets_for_ability(state, filter, ability);
        }
        return targeting::find_legal_targets(state, filter, ability.controller, ability.source_id);
    };

    // CR 109.4 + CR 115.1: For each candidate from the companion player slot,
    // re-enumerate with the relative controller bound to that player. The
    // filter is rewritten to `ControllerRef::You` so `find_legal_targets`'s
    // existing source-controller plumbing handles per-player substitution
    // uniformly for both the `You` (per-player iteration) and `TargetPlayer`
    // (Karazikar-style attacked-player) cases.
    let enumeration_filter = match relative_kind {
        Some(crate::types::ability::ControllerRef::TargetPlayer) => rewrite_relative_controller(
            filter,
            crate::types::ability::ControllerRef::TargetPlayer,
            crate::types::ability::ControllerRef::You,
        ),
        _ => filter.clone(),
    };

    let mut legal_targets = Vec::new();
    for player_id in player_slot
        .legal_targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Player(player_id) => Some(*player_id),
            TargetRef::Object(_) => None,
        })
    {
        let targets = if needs_ability_context {
            targeting::find_legal_targets_for_ability_with_controller(
                state,
                &enumeration_filter,
                ability,
                player_id,
            )
        } else {
            targeting::find_legal_targets(state, &enumeration_filter, player_id, ability.source_id)
        };
        for target in targets {
            if !legal_targets.contains(&target) {
                legal_targets.push(target);
            }
        }
    }

    legal_targets
}

/// Returns the relative `ControllerRef` (`You` or `TargetPlayer`) embedded in
/// `filter`, if any. Used by `legal_targets_for_ability_filter` (static slot
/// build) and `legal_targets_for_selected_slot` (selection-time recompute) to
/// detect filters that need per-player re-enumeration against the player chosen
/// in a companion `TargetFilter::Player` slot.
fn relative_controller_kind(filter: &TargetFilter) -> Option<crate::types::ability::ControllerRef> {
    use crate::types::ability::ControllerRef;
    match filter {
        TargetFilter::Typed(tf) => match tf.controller {
            Some(ControllerRef::You) => Some(ControllerRef::You),
            Some(ControllerRef::TargetPlayer) => Some(ControllerRef::TargetPlayer),
            _ => tf.properties.iter().find_map(|prop| match prop {
                FilterProp::Owned {
                    controller: ControllerRef::You,
                } => Some(ControllerRef::You),
                FilterProp::Owned {
                    controller: ControllerRef::TargetPlayer,
                } => Some(ControllerRef::TargetPlayer),
                _ => None,
            }),
        },
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(relative_controller_kind)
        }
        TargetFilter::Not { filter } => relative_controller_kind(filter),
        _ => None,
    }
}

/// CR 702.5a + CR 303.4: When `spec` is the host slot of an `Effect::Attach`
/// whose `attachment` resolves to an object, return the host filter that object's
/// Enchant keyword imposes, plus the attachment id/controller. `None` = no
/// restriction (not an Attach, attachment unresolved, or aura_enchant_filter
/// returned None: not an Aura / Aura with no Enchant keyword). No restriction ⇒
/// ANY battlefield permanent is legal (CR 702.5a; mirrors the no-Enchant
/// else-branch in sba::is_valid_attachment_target).
fn attach_host_enchant_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &TargetSlotSpec,
    selected_slots: &[Option<TargetRef>],
) -> Option<(TargetFilter, ObjectId, PlayerId)> {
    // Walk the effect + sub_ability chain (mirrors collect_target_slot_specs) to
    // find the Attach whose host `target` filter is the one we're enumerating.
    let mut current = Some(ability);
    let mut attachment_filter: Option<&TargetFilter> = None;
    while let Some(node) = current {
        if let Effect::Attach { attachment, target } = &node.effect {
            if target == &spec.filter {
                attachment_filter = Some(attachment);
                break;
            }
        }
        current = node.sub_ability.as_deref();
    }
    let attachment_filter = attachment_filter?;

    // Resolve the attachment (the moved Aura) to a concrete object id.
    let attachment_id = match attachment_filter {
        TargetFilter::SelfRef => ability.source_id,
        TargetFilter::ParentTarget => selected_slots.iter().find_map(|sel| match sel {
            Some(TargetRef::Object(id)) => Some(*id),
            _ => None,
        })?,
        _ => return None,
    };

    let filter = crate::game::effects::change_targets::aura_enchant_filter(state, attachment_id)?;
    let controller = state.objects.get(&attachment_id)?.controller;
    Some((filter, attachment_id, controller))
}

fn is_per_opponent_target_fanout(ability: &ResolvedAbility) -> bool {
    if ability.target_choice_timing != TargetChoiceTiming::Stack {
        return false;
    }
    if ability
        .effect
        .target_filter()
        .and_then(relative_controller_kind)
        != Some(ControllerRef::TargetPlayer)
    {
        return false;
    }
    matches!(
        ability
            .multi_target
            .as_ref()
            .and_then(|spec| spec.max.as_ref()),
        Some(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent
            }
        })
    )
}

fn per_opponent_fanout_players(state: &GameState, controller: PlayerId) -> Vec<PlayerId> {
    players::apnap_order_from(state, None, controller)
        .into_iter()
        .filter(|id| {
            *id != controller
                && state.players.iter().any(|player| {
                    player.id == *id && !player.is_eliminated && !player.is_phased_out()
                })
        })
        .collect()
}

fn per_opponent_fanout_constraint_targets(
    state: &GameState,
    controller: PlayerId,
    opponent: PlayerId,
) -> Vec<TargetRef> {
    if per_opponent_fanout_players(state, controller).contains(&opponent) {
        vec![TargetRef::Player(opponent)]
    } else {
        Vec::new()
    }
}

fn per_opponent_fanout_object_filter(ability: &ResolvedAbility) -> Option<TargetFilter> {
    ability.effect.target_filter().map(|filter| {
        rewrite_relative_controller(filter, ControllerRef::TargetPlayer, ControllerRef::You)
    })
}

fn collect_per_opponent_target_fanout_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    acc: &mut SlotAccumulator,
) -> Result<(), EngineError> {
    let Some(object_filter) = per_opponent_fanout_object_filter(ability) else {
        return Ok(());
    };

    for opponent in per_opponent_fanout_players(state, ability.controller) {
        let legal_targets =
            targeting::find_legal_targets(state, &object_filter, opponent, ability.source_id);
        if legal_targets.is_empty() {
            if ability.targeting_is_optional() {
                // CR 115.1 + CR 603.3d: "Up to one" per-opponent fanout — an
                // opponent with no legal targets contributes no slots. Omitting
                // both the player slot and the creature slot avoids presenting
                // the player with an empty selection step they cannot act on.
                continue;
            }
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available".to_string(),
            ));
        }
        let player_targets =
            per_opponent_fanout_constraint_targets(state, ability.controller, opponent);
        acc.push(TargetSelectionSlot {
            legal_targets: player_targets,
            optional: false,
        });
        acc.push(TargetSelectionSlot {
            legal_targets,
            optional: ability.targeting_is_optional(),
        });
    }

    Ok(())
}

fn collect_per_opponent_target_fanout_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &mut Vec<TargetSlotSpec>,
    next_instance: &mut usize,
) {
    let Some(object_filter) = per_opponent_fanout_object_filter(ability) else {
        return;
    };

    for opponent in per_opponent_fanout_players(state, ability.controller) {
        // CR 115.1 + CR 603.3d: Mirror the slot-builder: skip opponents whose
        // creature pool is empty when targeting is optional so specs and slots
        // stay in lockstep.
        if ability.targeting_is_optional()
            && targeting::find_legal_targets(state, &object_filter, opponent, ability.source_id)
                .is_empty()
        {
            continue;
        }
        // CR 601.2c + CR 115.3: per-opponent fanout slots are SEPARATE instances
        // of "target" — the Player slot and the object slot each get their own
        // fresh TargetInstanceId so they never cross-constrain each other.
        let player_id = TargetInstanceId(*next_instance);
        *next_instance += 1;
        let object_id = TargetInstanceId(*next_instance);
        *next_instance += 1;
        specs.push(TargetSlotSpec {
            filter: TargetFilter::SpecificPlayer { id: opponent },
            optional: false,
            instance: player_id,
        });
        specs.push(TargetSlotSpec {
            filter: object_filter.clone(),
            optional: ability.targeting_is_optional(),
            instance: object_id,
        });
    }
}

fn validate_per_opponent_target_fanout_targets(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    let Some(object_filter) = per_opponent_fanout_object_filter(ability) else {
        return Vec::new();
    };

    let mut current_player = None;
    let mut legal = Vec::new();
    for target in &ability.targets {
        match target {
            TargetRef::Player(player_id) => current_player = Some(*player_id),
            TargetRef::Object(object_id) => {
                let Some(player_id) = current_player else {
                    continue;
                };
                let legal_targets = targeting::find_legal_targets(
                    state,
                    &object_filter,
                    player_id,
                    ability.source_id,
                );
                if legal_targets.contains(target) {
                    legal.push(TargetRef::Object(*object_id));
                }
            }
        }
    }
    legal
}

fn object_targets_only(targets: &[TargetRef]) -> Vec<TargetRef> {
    targets
        .iter()
        .filter(|target| matches!(target, TargetRef::Object(_)))
        .cloned()
        .collect()
}

/// Substitute every `from`-controller binding in `filter` with `to`. Used to
/// rewrite `TargetPlayer` → `You` so per-player enumeration through
/// `find_legal_targets`'s `source_controller` parameter works uniformly.
fn rewrite_relative_controller(
    filter: &TargetFilter,
    from: crate::types::ability::ControllerRef,
    to: crate::types::ability::ControllerRef,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => {
            let mut new_tf = tf.clone();
            if new_tf.controller == Some(from.clone()) {
                new_tf.controller = Some(to.clone());
            }
            for prop in &mut new_tf.properties {
                if let FilterProp::Owned { controller } = prop {
                    if *controller == from {
                        *controller = to.clone();
                    }
                }
            }
            TargetFilter::Typed(new_tf)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|f| rewrite_relative_controller(f, from.clone(), to.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|f| rewrite_relative_controller(f, from.clone(), to.clone()))
                .collect(),
        },
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(rewrite_relative_controller(inner, from, to)),
        },
        other => other.clone(),
    }
}

fn target_slot_specs(state: &GameState, ability: &ResolvedAbility) -> Vec<TargetSlotSpec> {
    let mut specs = Vec::new();
    // CR 601.2c + CR 115.3: instance ids are allocated densely from 0 as specs
    // are collected; each fresh-id push site bumps the seed.
    let mut next_instance = 0usize;
    collect_target_slot_specs(state, ability, &mut specs, &mut next_instance);
    specs
}

fn relative_filter_controller(
    ability: &ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
) -> PlayerId {
    selected_slots
        .iter()
        .rev()
        .find_map(|slot| match slot {
            Some(TargetRef::Player(player_id)) => Some(*player_id),
            Some(TargetRef::Object(_)) | None => None,
        })
        .unwrap_or(ability.controller)
}

/// CR 115.4 + CR 601.2c: "other target" / "another target" filters require
/// a different choice from the targets already announced for this spell/ability.
fn target_filter_has_another_target_marker(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(target_filter_has_another_target_marker)
        }
        TargetFilter::TrackedSetFiltered { filter, .. } => {
            target_filter_has_another_target_marker(filter)
        }
        _ => false,
    }
}

/// Compute the legal targets for one slot, then drop any object already chosen
/// in a prior slot of the SAME instance of "target".
///
/// `prior_specs` are the specs for the slots before `spec` (i.e. `&specs[..i]`);
/// `selected_slots` are the corresponding prior selections (same length and
/// order). The two are zipped so we can tell which prior selections belong to
/// `spec.instance`. Callers must pass a `prior_specs`/`selected_slots` pair that
/// lines up one-for-one.
///
/// CR 601.2c + CR 115.3 NOTE: the parallel SLOT-ONLY lattice
/// (`legal_targets_for_slot` / `has_legal_completion` /
/// `validate_selected_slot_prefix`, and the `choose_target` entry point) is
/// intentionally NOT given this per-instance distinctness filter. That lattice
/// is only reached for single-target Aura casts and test fixtures — never a
/// `multi_target` same-instance group — so there is no same-instance pair for
/// it to over-share. This is a deliberate scoping choice, not an enforcement
/// gap: distinctness lives in the spec-aware lattice that the multi_target path
/// (Mothman et al.) actually flows through.
fn legal_targets_for_selected_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &TargetSlotSpec,
    prior_specs: &[TargetSlotSpec],
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    // CR 120.3a + CR 603.7c: The companion `TargetFilter::Player` slot for a
    // damage-to-player trigger binds "that player" to the damaged player carried
    // by the triggering event, not a free choice among every player. This is the
    // selection-time recompute that feeds legal-action generation; without it the
    // slot would be re-offered as all players (overriding the static slot built
    // in `collect_target_slots`) and the dependent "creatures that player
    // controls" slot would have no satisfiable combination, hanging the
    // controller in multiplayer. The constraint itself is gated inside the
    // helper, so non-damage-trigger Player slots still offer every player.
    if matches!(spec.filter, TargetFilter::Player)
        && ability_needs_companion_target_player_slot(ability)
    {
        return companion_target_player_legal_targets(state, ability);
    }
    // Each branch computes the raw legal set into `legal`; the per-instance
    // distinctness filter (CR 601.2c + CR 115.3) is then applied ONCE at the
    // single tail return. For single-target / separate-instance / early-return
    // cases `already_in_instance` is empty, so the tail filter is a no-op.
    let per_opponent_fanout_targets = if is_per_opponent_target_fanout(ability) {
        if let TargetFilter::SpecificPlayer { id } = spec.filter {
            Some(per_opponent_fanout_constraint_targets(
                state,
                ability.controller,
                id,
            ))
        } else {
            None
        }
    } else {
        None
    };

    let mut legal: Vec<TargetRef> = if matches!(ability.effect, Effect::PairWith { .. }) {
        pair_with_legal_choices(state, ability, &spec.filter)
    } else if let Some(targets) = damage_any_target_legal_targets(state, ability, &spec.filter) {
        targets
    } else if let Some(targets) = per_opponent_fanout_targets {
        targets
    } else {
        // CR 109.4 + CR 115.1: A filter scoped to a *relative* controller —
        // `You` ("creatures you control") or `TargetPlayer` ("creatures that
        // player controls") — is re-bound to the player chosen in a prior slot
        // (the companion `TargetFilter::Player` slot, or an `Effect::Choose`).
        // `relative_filter_controller` reads that player back from
        // `selected_slots`. For the `TargetPlayer` case the filter is also
        // rewritten to `You` so `find_legal_targets`' source-controller plumbing
        // resolves it — at selection time `ability.targets` is still empty, so
        // filter.rs' `TargetPlayer` lookup (which reads `ability.targets`) would
        // fail closed and collapse the dependent slot to empty, hanging
        // legal-action generation. This mirrors the static
        // `legal_targets_for_ability_filter` path so both agree.
        let relative_kind = relative_controller_kind(&spec.filter);
        let controller = if relative_kind.is_some() {
            relative_filter_controller(ability, selected_slots)
        } else {
            ability.controller
        };
        let enumeration_filter = match relative_kind {
            Some(ControllerRef::TargetPlayer) => rewrite_relative_controller(
                &spec.filter,
                ControllerRef::TargetPlayer,
                ControllerRef::You,
            ),
            _ => spec.filter.clone(),
        };

        if target_filter_contains_chosen_x_ref(&enumeration_filter) {
            if controller == ability.controller {
                targeting::find_legal_targets_for_ability(state, &enumeration_filter, ability)
            } else {
                targeting::find_legal_targets_for_ability_with_controller(
                    state,
                    &enumeration_filter,
                    ability,
                    controller,
                )
            }
        } else {
            targeting::find_legal_targets(state, &enumeration_filter, controller, ability.source_id)
        }
    };

    // CR 702.5a + CR 303.4j: An Aura being attached may only go to a host it can
    // legally enchant. Restrict offered hosts to those matching the moved aura's own
    // Enchant filter; no Enchant keyword => no restriction (any host).
    if let Some((enchant_filter, aura_id, aura_controller)) =
        attach_host_enchant_filter(state, ability, spec, selected_slots)
    {
        let ctx = crate::game::filter::FilterContext::from_source_with_controller(
            aura_id,
            aura_controller,
        );
        legal.retain(|t| match t {
            TargetRef::Object(id) => {
                crate::game::filter::matches_target_filter(state, *id, &enchant_filter, &ctx)
            }
            TargetRef::Player(pid) => crate::game::filter::player_matches_target_filter_in_state(
                state,
                &enchant_filter,
                *pid,
                Some(aura_controller),
            ),
        });
    }

    // CR 601.2c + CR 115.3: within one instance of "target", the same object
    // can't be chosen twice. Remove objects already chosen in prior slots of
    // THIS instance. Prior slots of a DIFFERENT instance (separate "target") do
    // not constrain this slot — they may legally reuse the same object
    // (CR 601.2c "Destroy target artifact and target land" Example).
    let already_in_instance: std::collections::HashSet<ObjectId> = prior_specs
        .iter()
        .zip(selected_slots)
        .filter(|(prior, _)| prior.instance == spec.instance)
        .filter_map(|(_, sel)| match sel {
            Some(TargetRef::Object(id)) => Some(*id),
            _ => None,
        })
        .collect();
    legal.retain(|t| !matches!(t, TargetRef::Object(id) if already_in_instance.contains(id)));

    // CR 115.4: "other target" / "another target" is a separate instance of
    // "target" but must differ from every target already chosen for this
    // spell/ability.
    if target_filter_has_another_target_marker(&spec.filter) {
        for prior in selected_slots.iter().flatten() {
            legal.retain(|t| t != prior);
        }
    }
    exclude_cost_paid_object_that_left_battlefield(state, ability, legal)
}

fn damage_any_target_legal_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<Vec<TargetRef>> {
    if !matches!(
        (&ability.effect, filter),
        (
            Effect::DealDamage {
                target: TargetFilter::Any,
                ..
            },
            TargetFilter::Any
        )
    ) {
        return None;
    }

    let player_targets = targeting::find_legal_targets(
        state,
        &TargetFilter::Player,
        ability.controller,
        ability.source_id,
    );
    let permanent_targets = targeting::find_legal_targets(
        state,
        &TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(vec![
            TypeFilter::Creature,
            TypeFilter::Planeswalker,
            TypeFilter::Battle,
        ]))),
        ability.controller,
        ability.source_id,
    );

    Some(
        player_targets
            .into_iter()
            .chain(permanent_targets)
            .collect(),
    )
}

/// CR 603.12: Check if a sub-ability represents a reflexive trigger whose targeting
/// should be deferred to resolution time. Reflexive trigger conditions (WhenYouDo,
/// QuantityCheck on CountersOnSelf) indicate the sub-ability fires as a separate
/// triggered ability during resolution — targets are chosen then, not at stack time.
fn defers_conditional_target_selection(sub: &ResolvedAbility) -> bool {
    matches!(
        &sub.condition,
        Some(AbilityCondition::WhenYouDo)
            | Some(AbilityCondition::QuantityCheck { .. })
            | Some(AbilityCondition::PreviousEffectAmount { .. })
            | Some(AbilityCondition::AdditionalCostPaidInstead)
    ) || sub.target_choice_timing == TargetChoiceTiming::Resolution
}

fn defers_sub_ability_target_selection(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Scry { .. }
            | Effect::Dig { .. }
            | Effect::Surveil { .. }
            | Effect::ChooseCard { .. }
            | Effect::SearchLibrary { .. }
            | Effect::RevealHand { .. }
            | Effect::Choose { .. }
    )
}

fn skips_stack_targets_after_deferred_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone { .. } | Effect::Shuffle { .. } | Effect::PutAtLibraryPosition { .. }
    )
}

fn collect_target_slots_after_deferred_effect(
    state: &GameState,
    sub_ability: Option<&ResolvedAbility>,
    acc: &mut SlotAccumulator,
) -> Result<(), EngineError> {
    let Some(sub_ability) = sub_ability else {
        return Ok(());
    };
    if defers_conditional_target_selection(sub_ability) {
        return Ok(());
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        return collect_target_slots_after_deferred_effect(
            state,
            sub_ability.sub_ability.as_deref(),
            acc,
        );
    }
    collect_target_slots(state, sub_ability, acc)
}

fn collect_target_slot_specs_after_deferred_effect(
    state: &GameState,
    sub_ability: Option<&ResolvedAbility>,
    specs: &mut Vec<TargetSlotSpec>,
    next_instance: &mut usize,
) {
    let Some(sub_ability) = sub_ability else {
        return;
    };
    if defers_conditional_target_selection(sub_ability) {
        return;
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        collect_target_slot_specs_after_deferred_effect(
            state,
            sub_ability.sub_ability.as_deref(),
            specs,
            next_instance,
        );
        return;
    }
    collect_target_slot_specs(state, sub_ability, specs, next_instance);
}

fn build_target_assignments(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    current: &mut Vec<TargetRef>,
    out: &mut Vec<Vec<TargetRef>>,
    limit: Option<usize>,
) {
    if limit.is_some_and(|limit| out.len() >= limit) {
        return;
    }

    if index == target_slots.len() {
        if validate_selected_targets(target_slots, current, constraints).is_ok() {
            out.push(current.clone());
        }
        return;
    }

    let slot = &target_slots[index];
    if slot.optional {
        build_target_assignments(target_slots, constraints, index + 1, current, out, limit);
    }
    for target in &slot.legal_targets {
        if limit.is_some_and(|limit| out.len() >= limit) {
            return;
        }
        current.push(target.clone());
        if validate_target_prefix(target_slots, current, constraints).is_ok() {
            build_target_assignments(target_slots, constraints, index + 1, current, out, limit);
        }
        current.pop();
    }
}

fn build_target_assignments_for_ability_with_limit(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    limit: Option<usize>,
) -> Vec<Vec<TargetRef>> {
    let specs = target_slot_specs(state, ability);
    let view = AbilityTargetingView {
        state,
        ability,
        specs: &specs,
        target_slots,
        constraints,
    };
    let mut current = Vec::with_capacity(target_slots.len());
    let mut out = Vec::new();
    build_target_assignments_with_specs(&view, 0, &mut current, &mut out, limit);
    out
}

fn build_target_assignments_with_specs(
    view: &AbilityTargetingView<'_>,
    index: usize,
    current: &mut Vec<TargetRef>,
    out: &mut Vec<Vec<TargetRef>>,
    limit: Option<usize>,
) {
    if limit.is_some_and(|limit| out.len() >= limit) {
        return;
    }

    if index == view.target_slots.len() {
        if validate_target_prefix_with_specs(
            view.state,
            view.ability,
            view.specs,
            view.target_slots,
            current,
            view.constraints,
        )
        .is_ok()
        {
            out.push(current.clone());
        }
        return;
    }

    let slot = &view.target_slots[index];
    if slot.optional {
        build_target_assignments_with_specs(view, index + 1, current, out, limit);
    }

    let selected_slots: Vec<Option<TargetRef>> = current.iter().cloned().map(Some).collect();
    let legal_targets = legal_targets_for_spec_slot(
        view.state,
        view.ability,
        view.specs,
        view.target_slots,
        index,
        &selected_slots,
    );
    for target in legal_targets {
        if limit.is_some_and(|limit| out.len() >= limit) {
            return;
        }
        current.push(target);
        if validate_target_prefix_with_specs(
            view.state,
            view.ability,
            view.specs,
            view.target_slots,
            current,
            view.constraints,
        )
        .is_ok()
        {
            build_target_assignments_with_specs(view, index + 1, current, out, limit);
        }
        current.pop();
    }
}

fn build_target_selection_progress(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: Vec<Option<TargetRef>>,
) -> Result<TargetSelectionProgress, EngineError> {
    if current_slot > target_slots.len() || selected_slots.len() != current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }
    validate_selected_slot_prefix(target_slots, &selected_slots, constraints)?;

    if current_slot == target_slots.len() {
        return Ok(TargetSelectionProgress {
            current_slot,
            selected_slots,
            current_legal_targets: Vec::new(),
        });
    }

    let current_legal_targets =
        legal_targets_for_slot(target_slots, constraints, current_slot, &selected_slots);
    let slot = &target_slots[current_slot];
    let mut skipped_slots = selected_slots.clone();
    skipped_slots.push(None);
    let can_skip = slot.optional
        && has_legal_completion(target_slots, constraints, current_slot + 1, &skipped_slots);

    if current_legal_targets.is_empty() && !can_skip {
        return Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        ));
    }

    Ok(TargetSelectionProgress {
        current_slot,
        selected_slots,
        current_legal_targets,
    })
}

fn build_target_selection_progress_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: Vec<Option<TargetRef>>,
) -> Result<TargetSelectionProgress, EngineError> {
    if current_slot > target_slots.len() || selected_slots.len() != current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }
    validate_selected_slots_for_ability(
        state,
        ability,
        target_slots,
        &selected_slots,
        constraints,
    )?;

    if current_slot == target_slots.len() {
        return Ok(TargetSelectionProgress {
            current_slot,
            selected_slots,
            current_legal_targets: Vec::new(),
        });
    }

    let specs = target_slot_specs(state, ability);
    let current_legal_targets = legal_targets_for_slot_with_specs(
        state,
        ability,
        &specs,
        target_slots,
        constraints,
        current_slot,
        &selected_slots,
    );
    let slot = &target_slots[current_slot];
    let mut skipped_slots = selected_slots.clone();
    skipped_slots.push(None);
    let can_skip = slot.optional
        && has_legal_completion_with_specs(
            state,
            ability,
            &specs,
            target_slots,
            constraints,
            current_slot + 1,
            &skipped_slots,
        );

    if current_legal_targets.is_empty() && !can_skip {
        return Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        ));
    }

    Ok(TargetSelectionProgress {
        current_slot,
        selected_slots,
        current_legal_targets,
    })
}

fn legal_targets_for_slot(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    let Some(slot) = target_slots.get(current_slot) else {
        return Vec::new();
    };

    slot.legal_targets
        .iter()
        .filter(|target| {
            let mut next_slots = selected_slots.to_vec();
            next_slots.push(Some((*target).clone()));
            validate_selected_slot_prefix(target_slots, &next_slots, constraints).is_ok()
                && has_legal_completion(target_slots, constraints, current_slot + 1, &next_slots)
        })
        .cloned()
        .collect()
}

fn has_legal_completion(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    selected_slots: &[Option<TargetRef>],
) -> bool {
    if index == target_slots.len() {
        return validate_selected_slot_prefix(target_slots, selected_slots, constraints).is_ok();
    }

    let slot = &target_slots[index];
    if slot.optional {
        let mut skipped_slots = selected_slots.to_vec();
        skipped_slots.push(None);
        if has_legal_completion(target_slots, constraints, index + 1, &skipped_slots) {
            return true;
        }
    }

    slot.legal_targets.iter().any(|target| {
        let mut next_slots = selected_slots.to_vec();
        next_slots.push(Some(target.clone()));
        validate_selected_slot_prefix(target_slots, &next_slots, constraints).is_ok()
            && has_legal_completion(target_slots, constraints, index + 1, &next_slots)
    })
}

fn legal_targets_for_spec_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    let Some(spec) = specs.get(current_slot) else {
        return target_slots
            .get(current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default();
    };
    // CR 601.2c + CR 115.3: pass the prior specs so same-instance distinctness
    // is enforced. `&specs[..current_slot]` lines up one-for-one with the prior
    // selections in `selected_slots` (both prefixes of length `current_slot`).
    legal_targets_for_selected_slot(state, ability, spec, &specs[..current_slot], selected_slots)
}

fn legal_targets_for_slot_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    legal_targets_for_spec_slot(
        state,
        ability,
        specs,
        target_slots,
        current_slot,
        selected_slots,
    )
    .into_iter()
    .filter(|target| {
        let mut next_slots = selected_slots.to_vec();
        next_slots.push(Some(target.clone()));
        validate_selected_slots_with_specs(
            state,
            ability,
            specs,
            target_slots,
            &next_slots,
            constraints,
        )
        .is_ok()
            && has_legal_completion_with_specs(
                state,
                ability,
                specs,
                target_slots,
                constraints,
                current_slot + 1,
                &next_slots,
            )
    })
    .collect()
}

fn has_legal_completion_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    selected_slots: &[Option<TargetRef>],
) -> bool {
    if index == target_slots.len() {
        return validate_selected_slots_with_specs(
            state,
            ability,
            specs,
            target_slots,
            selected_slots,
            constraints,
        )
        .is_ok();
    }

    let slot = &target_slots[index];
    if slot.optional {
        let mut skipped_slots = selected_slots.to_vec();
        skipped_slots.push(None);
        if has_legal_completion_with_specs(
            state,
            ability,
            specs,
            target_slots,
            constraints,
            index + 1,
            &skipped_slots,
        ) {
            return true;
        }
    }

    legal_targets_for_spec_slot(state, ability, specs, target_slots, index, selected_slots)
        .into_iter()
        .any(|target| {
            let mut next_slots = selected_slots.to_vec();
            next_slots.push(Some(target));
            validate_selected_slots_with_specs(
                state,
                ability,
                specs,
                target_slots,
                &next_slots,
                constraints,
            )
            .is_ok()
                && has_legal_completion_with_specs(
                    state,
                    ability,
                    specs,
                    target_slots,
                    constraints,
                    index + 1,
                    &next_slots,
                )
        })
}

fn validate_selected_slot_prefix(
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if selected_slots.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let mut compact_targets = Vec::new();
    for (index, selected_slot) in selected_slots.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };

        match selected_slot {
            Some(target) => {
                if !slot.legal_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
                compact_targets.push(target.clone());
            }
            None if slot.optional => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
    }

    validate_target_constraints(None, &compact_targets, constraints, None)
}

fn validate_target_prefix_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let specs = target_slot_specs(state, ability);
    validate_target_prefix_with_specs(state, ability, &specs, target_slots, targets, constraints)
}

fn validate_target_prefix_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let selected_slots: Vec<Option<TargetRef>> = targets.iter().cloned().map(Some).collect();
    validate_selected_slots_with_specs(
        state,
        ability,
        specs,
        target_slots,
        &selected_slots,
        constraints,
    )
}

fn validate_selected_slots_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let specs = target_slot_specs(state, ability);
    validate_selected_slots_with_specs(
        state,
        ability,
        &specs,
        target_slots,
        selected_slots,
        constraints,
    )
}

fn validate_selected_slots_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if selected_slots.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let mut compact_targets = Vec::new();
    for (index, selected_slot) in selected_slots.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };

        let legal_targets = specs
            .get(index)
            .map(|spec| {
                // CR 601.2c + CR 115.3: `&specs[..index]` (prior specs) lines up
                // one-for-one with `&selected_slots[..index]` (prior selections),
                // so the validate path enforces the same per-instance distinctness
                // as the offered-set path (`legal_targets_for_spec_slot`).
                legal_targets_for_selected_slot(
                    state,
                    ability,
                    spec,
                    &specs[..index],
                    &selected_slots[..index],
                )
            })
            .unwrap_or_else(|| slot.legal_targets.clone());

        match selected_slot {
            Some(target) => {
                if !legal_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
                compact_targets.push(target.clone());
            }
            None if slot.optional => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
    }

    validate_target_constraints(Some(state), &compact_targets, constraints, Some(ability))
}

fn assign_targets_recursive(
    state: &GameState,
    ability: &mut ResolvedAbility,
    targets: &[TargetRef],
    next_target: &mut usize,
) -> Result<(), EngineError> {
    if let Some(sub_ability) = ability.sub_ability.as_mut().filter(|sub| {
        matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    }) {
        if ability.context.additional_cost_paid {
            assign_targets_recursive(state, sub_ability, targets, next_target)?;
            ability.targets = sub_ability.targets.clone();
            return Ok(());
        }
    }

    if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &ability.effect
    {
        for filter in move_counter_stack_target_filters(source, target, *selection) {
            if !filter.is_context_ref() {
                if let Some(target) = targets.get(*next_target) {
                    ability.targets.push(target.clone());
                    *next_target += 1;
                } else if !ability.optional_targeting {
                    return Err(EngineError::InvalidAction(
                        "Missing required target".to_string(),
                    ));
                }
            }
        }
        if defers_sub_ability_target_selection(&ability.effect) {
            assign_targets_after_deferred_effect(
                state,
                ability.sub_ability.as_deref_mut(),
                targets,
                next_target,
            )?;
            return Ok(());
        }
        if let Some(sub_ability) = ability.sub_ability.as_mut() {
            if defers_conditional_target_selection(sub_ability) {
                return Ok(());
            }
            assign_targets_recursive(state, sub_ability, targets, next_target)?;
        }
        return Ok(());
    }

    if let Effect::Attach { attachment, target } = &ability.effect {
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if attach_side_needs_target_slot(filter, is_attachment) {
                if let Some(target) = targets.get(*next_target) {
                    ability.targets.push(target.clone());
                    *next_target += 1;
                } else if !ability.optional_targeting {
                    return Err(EngineError::InvalidAction(
                        "Missing required target".to_string(),
                    ));
                }
            }
        }
        if defers_sub_ability_target_selection(&ability.effect) {
            assign_targets_after_deferred_effect(
                state,
                ability.sub_ability.as_deref_mut(),
                targets,
                next_target,
            )?;
            return Ok(());
        }
        if let Some(sub_ability) = ability.sub_ability.as_mut() {
            if defers_conditional_target_selection(sub_ability) {
                return Ok(());
            }
            assign_targets_recursive(state, sub_ability, targets, next_target)?;
        }
        return Ok(());
    }

    // CR 609.7 + CR 601.2c: Mirror the source-scoped `PreventDamage` slot pushed
    // by `collect_target_slots`. The chosen source spell is consumed into THIS
    // node's `targets` (the PreventDamage HEAD node) BEFORE descending into the
    // sub-chain, so the modal sub (mode 3's PutCounter) consumes its own target
    // next. Slot order matches `collect_target_slots`: source slot first.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && prevent_damage_source_slot_filter(&ability.effect).is_some()
    {
        if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }

    // CR 109.4 + CR 115.1: Mirror the companion-player slot pushed by
    // `collect_target_slots` for effects whose filters reference
    // `ControllerRef::TargetPlayer` (DamageAll, PutCounterAll, etc.). The
    // selected player must be written onto THIS node's `targets` so the
    // filter's `TargetPlayer` resolution at runtime (filter.rs) finds it.
    // Slot order matches `collect_target_slots`: player slot before primary.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
    {
        if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
    {
        if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
    {
        if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && triggers::extract_target_filter_from_effect(&ability.effect).is_some()
    {
        if let Some(spec) = ability.multi_target.as_ref() {
            let remaining_minimum = ability
                .sub_ability
                .as_deref()
                .map(|sub| minimum_targets_in_chain(state, sub))
                .unwrap_or(0);
            let remaining_after_current = targets.len().saturating_sub(*next_target);
            // Issue #321: cap at this node's own resolved `multi_target` max so a
            // node does not claim a downstream `up to N` effect's optional
            // targets. Mirrors the cap in `assign_selected_slots_recursive`.
            let bounds = resolve_multi_target_bounds(state, ability, spec, remaining_after_current)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
            let current_count = remaining_after_current
                .saturating_sub(remaining_minimum)
                .min(bounds.max);
            if current_count < bounds.min {
                return Err(EngineError::InvalidAction(
                    "Incorrect number of multi-target selections".to_string(),
                ));
            }
            // CR 109.4: Use `extend_from_slice` so a companion player target
            // pushed by the `effect_references_target_player` branch above
            // survives — both slots live on this node's `targets`.
            ability
                .targets
                .extend_from_slice(&targets[*next_target..*next_target + current_count]);
            *next_target += current_count;
        } else if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        assign_targets_after_deferred_effect(
            state,
            ability.sub_ability.as_deref_mut(),
            targets,
            next_target,
        )?;
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        if defers_conditional_target_selection(sub_ability) {
            return Ok(());
        }
        assign_targets_recursive(state, sub_ability, targets, next_target)?;
    }
    Ok(())
}

fn assign_selected_slots_recursive(
    state: &GameState,
    ability: &mut ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
    next_slot: &mut usize,
) -> Result<(), EngineError> {
    if let Some(sub_ability) = ability.sub_ability.as_mut().filter(|sub| {
        matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    }) {
        if ability.context.additional_cost_paid {
            assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)?;
            ability.targets = sub_ability.targets.clone();
            return Ok(());
        }
    }

    if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &ability.effect
    {
        for filter in move_counter_stack_target_filters(source, target, *selection) {
            if !filter.is_context_ref() {
                let Some(selected_slot) = selected_slots.get(*next_slot) else {
                    return Err(EngineError::InvalidAction(
                        "Missing target selection".to_string(),
                    ));
                };
                match selected_slot {
                    Some(target) => ability.targets.push(target.clone()),
                    None if ability.optional_targeting => {}
                    None => {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    }
                }
                *next_slot += 1;
            }
        }
        if defers_sub_ability_target_selection(&ability.effect) {
            assign_selected_slots_after_deferred_effect(
                state,
                ability.sub_ability.as_deref_mut(),
                selected_slots,
                next_slot,
            )?;
            return Ok(());
        }
        if let Some(sub_ability) = ability.sub_ability.as_mut() {
            if defers_conditional_target_selection(sub_ability) {
                return Ok(());
            }
            assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)?;
        }
        return Ok(());
    }

    if let Effect::Attach { attachment, target } = &ability.effect {
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if attach_side_needs_target_slot(filter, is_attachment) {
                let Some(selected_slot) = selected_slots.get(*next_slot) else {
                    return Err(EngineError::InvalidAction(
                        "Missing target selection".to_string(),
                    ));
                };
                match selected_slot {
                    Some(target) => ability.targets.push(target.clone()),
                    None if ability.optional_targeting => {}
                    None => {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    }
                }
                *next_slot += 1;
            }
        }
        if defers_sub_ability_target_selection(&ability.effect) {
            assign_selected_slots_after_deferred_effect(
                state,
                ability.sub_ability.as_deref_mut(),
                selected_slots,
                next_slot,
            )?;
            return Ok(());
        }
        if let Some(sub_ability) = ability.sub_ability.as_mut() {
            if defers_conditional_target_selection(sub_ability) {
                return Ok(());
            }
            assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)?;
        }
        return Ok(());
    }

    // CR 609.7 + CR 601.2c: Mirror the source-scoped `PreventDamage` slot — the
    // modal cast pipeline drives the slots path, so the chosen source spell must
    // be consumed into THIS node's `targets` here too, BEFORE descending into the
    // (modal) sub-chain. Slot order matches `collect_target_slots`: source first.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && prevent_damage_source_slot_filter(&ability.effect).is_some()
    {
        let Some(selected_slot) = selected_slots.get(*next_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        match selected_slot {
            Some(target) => ability.targets.push(target.clone()),
            None if ability.optional_targeting => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }

    // CR 109.4 + CR 115.1: Mirror the companion-player slot pushed by
    // `collect_target_slots` for `ControllerRef::TargetPlayer` filters
    // (DamageAll, PutCounterAll, etc.). See `assign_targets_recursive`.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
    {
        let Some(selected_slot) = selected_slots.get(*next_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        match selected_slot {
            Some(target) => ability.targets.push(target.clone()),
            None if ability.optional_targeting => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
    {
        let Some(selected_slot) = selected_slots.get(*next_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        match selected_slot {
            Some(target) => ability.targets.push(target.clone()),
            None if ability.optional_targeting => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
    {
        let Some(selected_slot) = selected_slots.get(*next_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        match selected_slot {
            Some(target) => ability.targets.push(target.clone()),
            None if ability.optional_targeting => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && triggers::extract_target_filter_from_effect(&ability.effect).is_some()
    {
        if let Some(spec) = ability.multi_target.as_ref() {
            let remaining_minimum = ability
                .sub_ability
                .as_deref()
                .map(|sub| minimum_targets_in_chain(state, sub))
                .unwrap_or(0);
            let remaining_after_current = selected_slots.len().saturating_sub(*next_slot);
            // Issue #321: A multi-target node must consume only as many slots as
            // `collect_target_slots` produced for it — i.e. its own resolved
            // `multi_target` max (clamped to `spec.min`). Subtracting only the
            // sub-chain's *minimum* is not enough: when a downstream effect is
            // itself `up to N` (min 0), the current node would greedily claim
            // the sub-effect's optional slots too, applying its effect (e.g.
            // Betor's "+1/+1 counters" PutCounter) to the graveyard-return
            // target as well. Cap at this node's max so each effect resolves
            // against exactly its own chosen targets (CR 601.2c).
            let bounds = resolve_multi_target_bounds(state, ability, spec, remaining_after_current)
                .map_err(|err| EngineError::InvalidAction(format!("{err:?}")))?;
            let current_slots = remaining_after_current
                .saturating_sub(remaining_minimum)
                .min(bounds.max);
            let end_slot = *next_slot + current_slots;
            let Some(window) = selected_slots.get(*next_slot..end_slot) else {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            };
            if window.len() < bounds.min || window[..bounds.min].iter().any(Option::is_none) {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
            ability.targets.extend(window.iter().flatten().cloned());
            *next_slot = end_slot;
        } else {
            let Some(selected_slot) = selected_slots.get(*next_slot) else {
                return Err(EngineError::InvalidAction(
                    "Missing target selection".to_string(),
                ));
            };

            match selected_slot {
                Some(target) => ability.targets.push(target.clone()),
                None if ability.optional_targeting => {}
                None => {
                    return Err(EngineError::InvalidAction(
                        "Missing required target".to_string(),
                    ));
                }
            }
            *next_slot += 1;
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        assign_selected_slots_after_deferred_effect(
            state,
            ability.sub_ability.as_deref_mut(),
            selected_slots,
            next_slot,
        )?;
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        if defers_conditional_target_selection(sub_ability) {
            return Ok(());
        }
        assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)?;
    }
    Ok(())
}

fn assign_targets_after_deferred_effect(
    state: &GameState,
    sub_ability: Option<&mut ResolvedAbility>,
    targets: &[TargetRef],
    next_target: &mut usize,
) -> Result<(), EngineError> {
    let Some(sub_ability) = sub_ability else {
        return Ok(());
    };
    if defers_conditional_target_selection(sub_ability) {
        return Ok(());
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        return assign_targets_after_deferred_effect(
            state,
            sub_ability.sub_ability.as_deref_mut(),
            targets,
            next_target,
        );
    }
    assign_targets_recursive(state, sub_ability, targets, next_target)
}

fn assign_selected_slots_after_deferred_effect(
    state: &GameState,
    sub_ability: Option<&mut ResolvedAbility>,
    selected_slots: &[Option<TargetRef>],
    next_slot: &mut usize,
) -> Result<(), EngineError> {
    let Some(sub_ability) = sub_ability else {
        return Ok(());
    };
    if defers_conditional_target_selection(sub_ability) {
        return Ok(());
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        return assign_selected_slots_after_deferred_effect(
            state,
            sub_ability.sub_ability.as_deref_mut(),
            selected_slots,
            next_slot,
        );
    }
    assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)
}

/// CR 115.3: Validate targeting constraints — e.g., different target players must be distinct.
///
/// `ability` is `Some` only on the `_for_ability` validation family (resolution-time
/// selection), where source-relative dynamic constraints can be resolved against
/// game state using the ability's controller/source provenance. Fixed caps only
/// need `state`, so stack-announcement/random-selection callsites still enforce
/// those when a stateful validation path is available.
fn validate_target_constraints(
    state: Option<&GameState>,
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
    ability: Option<&ResolvedAbility>,
) -> Result<(), EngineError> {
    for constraint in constraints {
        match constraint {
            TargetSelectionConstraint::DifferentTargetPlayers => {
                let players = targets
                    .iter()
                    .filter_map(|target| match target {
                        TargetRef::Player(player) => Some(*player),
                        TargetRef::Object(_) => None,
                    })
                    .collect::<std::collections::HashSet<_>>();
                let player_target_count = targets
                    .iter()
                    .filter(|target| matches!(target, TargetRef::Player(_)))
                    .count();
                if players.len() != player_target_count {
                    return Err(EngineError::InvalidAction(
                        "Selected player targets must be different".to_string(),
                    ));
                }
            }
            TargetSelectionConstraint::DifferentObjectControllers => {
                let Some(state) = state else {
                    continue;
                };
                let mut controllers = std::collections::HashSet::new();
                for target in targets {
                    let TargetRef::Object(object_id) = target else {
                        continue;
                    };
                    let controller = state
                        .objects
                        .get(object_id)
                        .ok_or_else(|| {
                            EngineError::InvalidAction("Selected object target is missing".into())
                        })?
                        .controller;
                    if !controllers.insert(controller) {
                        return Err(EngineError::InvalidAction(
                            "Selected object targets must be controlled by different players"
                                .to_string(),
                        ));
                    }
                }
            }
            TargetSelectionConstraint::TotalManaValue { comparator, value } => {
                let Some(state) = state else {
                    continue;
                };
                let cap = match value {
                    QuantityExpr::Fixed { value } => *value,
                    _ => {
                        // Skip dynamic caps when source/controller provenance is
                        // unavailable. For the where-X die-result cap
                        // (`EventContextAmount`), `resolve_quantity` reads
                        // `state.die_result_this_resolution` (CR 706.2 + CR 706.4).
                        let Some(ability) = ability else {
                            continue;
                        };
                        crate::game::quantity::resolve_quantity(
                            state,
                            value,
                            ability.controller,
                            ability.source_id,
                        )
                    }
                };
                // CR 202.3 + CR 202.3e: combined mana value of the chosen object
                // targets; on-stack spells include the announced X value.
                let sum: i32 = targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) => state
                            .objects
                            .get(id)
                            .map(|o| o.mana_cost.mana_value_with_x(o.zone, o.cost_x_paid) as i32),
                        TargetRef::Player(_) => None,
                    })
                    .sum();
                // CR 601.2c + CR 608.2c + CR 109.5: enforce the cap against the
                // chosen set.
                if !comparator.evaluate(sum, cap) {
                    return Err(EngineError::InvalidAction(
                        "Selected targets exceed the allowed total mana value".to_string(),
                    ));
                }
            }
        }
    }

    Ok(())
}

fn chain_has_target_sink(ability: &ResolvedAbility) -> bool {
    if let Effect::Attach { attachment, target } = &ability.effect {
        if attach_side_needs_target_slot(attachment, true)
            || attach_side_needs_target_slot(target, false)
        {
            return true;
        }
    }

    // CR 609.7 + CR 601.2c: A source-scoped `PreventDamage` head node consumes
    // the chosen source spell into its own `targets[0]` — `collect_target_slots`
    // pushes a source slot for it, and `assign_targets_recursive` consumes one
    // target into this node BEFORE descending into the (modal) sub-chain.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && prevent_damage_source_slot_filter(&ability.effect).is_some()
    {
        return true;
    }

    // CR 109.4 + CR 115.1: A node also acts as a target sink when its filter
    // references `ControllerRef::TargetPlayer` (DamageAll, PutCounterAll,
    // etc.) — `collect_target_slots` pushes a companion player slot for it,
    // and `assign_targets_recursive` consumes one target into this node.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
    {
        return true;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
    {
        return true;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
    {
        return true;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && triggers::extract_target_filter_from_effect(&ability.effect).is_some()
    {
        return true;
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return chain_has_target_sink_after_deferred_effect(ability.sub_ability.as_deref());
    }
    ability
        .sub_ability
        .as_deref()
        .is_some_and(chain_has_target_sink)
}

fn chain_has_target_sink_after_deferred_effect(sub_ability: Option<&ResolvedAbility>) -> bool {
    let Some(sub_ability) = sub_ability else {
        return false;
    };
    if defers_conditional_target_selection(sub_ability) {
        return false;
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        return chain_has_target_sink_after_deferred_effect(sub_ability.sub_ability.as_deref());
    }
    chain_has_target_sink(sub_ability)
}

fn minimum_targets_in_chain(state: &GameState, ability: &ResolvedAbility) -> usize {
    let attach_targets = if let Effect::Attach { attachment, target } = &ability.effect {
        if ability.optional_targeting {
            0
        } else {
            usize::from(attach_side_needs_target_slot(attachment, true))
                + usize::from(attach_side_needs_target_slot(target, false))
        }
    } else {
        0
    };
    let move_counter_targets = if let Effect::MoveCounters {
        source,
        target,
        selection,
        ..
    } = &ability.effect
    {
        if ability.optional_targeting {
            0
        } else {
            move_counter_stack_target_filters(source, target, *selection)
                .into_iter()
                .filter(|filter| !filter.is_context_ref())
                .count()
        }
    } else {
        0
    };

    // CR 109.4: Companion player slot for `ControllerRef::TargetPlayer` filters
    // contributes one required slot (or zero when targeting is optional).
    let player_companion = if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
        && !ability.optional_targeting
    {
        1
    } else {
        0
    };
    let target_creature_quantity_companion = if ability.target_choice_timing
        == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
        && !ability.optional_targeting
    {
        1
    } else {
        0
    };
    let parent_target_combat_relation_companion = if ability.target_choice_timing
        == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        && !ability.optional_targeting
    {
        1
    } else {
        0
    };
    let current = if matches!(
        &ability.effect,
        Effect::Attach { .. } | Effect::MoveCounters { .. }
    ) {
        0
    } else if ability.target_choice_timing == TargetChoiceTiming::Stack
        && triggers::extract_target_filter_from_effect(&ability.effect).is_some()
    {
        if let Some(spec) = ability
            .multi_target
            .as_ref()
            .filter(|spec| spec.max.is_some())
        {
            resolve_multi_target_min(state, ability, spec)
        } else if ability.optional_targeting {
            0
        } else {
            1
        }
    } else {
        0
    };
    let current = attach_targets
        + move_counter_targets
        + player_companion
        + target_creature_quantity_companion
        + parent_target_combat_relation_companion
        + current;

    let rest = if defers_sub_ability_target_selection(&ability.effect) {
        minimum_targets_after_deferred_effect(state, ability.sub_ability.as_deref())
    } else {
        ability
            .sub_ability
            .as_deref()
            .map(|sub| minimum_targets_in_chain(state, sub))
            .unwrap_or(0)
    };

    current + rest
}

fn minimum_targets_after_deferred_effect(
    state: &GameState,
    sub_ability: Option<&ResolvedAbility>,
) -> usize {
    let Some(sub_ability) = sub_ability else {
        return 0;
    };
    if defers_conditional_target_selection(sub_ability) {
        return 0;
    }
    if skips_stack_targets_after_deferred_effect(&sub_ability.effect) {
        return minimum_targets_after_deferred_effect(state, sub_ability.sub_ability.as_deref());
    }
    minimum_targets_in_chain(state, sub_ability)
}

/// CR 700.2a: The controller of a modal spell or activated ability chooses the mode(s)
/// as part of casting. If a mode would be illegal, it can't be chosen.
/// CR 700.2d: A player normally can't choose the same mode more than once.
pub fn validate_modal_indices(
    modal: &ModalChoice,
    indices: &[usize],
    unavailable_modes: &[usize],
) -> Result<(), EngineError> {
    if indices.len() < modal.min_choices || indices.len() > modal.max_choices {
        return Err(EngineError::InvalidAction(format!(
            "Must choose between {} and {} modes, got {}",
            modal.min_choices,
            modal.max_choices,
            indices.len()
        )));
    }

    let mut seen = std::collections::HashSet::new();
    for &idx in indices {
        if idx >= modal.mode_count {
            return Err(EngineError::InvalidAction(format!(
                "Mode index {idx} out of range ({})",
                modal.mode_count
            )));
        }
        if !modal.allow_repeat_modes && !seen.insert(idx) {
            return Err(EngineError::InvalidAction(format!(
                "Duplicate mode index {idx}"
            )));
        }
        // CR 700.2a-b: Reject modes unavailable due to prior selections or
        // unsatisfied targeting requirements.
        if unavailable_modes.contains(&idx) {
            return Err(EngineError::InvalidAction(format!(
                "Mode index {idx} is unavailable"
            )));
        }
    }

    Ok(())
}

/// CR 700.2d: Generate all valid mode selection sequences for a modal spell/ability.
pub fn generate_modal_index_sequences(modal: &ModalChoice) -> Vec<Vec<usize>> {
    let mut actions = Vec::new();
    for count in modal.min_choices..=modal.max_choices {
        let mut current = Vec::with_capacity(count);
        let start = if modal.allow_repeat_modes {
            0
        } else {
            usize::MAX
        };
        build_mode_sequences(
            modal.mode_count,
            count,
            start,
            modal.allow_repeat_modes,
            &mut current,
            &mut actions,
        );
    }
    actions
}

fn build_mode_sequences(
    mode_count: usize,
    remaining: usize,
    min_index: usize,
    allow_repeat: bool,
    current: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    if remaining == 0 {
        out.push(current.clone());
        return;
    }

    let start_index = if min_index == usize::MAX {
        0
    } else {
        min_index
    };
    for idx in start_index..mode_count {
        current.push(idx);
        build_mode_sequences(
            mode_count,
            remaining - 1,
            if allow_repeat { idx } else { idx + 1 },
            allow_repeat,
            current,
            out,
        );
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityKind, AggregateFunction, BounceSelection, CardTypeSetSource,
        CastManaObjectScope, CastManaSpentMetric, Comparator, ContinuousModification,
        ControllerRef, CountScope, CounterTransferMode, DamageKindFilter, Duration, Effect,
        FilterProp, GameRestriction, LibraryPosition, ModalChoice, ModalSelectionConstraint,
        MultiTargetSpec, ObjectProperty, ObjectScope, ProhibitedActivity, PtStat, PtValue,
        PtValueScope, QuantityExpr, QuantityRef, RestrictionExpiry, RestrictionPlayerScope,
        SearchSelectionConstraint, SharedQuality, SharedQualityRelation, StaticDefinition,
        TargetFilter, TargetRef, TypeFilter, TypedFilter, UnlessPayModifier,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        GameState, PayCostKind, StackEntryKind, TargetSelectionConstraint, TargetSelectionSlot,
        WaitingFor,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::mana::{ManaCost, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use crate::types::{FormatConfig, GameAction};

    /// Issue: Alela, Cunning Conqueror hung the controller in a 4-player game.
    /// "Whenever one or more Faeries you control deal combat damage to a player,
    /// goad target creature that player controls" surfaces a companion
    /// `TargetPlayer` slot to bind the goad target's "that player controls"
    /// filter. The slot was populated with every player at the table (the
    /// source's own controller included), so the dependent creature slot had no
    /// satisfiable combination and legal-action generation collapsed to empty,
    /// hanging the AI. CR 120.3a + CR 603.7c: "that player" is the damaged
    /// player carried by the triggering event, so the companion slot must offer
    /// only that player. Two-player games masked this (a single opponent).
    #[test]
    fn companion_target_player_slot_binds_to_damaged_player() {
        use crate::types::events::GameEvent;

        let mut state = GameState::new(FormatConfig::duel_commander(), 4, 7);
        let alela = create_object(
            &mut state,
            CardId(1),
            PlayerId(3),
            "Alela, Cunning Conqueror".to_string(),
            Zone::Battlefield,
        );
        // The damaged player (0) controls a creature — a legal goad target.
        let hydra = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Managorger Hydra".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&hydra)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        // A non-damaged player (2) also controls a creature — it must NOT be
        // reachable, because the companion slot is bound to player 0 only.
        let other = create_object(
            &mut state,
            CardId(3),
            PlayerId(2),
            "Doc Aurlock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // The pending trigger's event batch: combat damage dealt to player 0.
        state.pending_trigger_event_batch = vec![GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(0),
            source_amounts: vec![],
            total_damage: 11,
        }];

        let mut ability = ResolvedAbility::new(
            Effect::Goad {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![],
            alela,
            PlayerId(3),
        );
        // Triggered abilities carry a source incarnation; the constraint is
        // gated on it so only triggers (not spells) read the pending event batch.
        ability.source_incarnation = Some(1);

        let slots = build_target_slots(&state, &ability).expect("target slots build");

        // Static slot: the companion player slot must list ONLY the damaged
        // player — not all four players.
        let player_slot = slots
            .iter()
            .find(|s| {
                !s.legal_targets.is_empty()
                    && s.legal_targets
                        .iter()
                        .all(|t| matches!(t, TargetRef::Player(_)))
            })
            .expect("companion player slot present");
        assert_eq!(
            player_slot.legal_targets,
            vec![TargetRef::Player(PlayerId(0))],
            "static companion slot must bind to the damaged player, not all players"
        );

        // Dynamic path: this is what feeds legal-action generation and is where
        // the hang actually occurred. Slot 0 (the player) must recompute to ONLY
        // the damaged player — a prior version constrained the static slot but
        // re-offered all players here, so the dependent slot 1 had no satisfiable
        // combination and legal actions collapsed to empty.
        let slot0 =
            build_target_selection_progress_for_ability(&state, &ability, &slots, &[], 0, vec![])
                .expect("slot 0 progress");
        assert_eq!(
            slot0.current_legal_targets,
            vec![TargetRef::Player(PlayerId(0))],
            "dynamic slot 0 must offer only the damaged player"
        );

        // Slot 1 after choosing the damaged player: the goad target is that
        // player's creature (the Hydra), never a non-damaged player's creature.
        let slot1 = build_target_selection_progress_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            1,
            vec![Some(TargetRef::Player(PlayerId(0)))],
        )
        .expect("slot 1 progress");
        assert_eq!(
            slot1.current_legal_targets,
            vec![TargetRef::Object(hydra)],
            "goad target must be the damaged player's creature only"
        );
    }

    /// Issue #478 regression: a delayed-trigger return effect
    /// (`ChangeZone { target: ParentTarget }`) carries a resolution-time
    /// *snapshot* in `targets`, not a player-chosen target. CR 608.2b's
    /// re-validation/fizzle applies only to abilities that *specify targets*;
    /// a `ParentTarget` snapshot referencing an exiled card (Flickerwisp's
    /// "return that card") must survive `validate_targets_in_chain` verbatim so
    /// the return is not wrongly fizzled before `change_zone::resolve` runs.
    #[test]
    fn validate_targets_in_chain_preserves_parent_target_snapshot_off_battlefield() {
        let format = FormatConfig::duel_commander();
        let mut state = GameState::new(format, 2, 2);
        let victim = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Exile,
        );

        // A delayed-return ability: ChangeZone -> Battlefield with a
        // `ParentTarget` snapshot, the snapshot being the exiled victim.
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![TargetRef::Object(victim)],
            ObjectId(99),
            PlayerId(0),
        );

        let validated = validate_targets_in_chain(&state, &ability);
        // The snapshot must pass through unchanged — not filtered to
        // battlefield presence, which would empty it and fizzle the return.
        assert_eq!(
            validated.targets,
            vec![TargetRef::Object(victim)],
            "a ParentTarget snapshot of an exiled card must survive target \
             re-validation (CR 603.7c) — not be fizzle-filtered (CR 608.2b)"
        );
        assert!(
            !crate::game::targeting::check_fizzle(
                &flatten_targets_in_chain(&ability),
                &flatten_targets_in_chain(&validated),
            ),
            "a delayed-return ParentTarget ability must not fizzle when its \
             snapshotted object is off the battlefield"
        );
    }

    /// CR 608.2c + CR 608.2h + CR 704.5d (issue #1582): Recoil reads "Return
    /// target permanent to its owner's hand. Then that player discards a card."
    /// When the bounced permanent is a token, it ceases to exist as a
    /// state-based action after returning to hand, so the live object is gone
    /// before the chained discard resolves. The "that player" anaphor
    /// (`ParentTargetController` / `ParentTargetOwner`) must therefore resolve
    /// through last-known information (CR 608.2h) rather than the now-removed
    /// object — otherwise the discard silently resolves against the wrong player
    /// (or no one), which is exactly the reported bug.
    #[test]
    fn parent_target_player_falls_back_to_lki_after_object_ceases_to_exist() {
        let format = FormatConfig::duel_commander();
        let mut state = GameState::new(format, 2, 2);
        let token = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&token).unwrap().is_token = true;

        let ability = ResolvedAbility::new(
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::ParentTargetController,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
            vec![TargetRef::Object(token)],
            ObjectId(99),
            PlayerId(0),
        );

        // While the token is live, the anaphor resolves directly (CR 109.4).
        assert_eq!(
            parent_target_controller(&ability, &state),
            Some(PlayerId(1))
        );
        assert_eq!(parent_target_owner(&ability, &state), Some(PlayerId(1)));

        // Bounce to hand snapshots LKI, then SBA removes the token (CR 704.5d).
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, token, Zone::Hand, &mut events);
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            !state.objects.contains_key(&token),
            "CR 704.5d: bounced token must cease to exist"
        );
        assert!(
            state.lki_cache.contains_key(&token),
            "battlefield exit must snapshot last-known information for CR 608.2h"
        );

        // The fix: player anaphors resolve via LKI once the object is gone.
        assert_eq!(
            parent_target_controller(&ability, &state),
            Some(PlayerId(1)),
            "CR 608.2c: 'that player' must resolve via LKI after the token ceased to exist"
        );
        assert_eq!(
            parent_target_owner(&ability, &state),
            Some(PlayerId(1)),
            "CR 608.2c: 'its owner' must resolve via LKI after the token ceased to exist"
        );
    }

    //mazes end test for self bounce lands
    #[test]
    fn mazes_end_search_resolves_after_self_bounce_cost() {
        let format = FormatConfig::duel_commander();
        let mut state = GameState::new(format, 2, 2);
        let mazes_end = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Maze's End".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mazes_end).expect("Maze's End");
            obj.card_types.core_types.push(CoreType::Land);
            std::sync::Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::SearchLibrary {
                        filter: TargetFilter::Typed(
                            TypedFilter::new(TypeFilter::Land)
                                .with_type(TypeFilter::Subtype("Gate".to_string())),
                        ),
                        count: QuantityExpr::Fixed { value: 1 },
                        reveal: false,
                        target_player: None,
                        selection_constraint: SearchSelectionConstraint::None,
                        split: None,
                        source_zones: vec![crate::types::zones::Zone::Library],
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: Vec::new(),
                                generic: 3,
                            },
                        },
                        AbilityCost::Tap,
                        AbilityCost::ReturnToHand {
                            count: 1,
                            filter: Some(TargetFilter::SelfRef),
                            from_zone: Some(Zone::Battlefield),
                        },
                    ],
                }),
            );
        }
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(999),
                false,
                Vec::new(),
            ));
        }

        let waiting = crate::game::casting::handle_activate_ability(
            &mut state,
            PlayerId(0),
            mazes_end,
            0,
            &mut Vec::new(),
        )
        .expect("Maze's End activation should begin");
        assert!(
            matches!(
                waiting,
                WaitingFor::PayCost {
                    kind: PayCostKind::ReturnToHand,
                    ..
                }
            ),
            "self-bounce cost should request a return-to-hand selection"
        );
        state.waiting_for = waiting;

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![mazes_end],
            },
        )
        .expect("paying the self-bounce cost should finish activation");

        assert_eq!(state.objects[&mazes_end].zone, Zone::Hand);
        assert!(
            state.players[0].hand.contains(&mazes_end),
            "Maze's End is returned to hand as an activation cost"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "Maze's End ability should be on the stack"
        );
        match &state.stack[0].kind {
            StackEntryKind::ActivatedAbility { source_id, ability } => {
                assert_eq!(*source_id, mazes_end);
                assert!(matches!(ability.effect, Effect::SearchLibrary { .. }));
            }
            other => panic!("expected Maze's End activated ability on stack, got {other:?}"),
        }
    }

    #[test]
    fn build_chained_resolved_allows_empty_up_to_mode_selection() {
        let abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
        )];

        let resolved = build_chained_resolved(&abilities, &[], ObjectId(1), PlayerId(0)).unwrap();

        assert!(matches!(
            resolved.effect,
            Effect::GenericEffect {
                ref static_abilities,
                duration: None,
                target: None,
            } if static_abilities.is_empty()
        ));
        assert!(resolved.targets.is_empty());
        assert!(resolved.sub_ability.is_none());
    }

    #[test]
    fn build_chained_resolved_preserves_mode_sub_abilities() {
        // CR 700.2d: Cathartic Pyre mode 2 has "Discard up to two, then draw that many"
        // — the draw sub_ability must not be clobbered when chaining modes.
        let mode1 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let mut mode2 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
        );
        mode2.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        )));

        let abilities = vec![mode1, mode2];

        // Single mode: mode 2 only
        let resolved = build_chained_resolved(&abilities, &[1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(
            matches!(resolved.effect, Effect::Discard { .. }),
            "Root should be Discard"
        );
        let sub = resolved
            .sub_ability
            .as_ref()
            .expect("Draw sub_ability must be preserved");
        assert!(
            matches!(sub.effect, Effect::Draw { .. }),
            "Sub_ability should be Draw, got {:?}",
            sub.effect
        );

        // Both modes: mode 1 then mode 2 — mode 2's internal chain must survive
        let resolved =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(matches!(resolved.effect, Effect::Destroy { .. }));
        let mode2_node = resolved
            .sub_ability
            .as_ref()
            .expect("mode 2 should follow mode 1");
        assert!(matches!(mode2_node.effect, Effect::Discard { .. }));
        let draw_node = mode2_node
            .sub_ability
            .as_ref()
            .expect("Draw sub must survive multi-mode chaining");
        assert!(matches!(draw_node.effect, Effect::Draw { .. }));
    }

    /// Issue #310: `apply_instead_swap` must preserve every effect-shape
    /// field from the sub (player_scope, optional, multi_target, …) and every
    /// runtime-context field from the parent (controller, targets,
    /// chosen_x, …). Pre-fix the swap site in `effects/mod.rs` hand-rolled a
    /// partial clone that silently dropped `sub.player_scope` — same shape
    /// as the casting-path bug fixed by commit 4475b1939.
    #[test]
    fn apply_instead_swap_preserves_sub_player_scope_and_optional() {
        let parent = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                destination: crate::types::zones::Zone::Graveyard,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(10),
            PlayerId(0),
        );
        // Parent has no player_scope; sub has player_scope=Opponent — the
        // bug-class scenario. Pre-fix: swap silently dropped player_scope.
        let mut sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        sub.player_scope = Some(crate::types::ability::PlayerFilter::Opponent);
        sub.optional = true;
        sub.description = Some("override description".to_string());

        let swapped = apply_instead_swap(&parent, &sub);

        // Effect-shape fields come from sub.
        assert!(
            matches!(swapped.effect, Effect::Draw { .. }),
            "swap must adopt sub's effect"
        );
        assert_eq!(
            swapped.player_scope,
            Some(crate::types::ability::PlayerFilter::Opponent),
            "swap must preserve sub.player_scope (issue #310)"
        );
        assert!(swapped.optional, "swap must preserve sub.optional");
        assert_eq!(swapped.description.as_deref(), Some("override description"));
        // Identity / runtime-context fields come from parent.
        assert_eq!(
            swapped.controller,
            PlayerId(0),
            "swap must preserve parent.controller"
        );
        assert_eq!(
            swapped.source_id,
            ObjectId(10),
            "swap must preserve parent.source_id"
        );
        assert_eq!(
            swapped.targets,
            vec![TargetRef::Player(PlayerId(0))],
            "swap must preserve parent.targets (announced before resolution)"
        );
        // The parent's condition was carrying the "instead" gate which has
        // already been evaluated; swap clears it.
        assert!(
            swapped.condition.is_none(),
            "swap must clear parent.condition (CR 608.2c)"
        );
    }

    /// Issue #310: spell-cast and ability-activate paths now delegate to
    /// `build_resolved_from_def` so `player_scope` survives end-to-end. Pin
    /// that contract so accidental partial-clone regressions in casting
    /// surface here too.
    #[test]
    fn build_resolved_from_def_preserves_player_scope() {
        let def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Controller,
                destination: crate::types::zones::Zone::Graveyard,
            },
        )
        .player_scope(crate::types::ability::PlayerFilter::Opponent);

        let resolved = build_resolved_from_def(&def, ObjectId(1), PlayerId(0));
        assert_eq!(
            resolved.player_scope,
            Some(crate::types::ability::PlayerFilter::Opponent),
            "player_scope must survive build_resolved_from_def — issue #310",
        );
    }

    #[test]
    fn build_resolved_from_def_preserves_unless_pay_modifier() {
        let modifier = UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            payer: TargetFilter::ParentTargetController,
        };
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SetTapState {
                target: TargetFilter::ParentTarget,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .unless_pay(modifier.clone());

        let resolved = build_resolved_from_def(&def, ObjectId(1), PlayerId(0));
        assert_eq!(resolved.unless_pay, Some(modifier));
    }

    #[test]
    fn build_chained_resolved_sorts_indices_to_printed_order() {
        // CR 608.2c: Modes resolve in printed order regardless of the order
        // the player announced them in. Feeding [2, 0, 1] must still produce
        // a chain in order [0 → 1 → 2] (Destroy → Draw → Discard).
        let mode_destroy = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let mode_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mode_discard = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                unless_filter: None,
                filter: None,
            },
        );
        let abilities = vec![mode_destroy, mode_draw, mode_discard];

        let resolved =
            build_chained_resolved(&abilities, &[2, 0, 1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(
            matches!(resolved.effect, Effect::Destroy { .. }),
            "Root should be mode 0 (Destroy) — printed first"
        );
        let draw_node = resolved
            .sub_ability
            .as_ref()
            .expect("mode 1 should follow mode 0");
        assert!(
            matches!(draw_node.effect, Effect::Draw { .. }),
            "Second link should be mode 1 (Draw)"
        );
        let discard_node = draw_node
            .sub_ability
            .as_ref()
            .expect("mode 2 should follow mode 1");
        assert!(
            matches!(discard_node.effect, Effect::Discard { .. }),
            "Third link should be mode 2 (Discard) — printed last"
        );
    }

    #[test]
    fn chained_draw_player_plus_damageall_targetplayer_assigns_both_targets() {
        use crate::types::ability::{ControllerRef, TargetRef};
        // Reproduce Ashling's Command modes 2 + 3 chained:
        //   Mode 2: Draw 2, target: Player
        //   Mode 3: DamageAll { target: Typed{ controller: TargetPlayer } }
        // collect_target_slots emits 2 slots (one per mode). assign_targets_in_chain
        // must distribute both selected players — one to Draw.targets, one to
        // DamageAll.targets — so each effect's resolver sees the right player.
        let mode_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
            },
        );
        let mode_damageall = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
                player_filter: None,
                damage_source: None,
            },
        );

        let abilities = vec![mode_draw, mode_damageall];
        let mut chain =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();

        let p_a = TargetRef::Player(PlayerId(0));
        let p_b = TargetRef::Player(PlayerId(1));
        let state = GameState::new_two_player(42);
        let result = assign_targets_in_chain(&state, &mut chain, &[p_a.clone(), p_b.clone()]);
        assert!(
            result.is_ok(),
            "assigning two player targets to [Draw{{Player}}, DamageAll{{TargetPlayer}}] \
             chain must succeed, got {result:?}"
        );

        // Draw root should have first selected player.
        assert_eq!(chain.targets, vec![p_a.clone()], "Draw should get target 0");
        // DamageAll sub should have second selected player so its
        // `ControllerRef::TargetPlayer` filter resolves to the right player.
        let sub = chain
            .sub_ability
            .as_deref()
            .expect("sub_ability must exist");
        assert_eq!(
            sub.targets,
            vec![p_b],
            "DamageAll should get target 1 (the second player slot)"
        );
    }

    #[test]
    fn add_restriction_targeted_player_surfaces_one_slot_and_that_player_inherits_it() {
        use crate::types::statics::ActivationExemption;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(0xABE),
            PlayerId(0),
            "Abeyance".to_string(),
            Zone::Stack,
        );

        let root = ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::TargetedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::CastSpells { spell_filter: None },
                },
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    source: ObjectId(0),
                    affected_players: RestrictionPlayerScope::ParentTargetedPlayer,
                    expiry: RestrictionExpiry::EndOfTurn,
                    activity: ProhibitedActivity::ActivateAbilities {
                        exemption: ActivationExemption::ManaAbilities,
                    },
                },
            },
            vec![],
            source,
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &root).expect("target slots should build");
        assert_eq!(
            slots.len(),
            1,
            "\"target player\" declares one target; the \"that player\" tail inherits it"
        );

        let mut resolved = root;
        assign_targets_in_chain(&state, &mut resolved, &[TargetRef::Player(PlayerId(1))])
            .expect("single selected player should assign to the root restriction");

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &resolved, &mut events, 0)
            .expect("restriction chain should resolve");

        assert_eq!(state.restrictions.len(), 2);
        assert!(state.restrictions.iter().all(|restriction| matches!(
            restriction,
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::SpecificPlayer(PlayerId(1)),
                ..
            }
        )));
    }

    #[test]
    fn chained_token_player_plus_damageall_targetplayer_assigns_both_targets() {
        // CR 111.2 + CR 601.2c: Mirror of the Draw chain test for the Token
        // owner-target pathway. With Token{owner: Player} as mode 4 of a modal
        // spell paired with DamageAll{controller: TargetPlayer} as mode 3,
        // collect_target_slots must surface 2 slots (one per mode) and
        // assign_targets_in_chain must distribute both selected players —
        // one to Token.targets, one to DamageAll.targets.
        let mode_token = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Treasure".to_string(),
                power: crate::types::ability::PtValue::Fixed(0),
                toughness: crate::types::ability::PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Player,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let mode_damageall = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
                player_filter: None,
                damage_source: None,
            },
        );

        let abilities = vec![mode_token, mode_damageall];
        let mut chain =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();

        let p_a = TargetRef::Player(PlayerId(0));
        let p_b = TargetRef::Player(PlayerId(1));
        let state = GameState::new_two_player(42);
        let result = assign_targets_in_chain(&state, &mut chain, &[p_a.clone(), p_b.clone()]);
        assert!(
            result.is_ok(),
            "assigning two player targets to [Token{{Player}}, DamageAll{{TargetPlayer}}] \
             chain must succeed, got {result:?}"
        );

        // Token root should have first selected player.
        assert_eq!(
            chain.targets,
            vec![p_a.clone()],
            "Token should get target 0"
        );
        let sub = chain
            .sub_ability
            .as_deref()
            .expect("sub_ability must exist");
        assert_eq!(
            sub.targets,
            vec![p_b],
            "DamageAll should get target 1 (the second player slot)"
        );
    }

    #[test]
    fn search_library_collects_later_independent_stack_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fertilid's Favor".to_string(),
            Zone::Stack,
        );
        let artifact = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let mut put_counters = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                        TargetFilter::Typed(TypedFilter::creature()),
                    ],
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        put_counters.multi_target = Some(MultiTargetSpec::fixed(0, 1));

        let shuffle = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Player,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(put_counters);
        let put_land = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(shuffle);
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::Player),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(put_land);

        let slots = build_target_slots(&state, &ability).unwrap();

        assert_eq!(slots.len(), 2);
        assert!(!slots[0].optional, "target player is required");
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(
            slots[1].optional,
            "up to one artifact or creature is optional"
        );
        assert!(slots[1]
            .legal_targets
            .contains(&TargetRef::Object(artifact)));

        assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[
                Some(TargetRef::Player(PlayerId(0))),
                Some(TargetRef::Object(artifact)),
            ],
        )
        .unwrap();

        assert_eq!(ability.targets, vec![TargetRef::Player(PlayerId(0))]);
        let counter_step = ability
            .sub_ability
            .as_deref()
            .and_then(|change_zone| change_zone.sub_ability.as_deref())
            .and_then(|shuffle| shuffle.sub_ability.as_deref())
            .expect("counter continuation must exist");
        assert_eq!(counter_step.targets, vec![TargetRef::Object(artifact)]);
    }

    /// CR 109.4 + CR 707.2: "target opponent creates a token that's a copy of
    /// it" — Wedding Ring's shape. `CopyTokenOf` with a context-ref copy source
    /// (`ParentTarget`) and a `Typed{Opponent}` owner must surface exactly one
    /// player target slot, scoped to the opponent (issue #403 defect 1).
    #[test]
    fn build_target_slots_copy_token_owner_target_opponent_is_opponent_only() {
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::ParentTarget,
                owner: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let state = GameState::new_two_player(42);

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots.len(),
            1,
            "the `owner` axis must surface one player target slot"
        );
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
    }

    /// Regression guard: "create a token that's a copy of target creature" —
    /// the copy *source* is the targeted axis, so the slot is the creature
    /// filter, not the (default) `owner`.
    #[test]
    fn build_target_slots_copy_token_targeted_source_surfaces_creature_slot() {
        let creature = {
            let mut s = GameState::new_two_player(42);
            let id = create_object(
                &mut s,
                CardId(9),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Battlefield,
            );
            s.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
            (s, id)
        };
        let (state, creature_id) = creature;
        let ability = ResolvedAbility::new(
            Effect::CopyTokenOf {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                owner: TargetFilter::Controller,
                source_filter: None,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: vec![],
                additional_modifications: vec![],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 1, "the copy-source axis surfaces one slot");
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(creature_id)),
            "the slot must enumerate creature copy-source candidates"
        );
    }

    #[test]
    fn build_target_slots_token_owner_target_opponent_is_opponent_only() {
        // CR 111.2 + CR 115.1: Forbidden Orchard-shape effects encode
        // "target opponent creates ..." as Token{owner: Typed(Opponent)}, so
        // target-slot construction must offer only legal opponent players.
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Spirit".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Spirit".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let state = GameState::new_two_player(42);

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
    }

    #[test]
    fn resolution_timed_zone_sub_ability_defers_target_choice_to_resolution() {
        for (origin, filter) in [
            (
                Zone::Graveyard,
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            ),
            (
                Zone::Exile,
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            ),
        ] {
            let mut ability = ResolvedAbility::new(
                Effect::Mill {
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Controller,
                    destination: Zone::Graveyard,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            let mut sub = ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(origin),
                    destination: Zone::Battlefield,
                    target: filter,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Tapped,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            );
            sub.optional = true;
            sub.target_choice_timing = TargetChoiceTiming::Resolution;
            ability.sub_ability = Some(Box::new(sub));

            let state = GameState::new_two_player(42);
            let slots = build_target_slots(&state, &ability).expect("target slots should build");

            assert!(
                slots.is_empty(),
                "optional {origin:?} zone choice should happen at resolution"
            );
        }
    }

    #[test]
    fn root_graveyard_target_still_uses_stack_targeting() {
        let mut state = GameState::new_two_player(42);
        let artifact_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&artifact_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state
            .objects
            .get_mut(&artifact_id)
            .unwrap()
            .base_card_types
            .core_types
            .push(CoreType::Artifact);
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).properties(
                    vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                )),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(2),
            PlayerId(0),
        );
        ability.optional = true;

        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(artifact_id)]);
    }

    #[test]
    fn build_resolved_copies_optional_targeting() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        )
        .optional_targeting();

        let resolved = build_resolved_from_def(&def, ObjectId(10), PlayerId(0));

        assert!(resolved.optional_targeting);
    }

    #[test]
    fn validate_modal_indices_allows_repeat_when_enabled() {
        let modal = ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 3,
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        };

        assert!(validate_modal_indices(&modal, &[1, 1], &[]).is_ok());
    }

    #[test]
    fn validate_modal_indices_rejects_unavailable_modes() {
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 3,
            ..Default::default()
        };

        // Mode 1 is unavailable — should be rejected.
        let result = validate_modal_indices(&modal, &[1], &[1]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unavailable"));

        // Mode 0 is available — should succeed.
        assert!(validate_modal_indices(&modal, &[0], &[1]).is_ok());
    }

    #[test]
    fn compute_unavailable_modes_returns_previously_chosen() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);

        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 3,
            constraints: vec![ModalSelectionConstraint::NoRepeatThisTurn],
            ..Default::default()
        };

        // No modes chosen yet.
        assert!(compute_unavailable_modes(&state, source_id, &modal).is_empty());

        // Record mode 1 chosen.
        record_modal_mode_choices(&mut state, source_id, &modal, &[1]);
        assert_eq!(
            compute_unavailable_modes(&state, source_id, &modal),
            vec![1]
        );

        // Different source_id is unaffected.
        assert!(compute_unavailable_modes(&state, ObjectId(200), &modal).is_empty());
    }

    #[test]
    fn record_modal_mode_choices_tracks_game_scoped() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);

        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 4,
            constraints: vec![ModalSelectionConstraint::NoRepeatThisGame],
            ..Default::default()
        };

        record_modal_mode_choices(&mut state, source_id, &modal, &[2]);
        assert!(state.modal_modes_chosen_this_game.contains(&(source_id, 2)));
        // Turn-scoped map should NOT be populated for game-scoped constraint.
        assert!(!state.modal_modes_chosen_this_turn.contains(&(source_id, 2)));
    }

    #[test]
    fn generate_modal_index_sequences_supports_repeated_modes() {
        let modal = ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 2,
            allow_repeat_modes: true,
            ..Default::default()
        };

        let sequences = generate_modal_index_sequences(&modal);

        assert_eq!(sequences, vec![vec![0, 0], vec![0, 1], vec![1, 1]]);
    }

    #[test]
    fn generate_target_assignments_enforces_different_target_players() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];

        let assignments = generate_target_assignments(
            &slots,
            &[TargetSelectionConstraint::DifferentTargetPlayers],
        );

        assert_eq!(
            assignments,
            vec![
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0))
                ],
            ]
        );
    }

    #[test]
    fn target_selection_filters_objects_with_same_controller() {
        let mut state = GameState::new_two_player(42);
        let p0_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 A".to_string(),
            Zone::Battlefield,
        );
        let p0_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "P0 B".to_string(),
            Zone::Battlefield,
        );
        let p1_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "P1 A".to_string(),
            Zone::Battlefield,
        );
        for id in [p0_a, p0_b, p1_a] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::fixed(2, 2));
        let slots = build_target_slots(&state, &ability).expect("target slots");
        let progress = begin_target_selection_for_ability(
            &state,
            &ability,
            &slots,
            &[TargetSelectionConstraint::DifferentObjectControllers],
        )
        .expect("selection starts");

        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[TargetSelectionConstraint::DifferentObjectControllers],
            &progress,
            Some(TargetRef::Object(p0_a)),
        )
        .expect("first target accepted") else {
            panic!("expected second target prompt");
        };

        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(p1_a)]
        );
        assert!(!progress
            .current_legal_targets
            .contains(&TargetRef::Object(p0_b)));
    }

    /// CR 202.3 + CR 601.2c: `validate_target_constraints` enforces the
    /// `TotalManaValue` cap against the combined mana value of the chosen object
    /// targets. Helper that seeds graveyard creatures with explicit mana values
    /// and returns a `(state, ability)` pair plus their object ids.
    fn total_mv_fixture(mvs: &[u32]) -> (GameState, ResolvedAbility, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        let mut ids = Vec::new();
        for (i, mv) in mvs.iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("MV {mv}"),
                Zone::Graveyard,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(*mv);
            ids.push(id);
        }
        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        (state, ability, ids)
    }

    #[test]
    fn total_mana_value_constraint_rejects_over_cap_and_accepts_at_cap() {
        let (state, ability, ids) = total_mv_fixture(&[2, 3, 4]);
        let constraint = TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 5 },
        };
        // 2 + 4 = 6 > 5 → rejected.
        let over = vec![TargetRef::Object(ids[0]), TargetRef::Object(ids[2])];
        assert!(validate_target_constraints(
            Some(&state),
            &over,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_err());
        // 2 + 3 = 5 == 5 → accepted (LE is inclusive).
        let at = vec![TargetRef::Object(ids[0]), TargetRef::Object(ids[1])];
        assert!(validate_target_constraints(
            Some(&state),
            &at,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_ok());
    }

    #[test]
    fn total_mana_value_constraint_enforces_fixed_cap_without_ability() {
        let (state, _ability, ids) = total_mv_fixture(&[2]);
        let constraint = TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 1 },
        };
        let targets = vec![TargetRef::Object(ids[0])];
        // Fixed caps do not need ability provenance; stateful stack/random
        // selection paths must still reject over-cap choices.
        assert!(validate_target_constraints(
            Some(&state),
            &targets,
            std::slice::from_ref(&constraint),
            None
        )
        .is_err());
    }

    #[test]
    fn total_mana_value_constraint_resolves_event_context_amount_from_die_result() {
        let (mut state, ability, ids) = total_mv_fixture(&[3, 4]);
        // CR 706.2: the cap is the rolled die result.
        state.die_result_this_resolution = Some(7);
        let constraint = TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        };
        // 3 + 4 = 7 <= 7 → accepted against the seeded die result.
        let both = vec![TargetRef::Object(ids[0]), TargetRef::Object(ids[1])];
        assert!(validate_target_constraints(
            Some(&state),
            &both,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_ok());
        // Lower the roll → same selection now exceeds the cap.
        state.die_result_this_resolution = Some(6);
        assert!(validate_target_constraints(
            Some(&state),
            &both,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_err());
    }

    #[test]
    fn total_mana_value_constraint_prunes_over_cap_prefix_in_enumeration() {
        // CR 601.2c: "up to three target creature cards, total mana value 5 or
        // less" — auto-selection must prune the over-cap partial set (so a valid
        // under-cap completion is still reachable). With three MV-3 cards and a
        // cap of 5, no two cards fit (3+3=6 > 5), but a single card (3 <= 5) is
        // a legal completion.
        let (state, mut ability, ids) = total_mv_fixture(&[3, 3, 3]);
        ability.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 }));
        let constraint = TargetSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 5 },
        };
        // A single card is under cap → Ok.
        let single = vec![TargetRef::Object(ids[0])];
        assert!(validate_target_constraints(
            Some(&state),
            &single,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_ok());
        // Any two cards is over cap → Err (prefix pruned during enumeration).
        let pair = vec![TargetRef::Object(ids[0]), TargetRef::Object(ids[1])];
        assert!(validate_target_constraints(
            Some(&state),
            &pair,
            std::slice::from_ref(&constraint),
            Some(&ability),
        )
        .is_err());
    }

    #[test]
    fn auto_select_targets_preserves_optional_single_target_choice() {
        let slots = vec![TargetSelectionSlot {
            legal_targets: vec![TargetRef::Player(PlayerId(1))],
            optional: true,
        }];

        let selected = auto_select_targets(&slots, &[]).expect("optional targeting stays legal");

        assert_eq!(selected, None);
    }

    #[test]
    fn auto_select_targets_skips_optional_first_slot_when_only_one_completion_exists() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(0))],
                optional: true,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(0))],
                optional: false,
            },
        ];

        let selected =
            auto_select_targets(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers])
                .expect("unique assignment should be auto-selected");

        assert_eq!(selected, Some(vec![TargetRef::Player(PlayerId(0))]));
    }

    #[test]
    fn auto_select_targets_rejects_unsatisfied_target_constraints() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
            },
        ];

        let result =
            auto_select_targets(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers]);

        assert!(result.is_err());
    }

    #[test]
    fn begin_target_selection_filters_next_slot_choices_in_engine() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];

        let progress =
            begin_target_selection(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers])
                .expect("initial target selection should be legal");

        let TargetSelectionAdvance::InProgress(progress) = choose_target(
            &slots,
            &[TargetSelectionConstraint::DifferentTargetPlayers],
            &progress,
            Some(TargetRef::Player(PlayerId(0))),
        )
        .expect("first target should be accepted") else {
            panic!("expected target selection to continue");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(
            progress.selected_slots,
            vec![Some(TargetRef::Player(PlayerId(0)))]
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    #[test]
    fn choose_target_supports_skipping_optional_slot_before_required_target() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: true,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(42))],
                optional: false,
            },
        ];

        let progress = begin_target_selection(&slots, &[]).expect("selection should start");
        let TargetSelectionAdvance::InProgress(progress) =
            choose_target(&slots, &[], &progress, None).expect("optional slot can be skipped")
        else {
            panic!("expected target selection to continue");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(progress.selected_slots, vec![None]);
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(ObjectId(42))]
        );
    }

    #[test]
    fn build_target_slots_ignores_tracked_set_continuation_filters() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        assert!(
            slots.is_empty(),
            "tracked-set pronouns are bound by prior effects, not chosen as targets"
        );
    }

    #[test]
    fn build_target_slots_ignores_exiled_by_source_library_cleanup() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![],
                duration: None,
                target: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ExiledBySource,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        assert!(
            slots.is_empty(),
            "linked-exile cleanup is resolved from source links, not chosen as a target"
        );
    }

    #[test]
    fn build_target_slots_ignores_composed_exiled_by_source_cast_filter() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::ExiledBySource,
                    ],
                },
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        assert!(
            slots.is_empty(),
            "typed linked-exile filters are resolved from source links, not chosen as targets"
        );
    }

    #[test]
    fn build_target_slots_skips_cast_from_hand_permission() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Card)
                        .controller(ControllerRef::You)
                        .properties(vec![
                            FilterProp::InZone { zone: Zone::Hand },
                            FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: QuantityExpr::Fixed { value: 4 },
                            },
                        ]),
                ),
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        assert!(
            slots.is_empty(),
            "cast-from-hand permissions are resolution-time picks, not stack-time targets"
        );
    }

    #[test]
    fn build_target_slots_keeps_or_filter_with_non_context_branch_targeted() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::ExiledBySource,
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                    ],
                },
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );

        let err = build_target_slots(&state, &ability).expect_err("target slot should be required");

        assert!(matches!(err, EngineError::ActionNotAllowed(_)));
    }

    #[test]
    fn build_target_slots_uses_prior_player_targets_for_relative_controller_filters() {
        use crate::types::ability::ControllerRef;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for creature in [your_creature, opponent_creature] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(opponent_creature)]
        );
    }

    #[test]
    fn build_target_slots_restricts_deal_damage_any_to_any_target_classes() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let planeswalker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        let battle = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Battle".to_string(),
            Zone::Battlefield,
        );

        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        state
            .objects
            .get_mut(&planeswalker)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Planeswalker];
        state
            .objects
            .get_mut(&battle)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Battle];

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("damage spell should have targets");
        assert_eq!(slots.len(), 1);
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(creature)),
            "creatures are legal any-target damage recipients"
        );
        assert!(
            !slots[0].legal_targets.contains(&TargetRef::Object(land)),
            "lands must not be legal any-target damage recipients"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(planeswalker)),
            "planeswalkers are legal any-target damage recipients"
        );
        assert!(
            slots[0].legal_targets.contains(&TargetRef::Object(battle)),
            "battles are legal any-target damage recipients"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Player(PlayerId(0)))
                && slots[0]
                    .legal_targets
                    .contains(&TargetRef::Player(PlayerId(1))),
            "players remain legal any-target damage recipients"
        );
    }

    #[test]
    fn choose_target_for_ability_rebinds_relative_controller_to_selected_player() {
        use crate::game::zones::create_object;
        use crate::types::ability::ControllerRef;
        use crate::types::card_type::CoreType;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let opponent_one_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent One Creature".to_string(),
            Zone::Battlefield,
        );
        let opponent_two_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(2),
            "Opponent Two Creature".to_string(),
            Zone::Battlefield,
        );
        for creature in [opponent_one_creature, opponent_two_creature] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");

        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Player(PlayerId(1))),
        )
        .expect("first opponent target should be accepted") else {
            panic!("expected second slot to remain");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_one_creature)]
        );

        let result = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Object(opponent_two_creature)),
        );
        assert!(result.is_err());
    }

    #[test]
    fn per_opponent_gain_control_fanout_recomputes_each_object_slot_from_prior_player() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let caster_creature = create_creature(&mut state, PlayerId(0), CardId(1), "Caster");
        let opponent_one_creature = create_creature(&mut state, PlayerId(1), CardId(2), "Opp One");
        let opponent_two_creature = create_creature(&mut state, PlayerId(2), CardId(3), "Opp Two");
        let ability = per_opponent_gain_control_ability();

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(opponent_one_creature)]
        );
        assert_eq!(slots[2].legal_targets, vec![TargetRef::Player(PlayerId(2))]);
        assert_eq!(
            slots[3].legal_targets,
            vec![TargetRef::Object(opponent_two_creature)]
        );
        assert!(!slots[1]
            .legal_targets
            .contains(&TargetRef::Object(caster_creature)));
        assert!(!slots[3]
            .legal_targets
            .contains(&TargetRef::Object(caster_creature)));

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Player(PlayerId(1))]
        );
        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Player(PlayerId(1))),
        )
        .expect("forced first player target should be accepted") else {
            panic!("expected first object slot");
        };
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_one_creature)]
        );
    }

    #[test]
    fn per_opponent_gain_control_hidden_player_constraint_ignores_player_protection() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let protection_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Protection Source".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_creature(&mut state, PlayerId(1), CardId(2), "Opp One");
        create_creature(&mut state, PlayerId(2), CardId(3), "Opp Two");
        state.add_transient_continuous_effect(
            protection_source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Protection(
                    crate::types::keywords::ProtectionTarget::Everything,
                ),
            }],
            None,
        );
        let ability = per_opponent_gain_control_ability();

        assert!(
            targeting::find_legal_targets(
                &state,
                &TargetFilter::SpecificPlayer { id: PlayerId(1) },
                PlayerId(0),
                ability.source_id,
            )
            .is_empty(),
            "the same player remains illegal when they are an actual target"
        );

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Player(PlayerId(1))),
        )
        .expect("hidden player constraint should bypass player targeting protection") else {
            panic!("expected first object slot");
        };
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_creature)]
        );
    }

    #[test]
    fn per_opponent_fanout_optional_skips_opponent_with_no_legal_targets() {
        // Regression: Haytham Kenway crash — "for each opponent, exile up to
        // one target creature that player controls." When one opponent has no
        // creatures the slot-builder must skip that opponent entirely so the
        // player is never shown an empty selection step.
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        // Player 1 has no creatures. Player 2 has one.
        let opp_two_creature = create_creature(&mut state, PlayerId(2), CardId(1), "Opp Two");
        let mut ability = per_opponent_gain_control_ability();
        ability.multi_target = Some(MultiTargetSpec::bounded(
            0,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        let slots = build_target_slots(&state, &ability).expect("target slots should build");

        // Player 1's slots are omitted — only Player 2's pair is present.
        assert_eq!(slots.len(), 2, "Player 1 (no creatures) must be skipped");
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(2))]);
        assert!(!slots[0].optional);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(opp_two_creature)]
        );
        assert!(slots[1].optional);

        // Multiple valid assignments (skip or take opp-two creature) — no
        // single forced choice, so auto_select defers to the player.
        assert_eq!(
            auto_select_targets_for_ability(&state, &ability, &slots, &[])
                .expect("legal assignment exists"),
            None
        );
        assert!(has_legal_target_assignment_for_ability(
            &state,
            &ability,
            &slots,
            &[]
        ));
    }

    #[test]
    fn per_opponent_fanout_optional_all_opponents_no_creatures_yields_empty_slots() {
        // Regression: 2-player game, Haytham Kenway enters, opponent has no
        // creatures. Slot list must be empty so the trigger auto-pushes with
        // no targets and resolves doing nothing — no UI crash, no spurious
        // cost_payment_failed_flag.
        let state = GameState::new_two_player(42);
        let mut ability = per_opponent_gain_control_ability();
        ability.multi_target = Some(MultiTargetSpec::bounded(
            0,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert!(
            slots.is_empty(),
            "no legal creature targets for any opponent → slots must be empty"
        );
        assert!(has_legal_target_assignment_for_ability(
            &state,
            &ability,
            &slots,
            &[]
        ));
    }

    #[test]
    fn per_opponent_gain_control_assignment_preserves_constraint_players_until_validation() {
        let state = GameState::new(FormatConfig::standard(), 3, 42);
        let mut ability = per_opponent_gain_control_ability();
        let first = TargetRef::Object(ObjectId(1));
        let second = TargetRef::Object(ObjectId(2));

        assign_targets_in_chain(
            &state,
            &mut ability,
            &[
                TargetRef::Player(PlayerId(1)),
                first.clone(),
                TargetRef::Player(PlayerId(2)),
                second.clone(),
            ],
        )
        .expect("assignment should preserve fan-out slots");

        assert_eq!(
            ability.targets,
            vec![
                TargetRef::Player(PlayerId(1)),
                first.clone(),
                TargetRef::Player(PlayerId(2)),
                second.clone()
            ]
        );
        assert_eq!(flatten_targets_in_chain(&ability), vec![first, second]);
    }

    #[test]
    fn per_opponent_gain_control_validation_collapses_only_legal_objects() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let opponent_one_creature = create_creature(&mut state, PlayerId(1), CardId(1), "Opp One");
        let opponent_two_creature = create_creature(&mut state, PlayerId(2), CardId(2), "Opp Two");
        let mut ability = per_opponent_gain_control_ability();
        assign_targets_in_chain(
            &state,
            &mut ability,
            &[
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(opponent_one_creature),
                TargetRef::Player(PlayerId(2)),
                TargetRef::Object(opponent_two_creature),
            ],
        )
        .expect("assignment should preserve pair structure");

        let validated = validate_targets_in_chain(&state, &ability);
        assert_eq!(
            validated.targets,
            vec![
                TargetRef::Object(opponent_one_creature),
                TargetRef::Object(opponent_two_creature)
            ]
        );

        state
            .objects
            .get_mut(&opponent_two_creature)
            .expect("creature exists")
            .controller = PlayerId(1);
        let validated = validate_targets_in_chain(&state, &ability);
        assert_eq!(
            validated.targets,
            vec![TargetRef::Object(opponent_one_creature)],
            "second target is no longer controlled by its paired opponent"
        );
    }

    #[test]
    fn per_opponent_gain_control_runtime_transfers_all_objects_and_preserves_tail() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let opponent_one_creature = create_creature(&mut state, PlayerId(1), CardId(1), "Opp One");
        let opponent_two_creature = create_creature(&mut state, PlayerId(2), CardId(2), "Opp Two");
        state
            .objects
            .get_mut(&opponent_one_creature)
            .unwrap()
            .tapped = true;
        state
            .objects
            .get_mut(&opponent_two_creature)
            .unwrap()
            .tapped = true;
        let mut ability = per_opponent_gain_control_ability().sub_ability(
            ResolvedAbility::new(
                Effect::SetTapState {
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    scope: EffectScope::Single,
                    state: TapStateChange::Untap,
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )
            .sub_ability(ResolvedAbility::new(
                Effect::GenericEffect {
                    static_abilities: vec![StaticDefinition::continuous()
                        .affected(TargetFilter::ParentTarget)
                        .modifications(vec![ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Haste,
                        }])],
                    duration: Some(Duration::UntilEndOfTurn),
                    target: None,
                },
                vec![],
                ObjectId(900),
                PlayerId(0),
            )),
        );
        assign_targets_in_chain(
            &state,
            &mut ability,
            &[
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(opponent_one_creature),
                TargetRef::Player(PlayerId(2)),
                TargetRef::Object(opponent_two_creature),
            ],
        )
        .expect("assignment should preserve pair structure");

        let ability = validate_targets_in_chain(&state, &ability);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("fanout gain-control chain should resolve");
        crate::game::layers::evaluate_layers(&mut state);

        for id in [opponent_one_creature, opponent_two_creature] {
            let object = state.objects.get(&id).expect("object exists");
            assert_eq!(object.controller, PlayerId(0));
            assert!(
                !object.tapped,
                "Mass Mutiny tail should untap each gained creature"
            );
            assert!(
                object
                    .keywords
                    .contains(&crate::types::keywords::Keyword::Haste),
                "Mass Mutiny tail should grant haste to each gained creature"
            );
        }
    }

    fn create_creature(
        state: &mut GameState,
        controller: PlayerId,
        card_id: CardId,
        name: &str,
    ) -> ObjectId {
        let object = create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&object)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        object
    }

    fn per_opponent_gain_control_ability() -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Typed(
                    TypedFilter::permanent().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::bounded(
            1,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        ability
    }

    #[test]
    fn assign_selected_slots_handles_skipped_optional_slot_in_chain() {
        let mut ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.optional_targeting = true;
        let mut ability = ability.sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        ));

        let state = GameState::new_two_player(42);
        assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[None, Some(TargetRef::Player(PlayerId(1)))],
        )
        .expect("slot-based assignment should support skipped optional targets");

        assert!(ability.targets.is_empty());
        assert_eq!(
            flatten_targets_in_chain(&ability),
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    #[test]
    fn build_target_slots_stops_at_interactive_continuation_boundary() {
        let state = crate::types::game_state::GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                choice_optional: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("reveal target should be legal");

        assert_eq!(slots.len(), 1);
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));
    }

    /// CR 109.4 + CR 115.1: `PutCounterAll` with a filter referencing
    /// `ControllerRef::TargetPlayer` surfaces a companion `TargetFilter::Player`
    /// target slot so the player is chosen at target-declaration time. This
    /// covers the Splinter & Leo mode-2 gap ("put a +1/+1 counter on each other
    /// creature target player controls") and is the class-level fix for every
    /// mass-placement effect (DestroyAll, PumpAll, DamageAll, etc.).
    #[test]
    fn build_target_slots_surfaces_player_slot_for_target_player_filter() {
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for c in [your_creature, opp_creature] {
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        // Target-slot surfacing: a companion Player slot must appear, offering
        // both players as legal choices.
        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "expected a single TargetFilter::Player slot for TargetPlayer filter"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));

        // Runtime filter evaluation: with player=0 chosen, only P0's creatures
        // match the TypedFilter. With player=1 chosen, only P1's match.
        for (chosen, expected_match) in [(PlayerId(0), your_creature), (PlayerId(1), opp_creature)]
        {
            let mut resolved = ability.clone();
            resolved.targets = vec![TargetRef::Player(chosen)];
            let ctx = FilterContext::from_ability(&resolved);
            let filter = TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            );
            assert!(
                matches_target_filter(&state, expected_match, &filter, &ctx),
                "chosen player P{} — creature they control should match",
                chosen.0
            );
            let other = if expected_match == your_creature {
                opp_creature
            } else {
                your_creature
            };
            assert!(
                !matches_target_filter(&state, other, &filter, &ctx),
                "chosen player P{} — other player's creature should NOT match",
                chosen.0
            );
        }
    }

    /// CR 108.3 + CR 109.4 + CR 115.1: "target player's graveyard" is an
    /// ownership constraint on a non-battlefield zone. The `Owned{TargetPlayer}`
    /// filter must still surface the companion player target before the object
    /// target so target legality can bind to the chosen player.
    #[test]
    fn build_target_slots_surfaces_player_slot_for_target_player_owned_filter() {
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let your_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        let opp_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        for c in [your_card, opp_card] {
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Instant);
        }

        let filter = TargetFilter::Typed(TypedFilter::card().properties(vec![
            FilterProp::Owned {
                controller: ControllerRef::TargetPlayer,
            },
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
        ]));
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: filter.clone(),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            2,
            "expected companion player slot plus card target slot"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));

        let mut resolved = ability.clone();
        resolved.targets = vec![TargetRef::Player(PlayerId(1))];
        let ctx = FilterContext::from_ability(&resolved);
        assert!(
            matches_target_filter(&state, opp_card, &filter, &ctx),
            "chosen player's graveyard card should match"
        );
        assert!(
            !matches_target_filter(&state, your_card, &filter, &ctx),
            "other player's graveyard card should not match"
        );
    }

    #[test]
    fn build_target_slots_surfaces_player_slot_for_search_target_player_library() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(slots.len(), 1);
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));
        assert!(
            !slots[0]
                .legal_targets
                .contains(&TargetRef::Player(PlayerId(0))),
            "target opponent library search must not allow targeting yourself"
        );
    }

    /// Issue #933: mass filters can declare a target only through a dynamic
    /// threshold ("power greater than target creature's power"). The target
    /// lives inside `FilterProp::PtComparison.value`, so `DestroyAll` must
    /// surface a companion creature slot even though the effect has no primary
    /// `target_filter()`.
    #[test]
    fn build_target_slots_surfaces_creature_slot_for_target_power_mass_filter() {
        let mut state = GameState::new_two_player(42);
        let small = create_creature(&mut state, PlayerId(0), CardId(1), "Small");
        let large = create_creature(&mut state, PlayerId(0), CardId(2), "Large");
        let reference = create_creature(&mut state, PlayerId(1), CardId(3), "Reference");
        state.objects.get_mut(&small).unwrap().power = Some(2);
        state.objects.get_mut(&large).unwrap().power = Some(5);
        state.objects.get_mut(&reference).unwrap().power = Some(3);

        let filter = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Target,
                        },
                    }),
                    offset: 1,
                },
            },
        ]));
        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: filter.clone(),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots.len(),
            1,
            "target-relative mass filter should declare one creature target"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Object(reference)));

        let mut assigned = ability.clone();
        assign_targets_in_chain(&state, &mut assigned, &[TargetRef::Object(reference)])
            .expect("target should assign to mass filter ability");
        assert_eq!(assigned.targets, vec![TargetRef::Object(reference)]);

        let ctx = crate::game::filter::FilterContext::from_ability(&assigned);
        assert!(crate::game::filter::matches_target_filter(
            &state, large, &filter, &ctx
        ));
        assert!(!crate::game::filter::matches_target_filter(
            &state, small, &filter, &ctx
        ));
    }

    #[test]
    fn target_creature_quantity_walker_recurses_through_nested_filter_refs() {
        let target_power_filter = || {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Target,
                        },
                    },
                },
            ]))
        };

        let fixed_filter = || {
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 2 },
                },
            ]))
        };

        let shares_quality = TargetFilter::Typed(TypedFilter::creature().properties(vec![
            FilterProp::SharesQuality {
                quality: SharedQuality::Color,
                reference: Some(Box::new(target_power_filter())),
                relation: SharedQualityRelation::Shares,
            },
        ]));
        assert!(filter_references_target_creature_quantity(&shares_quality));

        let aggregate = QuantityExpr::Ref {
            qty: QuantityRef::Aggregate {
                function: AggregateFunction::Max,
                property: ObjectProperty::ManaValue,
                filter: target_power_filter(),
            },
        };
        assert!(quantity_expr_references_target_creature(&aggregate));

        let damage = QuantityExpr::Ref {
            qty: QuantityRef::DamageDealtThisTurn {
                source: Box::new(fixed_filter()),
                target: Box::new(target_power_filter()),
                aggregate: AggregateFunction::Sum,
                group_by: None,
                damage_kind: DamageKindFilter::Any,
            },
        };
        assert!(quantity_expr_references_target_creature(&damage));

        let spell_filter = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisTurn {
                scope: CountScope::Controller,
                filter: Some(target_power_filter()),
            },
        };
        assert!(quantity_expr_references_target_creature(&spell_filter));

        let card_types = QuantityExpr::Ref {
            qty: QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Objects {
                    filter: target_power_filter(),
                },
            },
        };
        assert!(quantity_expr_references_target_creature(&card_types));

        let mana_spent = QuantityExpr::Ref {
            qty: QuantityRef::ManaSpentToCast {
                scope: CastManaObjectScope::SelfObject,
                metric: CastManaSpentMetric::FromSource {
                    source_filter: target_power_filter(),
                },
            },
        };
        assert!(quantity_expr_references_target_creature(&mana_spent));

        assert!(!filter_references_target_creature_quantity(&fixed_filter()));
    }

    /// CR 115.1 + CR 611.2c: Continuous effects whose affected set is
    /// parameterized by "target player" also declare a player target even when
    /// `GenericEffect.target` itself is absent. Sudden Spoiling is this class:
    /// "creatures target player controls lose all abilities..."
    #[test]
    fn build_target_slots_surfaces_player_slot_for_generic_effect_static_affected_target_player() {
        let state = GameState::new_two_player(42);
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            ))
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);
        let mut ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "expected one companion player slot for TargetPlayer affected filter"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));

        assign_targets_in_chain(&state, &mut ability, &[TargetRef::Player(PlayerId(1))])
            .expect("companion player target should assign to GenericEffect");
        assert_eq!(ability.targets, vec![TargetRef::Player(PlayerId(1))]);
    }

    #[test]
    fn build_target_slots_generic_effect_explicit_target_ignores_target_player_static_affected() {
        let mut state = GameState::new_two_player(42);
        let target_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            ))
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "explicit GenericEffect.target owns target-slot surfacing"
        );
        assert_eq!(
            slots[0].legal_targets,
            vec![TargetRef::Object(target_creature)]
        );
    }

    /// CR 115.1 + CR 404 + CR 406: Nihil Spellbomb / Bojuka Bog / Tormod's
    /// Crypt regression guard. "Exile target player's graveyard" lowers to
    /// `ChangeZoneAll { origin: Graveyard, destination: Exile, target: Player }`.
    /// The mass `target: Player` filter parameterizes the scan by a player —
    /// the resolver enumerates that player's graveyard at resolution time —
    /// so a companion `TargetFilter::Player` slot must be surfaced; otherwise
    /// `ability.targets` stays empty and `player_scope` falls back to the
    /// activator, exiling the wrong (usually empty) graveyard.
    #[test]
    fn build_target_slots_surfaces_player_slot_for_change_zone_all_player_filter() {
        let state = crate::types::game_state::GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "expected a single TargetFilter::Player slot for graveyard-mass exile"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));
    }

    /// CR 109.4 + CR 115.1 + CR 506.2: Karazikar regression guard.
    ///
    /// "Whenever you attack a player, tap target creature that player controls
    /// and goad it." The Tap effect's target filter has
    /// `controller = ControllerRef::TargetPlayer`. Auto-surfacing must produce
    /// a Player target slot, and runtime filter evaluation with a chosen player
    /// must restrict legal creature targets to only that player's creatures —
    /// never the trigger controller's own creatures.
    #[test]
    fn karazikar_tap_target_player_restricts_to_chosen_players_creatures() {
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::types::ability::ControllerRef;

        let mut state = GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Soldier".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Goblin".to_string(),
            Zone::Battlefield,
        );
        for c in [your_creature, opp_creature] {
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let creature_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetPlayer));

        let ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: creature_filter.clone(),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        // Auto-surface produces the companion Player slot first.
        let slots = build_target_slots(&state, &ability).expect("should build");
        assert!(
            slots
                .iter()
                .any(|s| s.legal_targets.contains(&TargetRef::Player(PlayerId(1)))),
            "expected a Player slot offering opponent as a target"
        );

        // Runtime filter: with the opponent chosen, only the opponent's creature
        // matches; your own creature must be excluded.
        let mut resolved = ability.clone();
        resolved.targets = vec![TargetRef::Player(PlayerId(1))];
        let ctx = FilterContext::from_ability(&resolved);
        assert!(
            matches_target_filter(&state, opp_creature, &creature_filter, &ctx),
            "opponent's creature should be a legal tap target",
        );
        assert!(
            !matches_target_filter(&state, your_creature, &creature_filter, &ctx),
            "your own creature must NOT be a legal tap target — this is the Karazikar bug",
        );
    }

    /// CR 701.12a: ExchangeControl must surface two independent target slots,
    /// each honouring its per-slot filter. This is the regression guard for Bug A:
    /// the parser previously dropped both target clauses and the resolver's
    /// early `targets.len() < 2` branch made the effect a no-op.
    #[test]
    fn build_target_slots_exchange_control_surfaces_two_slots() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let p0_land = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "P0 Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p0_land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let p1_land = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(1),
            "P1 Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let target_a = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: Some(ControllerRef::You),
            ..Default::default()
        });
        let target_b = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: Some(ControllerRef::Opponent),
            ..Default::default()
        });
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl { target_a, target_b },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("two slots should build");
        assert_eq!(slots.len(), 2, "exchange-control must surface two slots");
        // Slot 0: "land you control" → only p0_land legal (caster is PlayerId(0)).
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(p0_land)]);
        // Slot 1: "land an opponent controls" → only p1_land legal.
        assert_eq!(slots[1].legal_targets, vec![TargetRef::Object(p1_land)]);
    }

    /// CR 701.12a: SelfRef slots ("this artifact and target X") are filled by
    /// the resolver from `ability.source_id` and must NOT be surfaced as a
    /// user-selectable slot. Only the non-SelfRef slot appears.
    #[test]
    fn build_target_slots_exchange_control_self_ref_suppressed() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let p1_artifact = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Opponent Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        let target_b = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: Some(ControllerRef::Opponent),
            ..Default::default()
        });
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::SelfRef,
                target_b,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("one slot should build");
        assert_eq!(slots.len(), 1, "SelfRef slot must not be surfaced");
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(p1_artifact)]);
    }

    #[test]
    fn build_target_slots_move_counters_surfaces_source_and_destination() {
        use crate::types::ability::ControllerRef;
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let source = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let destination = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Destination".to_string(),
            Zone::Battlefield,
        );
        for id in [source, destination] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let controlled_creature = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            ..Default::default()
        });
        let ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: controlled_creature.clone(),
                counter_type: None,
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                selection: CounterMoveSelection::StackTarget,
                target: controlled_creature,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("two slots should build");
        assert_eq!(
            slots.len(),
            2,
            "move-counters must target source and destination"
        );
        assert_eq!(
            slots[0].legal_targets,
            vec![TargetRef::Object(source), TargetRef::Object(destination)]
        );
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(source), TargetRef::Object(destination)]
        );
    }

    #[test]
    fn assign_targets_move_counters_preserves_source_and_destination_slots() {
        use crate::types::ability::ControllerRef;
        let state = crate::types::game_state::GameState::new_two_player(42);
        let controlled_creature = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            ..Default::default()
        });
        let mut ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: controlled_creature.clone(),
                counter_type: None,
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                selection: CounterMoveSelection::StackTarget,
                target: controlled_creature,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let counter_source = TargetRef::Object(ObjectId(1));
        let destination = TargetRef::Object(ObjectId(2));

        assign_targets_in_chain(
            &state,
            &mut ability,
            &[counter_source.clone(), destination.clone()],
        )
        .expect("move-counters should consume both target slots");

        assert_eq!(ability.targets, vec![counter_source, destination]);
    }

    #[test]
    fn assign_selected_slots_move_counters_preserves_source_and_destination_slots() {
        use crate::types::ability::ControllerRef;
        let controlled_creature = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            ..Default::default()
        });
        let mut ability = ResolvedAbility::new(
            Effect::MoveCounters {
                source: controlled_creature.clone(),
                counter_type: None,
                count: Some(QuantityExpr::Fixed { value: 1 }),
                mode: CounterTransferMode::Move,
                selection: CounterMoveSelection::StackTarget,
                target: controlled_creature,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let counter_source = TargetRef::Object(ObjectId(1));
        let destination = TargetRef::Object(ObjectId(2));

        let state = GameState::new_two_player(42);
        assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[Some(counter_source.clone()), Some(destination.clone())],
        )
        .expect("move-counters should consume both selected slots");

        assert_eq!(ability.targets, vec![counter_source, destination]);
    }

    #[test]
    fn build_target_slots_expands_finite_multi_target() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let creature_a = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        let creature_b = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::fixed(0, 2));

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|slot| slot.optional));
    }

    /// CR 601.2c + CR 601.2d (issue #2856): `cap_distribution_target_slots`
    /// clamps a divided spell's "up to N" target slots to its divisible pool —
    /// each chosen target needs ≥1, so a pool of K can be split among at most K
    /// targets. Exercises the class: pool below cap (clamps), pool at/above cap
    /// (no-op), no-distribute (no-op), and a non-divisible effect (no-op).
    #[test]
    fn cap_distribution_target_slots_clamps_to_divisible_pool() {
        use crate::types::game_state::DistributionUnit;

        let state = crate::types::game_state::GameState::new_two_player(42);
        let damage = DistributionUnit::Damage;

        let make = |x: u32| {
            let mut ability = ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                },
                vec![],
                ObjectId(10),
                PlayerId(0),
            );
            ability.multi_target = Some(crate::types::ability::MultiTargetSpec::fixed(0, 2));
            ability.set_chosen_x_recursive(x);
            ability
        };
        let two_optional_slots = || {
            vec![
                TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: true,
                },
                TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: true,
                },
            ]
        };

        // X = 1: pool of one clamps two "up to two" slots down to one.
        let mut slots = two_optional_slots();
        cap_distribution_target_slots(&state, &make(1), Some(&damage), &mut slots);
        assert_eq!(slots.len(), 1, "X=1 → at most one slot");

        // X = 0: distributes nothing, target count collapses to zero.
        let mut slots = two_optional_slots();
        cap_distribution_target_slots(&state, &make(0), Some(&damage), &mut slots);
        assert_eq!(slots.len(), 0, "X=0 → no slots");

        // X = 2: pool meets the printed cap — both slots survive.
        let mut slots = two_optional_slots();
        cap_distribution_target_slots(&state, &make(2), Some(&damage), &mut slots);
        assert_eq!(slots.len(), 2, "X=2 → printed cap of two retained");

        // X = 5: pool exceeds the printed cap — still capped by the printed two.
        let mut slots = two_optional_slots();
        cap_distribution_target_slots(&state, &make(5), Some(&damage), &mut slots);
        assert_eq!(slots.len(), 2, "pool > cap is a no-op");

        // No distribute flag: never clamp (a non-divided "to each of" multi-target
        // deals the full amount to every chosen target — CR 601.2d does not apply).
        let mut slots = two_optional_slots();
        cap_distribution_target_slots(&state, &make(1), None, &mut slots);
        assert_eq!(slots.len(), 2, "non-distributing ability is untouched");
    }

    #[test]
    fn build_target_slots_resolves_dynamic_multi_target_max() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for index in 0..3 {
            let creature = crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(index + 1),
                PlayerId(0),
                format!("Creature {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::up_to(
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            },
        ));

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert_eq!(slots.len(), 3);
        assert!(slots.iter().all(|slot| slot.optional));
    }

    #[test]
    fn build_target_slots_for_unlimited_multi_target_caps_at_legal_targets() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for index in 0..3 {
            let creature = crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(index + 1),
                PlayerId(0),
                format!("Creature {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::unlimited(0));

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert_eq!(slots.len(), 3);
        assert!(slots.iter().all(|slot| slot.optional));
    }

    #[test]
    fn build_target_slots_rejects_unannounced_x_multi_target_max() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let creature = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::up_to(
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        ));

        assert!(build_target_slots(&state, &ability).is_err());
        ability.chosen_x = Some(1);

        let slots = build_target_slots(&state, &ability).expect("chosen X should resolve max");
        assert_eq!(slots.len(), 1);
    }

    #[test]
    fn build_target_slots_resolves_exact_dynamic_multi_target_min() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for index in 0..3 {
            let creature = crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(index + 1),
                PlayerId(0),
                format!("Creature {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let x = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::exact(x));

        assert!(build_target_slots(&state, &ability).is_err());
        ability.chosen_x = Some(2);

        let slots = build_target_slots(&state, &ability).expect("chosen X should resolve bounds");
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|slot| !slot.optional));

        ability.chosen_x = Some(4);
        assert!(build_target_slots(&state, &ability).is_err());
    }

    #[test]
    fn has_legal_target_assignment_short_circuits_multi_target_existence() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for index in 0..16 {
            let land = crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(index),
                PlayerId(0),
                format!("Land {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
        }

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::land()),
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::fixed(0, 4));

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert!(has_legal_target_assignment_for_ability(
            &state,
            &ability,
            &slots,
            &[]
        ));
    }

    #[test]
    fn auto_select_targets_for_ability_short_circuits_multi_target_ambiguity() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for index in 0..32 {
            let land = crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(index),
                PlayerId(0),
                format!("Land {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
        }

        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::land()),
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::fixed(0, 5));

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert!(matches!(
            auto_select_targets_for_ability(&state, &ability, &slots, &[]),
            Ok(None)
        ));
    }

    #[test]
    fn assign_selected_slots_collects_multi_target_choices() {
        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec::fixed(0, 2));

        let state = GameState::new_two_player(42);
        assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[
                Some(TargetRef::Object(ObjectId(1))),
                Some(TargetRef::Object(ObjectId(2))),
            ],
        )
        .expect("slot-based assignment should preserve both chosen targets");

        assert_eq!(
            ability.targets,
            vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2))
            ]
        );
    }

    /// CR 115.1 + CR 701.9b: A `Random`-mode target slot resolves to one of the
    /// legal targets without prompting the controller. With a seeded RNG, the
    /// result is deterministic across runs (replay/test reproducibility).
    #[test]
    fn random_select_targets_picks_one_of_legal_targets() {
        let mut state = GameState::new_two_player(42);
        let slot = TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Object(ObjectId(7)),
                TargetRef::Object(ObjectId(11)),
            ],
            optional: false,
        };
        let chosen =
            random_select_targets_for_ability(&mut state, std::slice::from_ref(&slot), &[])
                .expect("random selection succeeds when legal targets exist");
        assert_eq!(chosen.len(), 1);
        assert!(slot.legal_targets.contains(&chosen[0]));
    }

    /// CR 115.1 + CR 701.9b: Determinism check — two independent runs with the
    /// same seeded RNG state and the same legal-target set must pick the same
    /// target. This guarantees replays and recorded games behave identically.
    #[test]
    fn random_select_targets_is_deterministic_under_seeded_rng() {
        let slot = TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Object(ObjectId(3)),
                TargetRef::Object(ObjectId(5)),
                TargetRef::Object(ObjectId(8)),
            ],
            optional: false,
        };
        let mut state_a = GameState::new_two_player(1234);
        let mut state_b = GameState::new_two_player(1234);
        let pick_a =
            random_select_targets_for_ability(&mut state_a, std::slice::from_ref(&slot), &[])
                .expect("seeded RNG run a");
        let pick_b =
            random_select_targets_for_ability(&mut state_b, std::slice::from_ref(&slot), &[])
                .expect("seeded RNG run b");
        assert_eq!(pick_a, pick_b, "same seed must yield same target");
    }

    /// CR 115.1 + CR 701.9b: A `Random`-mode slot with no legal targets fails
    /// (parallel to the controller-choice "no legal targets" case, except the
    /// game is the actor — there is no controller to skip the slot).
    #[test]
    fn random_select_targets_errors_when_no_legal_targets() {
        let mut state = GameState::new_two_player(42);
        let slot = TargetSelectionSlot {
            legal_targets: vec![],
            optional: false,
        };
        let result = random_select_targets_for_ability(&mut state, &[slot], &[]);
        assert!(result.is_err(), "empty legal-target set must error");
    }

    /// CR 115.6: Optional `Random`-mode slots with empty legal-target sets are
    /// skipped without producing a target — same shape as the controller-choice
    /// optional path.
    #[test]
    fn random_select_targets_skips_optional_empty_slot() {
        let mut state = GameState::new_two_player(42);
        let slot = TargetSelectionSlot {
            legal_targets: vec![],
            optional: true,
        };
        let chosen = random_select_targets_for_ability(&mut state, &[slot], &[])
            .expect("optional empty slot resolves to empty selection");
        assert!(chosen.is_empty());
    }

    /// CR 115.1 + CR 701.9b: Multi-slot `Random`-mode resolves each slot
    /// independently from `state.rng`. With two distinct legal targets per
    /// slot, the chain produces two picks that each lie in their slot's
    /// legal-target set.
    #[test]
    fn random_select_targets_resolves_each_slot_independently() {
        let mut state = GameState::new_two_player(42);
        let slot_a = TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2)),
            ],
            optional: false,
        };
        let slot_b = TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Object(ObjectId(10)),
                TargetRef::Object(ObjectId(20)),
            ],
            optional: false,
        };
        let chosen =
            random_select_targets_for_ability(&mut state, &[slot_a.clone(), slot_b.clone()], &[])
                .expect("multi-slot random selection succeeds");
        assert_eq!(chosen.len(), 2);
        assert!(slot_a.legal_targets.contains(&chosen[0]));
        assert!(slot_b.legal_targets.contains(&chosen[1]));
    }

    /// CR 115.3: Multi-slot random selection must not pick the same target
    /// twice across slots — the random helper filters previously-chosen
    /// targets from each subsequent slot's pool, mirroring the interactive
    /// `legal_targets_for_slot` filter.
    #[test]
    fn random_select_targets_does_not_repeat_across_slots() {
        let mut state = GameState::new_two_player(42);
        // Two slots with the same single legal target — the second slot must
        // either fail (required) or yield no pick (optional).
        let shared = TargetRef::Object(ObjectId(99));
        let slot_required = TargetSelectionSlot {
            legal_targets: vec![shared.clone()],
            optional: false,
        };
        let slot_optional = TargetSelectionSlot {
            legal_targets: vec![shared.clone()],
            optional: true,
        };
        // Required + required: second slot has no remaining legal target → error.
        let err = random_select_targets_for_ability(
            &mut state,
            &[slot_required.clone(), slot_required.clone()],
            &[],
        );
        assert!(
            err.is_err(),
            "duplicate-only legal set must not violate CR 115.3"
        );

        // Required + optional: optional slot yields no extra pick (skipped).
        let chosen =
            random_select_targets_for_ability(&mut state, &[slot_required, slot_optional], &[])
                .expect("required + optional resolves with one target");
        assert_eq!(chosen, vec![shared]);
    }

    /// CR 115.1: `build_resolved_from_def` propagates `target_selection_mode`
    /// from `AbilityDefinition` to `ResolvedAbility` so the runtime branch in
    /// `casting_targets` can route to the random path.
    #[test]
    fn build_resolved_from_def_carries_target_selection_mode() {
        use crate::types::ability::TargetSelectionMode;
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
        );
        def.target_selection_mode = TargetSelectionMode::Random;
        let resolved = build_resolved_from_def(&def, ObjectId(1), PlayerId(0));
        assert!(matches!(
            resolved.target_selection_mode,
            TargetSelectionMode::Random
        ));
    }

    fn make_simple_ability(targets: Vec<TargetRef>, source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    /// CR 109.4 + CR 608.2c: A Player target's controller IS the player itself.
    #[test]
    fn parent_target_controller_returns_player_for_player_target() {
        let state = GameState::new_two_player(42);
        let ability = make_simple_ability(vec![TargetRef::Player(PlayerId(1))], ObjectId(0));
        assert_eq!(
            parent_target_controller(&ability, &state),
            Some(PlayerId(1)),
            "Player target should resolve to that player"
        );
    }

    /// CR 109.4: An Object target's parent controller is the object's controller.
    #[test]
    fn parent_target_controller_returns_object_controller_for_object_target() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_simple_ability(vec![TargetRef::Object(creature)], ObjectId(0));
        assert_eq!(
            parent_target_controller(&ability, &state),
            Some(PlayerId(1)),
            "Object target should resolve to that object's controller"
        );
    }

    /// CR 608.2c: Stack-object targets resolve to the stack entry controller.
    /// This covers targeted activated/triggered abilities where the parent
    /// target object id is a stack entry, not a battlefield object.
    #[test]
    fn parent_target_controller_resolves_stack_entry_controller() {
        let mut state = GameState::new_two_player(42);
        let stack_id = ObjectId(77);
        let source_id = ObjectId(12);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: stack_id,
            source_id,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(make_simple_ability(vec![], source_id)),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Stack Source".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        });
        let by_entry_id = make_simple_ability(vec![TargetRef::Object(stack_id)], ObjectId(0));
        let by_source_id = make_simple_ability(vec![TargetRef::Object(source_id)], ObjectId(0));

        assert_eq!(
            parent_target_controller(&by_entry_id, &state),
            Some(PlayerId(1))
        );
        assert_eq!(
            parent_target_controller(&by_source_id, &state),
            Some(PlayerId(1))
        );
    }

    /// CR 108.3 + CR 608.2c: "its owner" refers to an object target's owner,
    /// not a companion player target that happens to precede it.
    #[test]
    fn parent_target_owner_ignores_player_targets() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Owned Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&creature).unwrap().owner = PlayerId(0);
        let ability = make_simple_ability(
            vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(creature)],
            ObjectId(0),
        );

        assert_eq!(
            parent_target_owner(&ability, &state),
            Some(PlayerId(0)),
            "ParentTargetOwner must skip player targets and read the object owner"
        );
    }

    /// An ability with no targets has no parent target — returns None.
    #[test]
    fn parent_target_controller_returns_none_for_empty_targets() {
        let state = GameState::new_two_player(42);
        let ability = make_simple_ability(vec![], ObjectId(0));
        assert_eq!(
            parent_target_controller(&ability, &state),
            None,
            "An ability with no targets has no parent target controller"
        );
    }

    /// CR 608.2c + CR 400.7j (issue #2890): Parent-target player anaphors must
    /// resolve from `effect_context_object` when inherited targets are absent.
    #[test]
    fn parent_target_controller_falls_back_to_effect_context_object() {
        use crate::types::ability::CostPaidObjectSnapshot;
        use crate::types::game_state::LKISnapshot;

        let state = GameState::new_two_player(42);
        let gone_id = ObjectId(77);
        let mut ability = make_simple_ability(vec![], ObjectId(0));
        ability.effect_context_object = Some(CostPaidObjectSnapshot {
            object_id: gone_id,
            lki: LKISnapshot {
                name: "Exiled Creature".to_string(),
                power: Some(2),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                controller: PlayerId(1),
                owner: PlayerId(1),
                card_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: std::collections::HashMap::new(),
            },
        });

        assert_eq!(
            parent_target_controller(&ability, &state),
            Some(PlayerId(1)),
            "effect_context_object must supply the parent controller when targets are empty"
        );
        assert_eq!(
            parent_target_owner(&ability, &state),
            Some(PlayerId(1)),
            "effect_context_object must supply the parent owner when targets are empty"
        );
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature))
    }

    /// CR 115.1 + CR 614.9 (Defect 1 / Nit 2): Soltari Guerrillas's "...deals
    /// that damage to target creature instead" redirect destination MUST surface
    /// a creature target slot through `build_target_slots`. This drives the REAL
    /// targeting pipeline — deleting the `collect_target_slots`
    /// CreateDamageReplacement branch makes this fail.
    #[test]
    fn build_target_slots_surfaces_redirect_creature_slot() {
        use crate::types::ability::DamageRedirectTarget;
        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Soltari".into(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Redirect Target".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: None,
                redirect_object_filter: Some(creature_filter()),
                recipient_object_filter: None,
            },
            vec![],
            host,
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("redirect slot must build");
        assert_eq!(slots.len(), 1, "exactly one redirect-destination slot");
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(creature)),
            "the redirect creature must be a legal target, got {:?}",
            slots[0].legal_targets
        );
    }

    /// CR 115.1 + CR 614.9 (Defect 3 / Nit 1+2): Jade Monolith's "would deal
    /// damage to target creature" original recipient MUST surface a creature
    /// target slot — without it the shield hosts on Jade with no recipient
    /// scoping and redirects damage to ANY creature. Deleting the
    /// `recipient_object_filter` arm of the `collect_target_slots` branch makes
    /// this fail.
    #[test]
    fn build_target_slots_surfaces_recipient_creature_slot() {
        use crate::types::ability::DamageRedirectTarget;
        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jade".into(),
            Zone::Battlefield,
        );
        let protected = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Protected Creature".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&protected)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::ChosenDamageSource),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: Some(creature_filter()),
            },
            vec![],
            host,
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("recipient slot must build");
        assert_eq!(slots.len(), 1, "exactly one original-recipient slot");
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(protected)),
            "the protected creature must be a legal target, got {:?}",
            slots[0].legal_targets
        );
    }

    /// Ordering contract (Nit 1): when BOTH filters are present the recipient
    /// slot is surfaced FIRST, then the redirect slot — matching the resolver's
    /// `chosen_target_object(_, 0)` / `chosen_redirect_object` indexing.
    #[test]
    fn build_target_slots_recipient_slot_precedes_redirect_slot() {
        use crate::types::ability::DamageRedirectTarget;
        let mut state = GameState::new_two_player(42);
        let host = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hybrid".into(),
            Zone::Battlefield,
        );
        for (cid, name) in [(2u64, "A"), (3, "B")] {
            let id = create_object(
                &mut state,
                CardId(cid),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: None,
                redirect_object_filter: Some(creature_filter()),
                recipient_object_filter: Some(creature_filter()),
            },
            vec![],
            host,
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("two slots must build");
        assert_eq!(
            slots.len(),
            2,
            "recipient + redirect slots must both surface when both filters are set"
        );
    }

    /// Spawn `count` creatures on the battlefield controlled by `controller`.
    fn spawn_creatures(
        state: &mut crate::types::game_state::GameState,
        controller: PlayerId,
        count: usize,
    ) {
        for index in 0..count {
            let creature = crate::game::zones::create_object(
                state,
                crate::types::identifiers::CardId(index as u64 + 1),
                controller,
                format!("Creature {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
        }
    }

    fn single_target_mode(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    /// CR 700.2: A two-mode modal where both chosen modes target — each slot's
    /// label must name the mode it belongs to, in sorted printed order, and the
    /// labels vector must be the same length as the slots vector.
    #[test]
    fn build_target_slots_labelled_aligns_labels_with_chosen_modes() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        spawn_creatures(&mut state, PlayerId(0), 2);

        let abilities = vec![
            single_target_mode(Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            }),
            single_target_mode(Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }),
        ];
        let descriptions = vec![
            "Destroy target creature.".to_string(),
            "Tap target creature.".to_string(),
        ];

        let (slots, labels) = build_target_slots_labelled(
            &state,
            &abilities,
            &[1, 0],
            &descriptions,
            ObjectId(10),
            PlayerId(0),
            &SpellContext::default(),
            None,
        )
        .expect("labelled modal slots build");

        assert_eq!(slots.len(), 2);
        assert_eq!(labels.len(), slots.len(), "labels parallel slots");
        // Indices sorted to printed order [0, 1] regardless of chosen order.
        assert_eq!(labels[0].as_deref(), Some("Destroy target creature."));
        assert_eq!(labels[1].as_deref(), Some("Tap target creature."));
    }

    /// A single chosen mode that contributes two slots (effect + sub-ability)
    /// must have both slots share that mode's head label.
    #[test]
    fn build_target_slots_labelled_multi_clause_single_mode_shares_label() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        spawn_creatures(&mut state, PlayerId(0), 2);

        let mut mode = single_target_mode(Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        });
        mode.sub_ability = Some(Box::new(single_target_mode(Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        })));
        let abilities = vec![mode];
        let descriptions = vec!["Destroy then tap.".to_string()];

        let (slots, labels) = build_target_slots_labelled(
            &state,
            &abilities,
            &[0],
            &descriptions,
            ObjectId(10),
            PlayerId(0),
            &SpellContext::default(),
            None,
        )
        .expect("multi-clause single mode builds");

        assert_eq!(slots.len(), 2, "effect + sub-ability each surface a slot");
        assert_eq!(labels.len(), slots.len());
        assert!(
            labels
                .iter()
                .all(|l| l.as_deref() == Some("Destroy then tap.")),
            "both clause slots share the mode head label"
        );
    }

    /// A per-opponent fan-out mode must propagate its mode label to every
    /// surfaced slot (player slot + object slot per opponent).
    #[test]
    fn build_target_slots_labelled_per_opponent_fanout_inherits_label() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        spawn_creatures(&mut state, PlayerId(1), 1);

        let mode = single_target_mode(Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        });
        let abilities = vec![mode];
        let descriptions = vec!["Tap a creature.".to_string()];

        let (slots, labels) = build_target_slots_labelled(
            &state,
            &abilities,
            &[0],
            &descriptions,
            ObjectId(10),
            PlayerId(0),
            &SpellContext::default(),
            None,
        )
        .expect("fan-out modal slots build");

        assert_eq!(labels.len(), slots.len());
        assert!(
            labels
                .iter()
                .all(|l| l.as_deref() == Some("Tap a creature.")),
            "every fanned-out slot inherits the mode label"
        );
    }

    /// A single chosen index with no matching `mode_descriptions` entry yields a
    /// `None` label per slot (graceful degradation — no panic on missing text).
    #[test]
    fn build_target_slots_labelled_missing_description_yields_none() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        spawn_creatures(&mut state, PlayerId(0), 1);

        let abilities = vec![single_target_mode(Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        })];

        let (slots, labels) = build_target_slots_labelled(
            &state,
            &abilities,
            &[0],
            &[],
            ObjectId(10),
            PlayerId(0),
            &SpellContext::default(),
            None,
        )
        .expect("missing-description modal slots build");

        assert_eq!(labels.len(), slots.len());
        assert!(
            labels.iter().all(|l| l.is_none()),
            "no description -> None labels"
        );
    }

    // -----------------------------------------------------------------------
    // CR 601.2c + CR 115.3: per-instance object-target distinctness
    // -----------------------------------------------------------------------

    /// Build a multi-target "up to N target creatures" ability (Mothman-shaped)
    /// whose single multi_target run surfaces up to `max` slots — all ONE
    /// instance of "target".
    fn up_to_n_target_creatures(
        source: ObjectId,
        controller: PlayerId,
        max: usize,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::creature()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![],
            source,
            controller,
        );
        ability.multi_target = Some(MultiTargetSpec::fixed(0, max));
        ability
    }

    /// CR 601.2c + CR 115.3 (offered set): in a multi_target "up to N target
    /// creatures" run, once creature A is chosen in slot 0 the spec-aware
    /// offered set for slot 1 must NOT contain A — it is the same instance of
    /// "target". The other distinct creatures remain offerable.
    #[test]
    fn multi_target_same_instance_offered_set_excludes_prior_choice() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let a = create_creature(&mut state, PlayerId(0), CardId(1), "A");
        let b = create_creature(&mut state, PlayerId(0), CardId(2), "B");
        let c = create_creature(&mut state, PlayerId(0), CardId(3), "C");

        let ability = up_to_n_target_creatures(ObjectId(900), PlayerId(0), 3);
        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");
        assert!(
            specs.len() >= 2,
            "multi_target run should surface >= 2 slots"
        );

        // All slots in the run share ONE instance id.
        assert!(
            specs.windows(2).all(|w| w[0].instance == w[1].instance),
            "every slot of one multi_target run is the same instance of \"target\""
        );

        // Slot 0 offered set: all three creatures (no prior selection).
        let slot0 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 0, &[]);
        for id in [a, b, c] {
            assert!(
                slot0.contains(&TargetRef::Object(id)),
                "slot 0 should offer every legal creature"
            );
        }

        // After choosing A in slot 0, slot 1 must exclude A but still offer B, C.
        let prior = vec![Some(TargetRef::Object(a))];
        let slot1 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 1, &prior);
        assert!(
            !slot1.contains(&TargetRef::Object(a)),
            "CR 601.2c: A already chosen in this instance must not be offered again"
        );
        assert!(
            slot1.contains(&TargetRef::Object(b)) && slot1.contains(&TargetRef::Object(c)),
            "other distinct creatures remain legal for the next slot"
        );
    }

    /// CR 601.2c + CR 115.3 (validate path): selecting the SAME object twice in
    /// one multi_target instance must be rejected; an all-distinct selection is
    /// accepted.
    #[test]
    fn multi_target_same_instance_validate_rejects_duplicate_object() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let a = create_creature(&mut state, PlayerId(0), CardId(1), "A");
        let b = create_creature(&mut state, PlayerId(0), CardId(2), "B");

        let ability = up_to_n_target_creatures(ObjectId(900), PlayerId(0), 2);
        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");

        // [A, A] in one instance is illegal.
        let dup = vec![Some(TargetRef::Object(a)), Some(TargetRef::Object(a))];
        assert!(
            validate_selected_slots_with_specs(&state, &ability, &specs, &target_slots, &dup, &[],)
                .is_err(),
            "CR 601.2c: the same object can't fill two slots of one instance"
        );

        // [A, B] (distinct) is legal.
        let distinct = vec![Some(TargetRef::Object(a)), Some(TargetRef::Object(b))];
        assert!(
            validate_selected_slots_with_specs(
                &state,
                &ability,
                &specs,
                &target_slots,
                &distinct,
                &[],
            )
            .is_ok(),
            "two distinct legal creatures must satisfy the multi_target instance"
        );
    }

    /// CR 601.2c + CR 115.3 (THE binding cross-instance Example): "Destroy
    /// target artifact and target land"-shaped abilities use the word "target"
    /// in two PLACES → two separate instances → the same object may be chosen
    /// once for each. A two-single-target `ExchangeControl` ability surfaces two
    /// slots with DISTINCT instance ids; one creature legal for both must be
    /// offered AND accepted in both slots.
    #[test]
    fn cross_instance_object_reuse_is_allowed() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let shared = create_creature(&mut state, PlayerId(0), CardId(1), "Shared");

        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::Typed(TypedFilter::creature()),
                target_b: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );
        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");
        assert_eq!(specs.len(), 2, "two target places -> two specs");
        assert_ne!(
            specs[0].instance, specs[1].instance,
            "CR 601.2c: two separate 'target' places are DIFFERENT instances"
        );

        // Slot 0 offers the shared creature.
        let slot0 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 0, &[]);
        assert!(slot0.contains(&TargetRef::Object(shared)));

        // After choosing it in slot 0, slot 1 (a DIFFERENT instance) STILL offers
        // it — cross-instance reuse is legal.
        let prior = vec![Some(TargetRef::Object(shared))];
        let slot1 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 1, &prior);
        assert!(
            slot1.contains(&TargetRef::Object(shared)),
            "CR 601.2c: a different instance of 'target' may reuse the same object"
        );

        // And [shared, shared] validates across the two distinct instances.
        let reuse = vec![
            Some(TargetRef::Object(shared)),
            Some(TargetRef::Object(shared)),
        ];
        assert!(
            validate_selected_slots_with_specs(
                &state,
                &ability,
                &specs,
                &target_slots,
                &reuse,
                &[],
            )
            .is_ok(),
            "CR 601.2c artifact+land Example: same object accepted in both separate instances"
        );
    }

    /// CR 115.4: Arc Trail class — "N damage to any target and M damage to any
    /// other target" uses two instances of "target", but the second must differ
    /// from the first.
    #[test]
    fn any_other_target_excludes_prior_cast_choices_across_instances() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let bear = create_creature(&mut state, PlayerId(1), CardId(1), "Bear");

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::Another]),
                ),
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");
        assert_eq!(specs.len(), 2);

        let prior = vec![Some(TargetRef::Object(bear))];
        let slot1 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 1, &prior);
        assert!(
            !slot1.contains(&TargetRef::Object(bear)),
            "any other target must exclude the first chosen target"
        );

        let dup = vec![Some(TargetRef::Object(bear)), Some(TargetRef::Object(bear))];
        assert!(
            validate_selected_slots_with_specs(&state, &ability, &specs, &target_slots, &dup, &[],)
                .is_err(),
            "reusing the same object for both Arc Trail targets must be rejected"
        );
    }

    /// CR 115.4 + CR 601.2c: typed "another target" filters use the same
    /// prior-target exclusion as "any other target"; the difference is only the
    /// candidate population.
    #[test]
    fn typed_another_target_excludes_prior_cast_choices_across_instances() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let bear = create_creature(&mut state, PlayerId(1), CardId(1), "Bear");

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::Another]),
                ),
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");
        assert_eq!(specs.len(), 2);

        let prior = vec![Some(TargetRef::Object(bear))];
        let slot1 = legal_targets_for_spec_slot(&state, &ability, &specs, &target_slots, 1, &prior);
        assert!(
            !slot1.contains(&TargetRef::Object(bear)),
            "typed another-target slot must not offer the first chosen target"
        );

        let dup = vec![Some(TargetRef::Object(bear)), Some(TargetRef::Object(bear))];
        assert!(
            validate_selected_slots_with_specs(&state, &ability, &specs, &target_slots, &dup, &[],)
                .is_err(),
            "reusing the same creature for target creature and another target creature must be rejected"
        );
    }

    /// CR 115.1 + CR 601.2c: the `DifferentObjectControllers` constraint still
    /// rejects same-controller object pairs after the per-slot distinctness
    /// filter is in place (no regression — distinctness and the controller
    /// constraint are orthogonal gates).
    #[test]
    fn different_object_controllers_constraint_still_rejects_same_controller_pair() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        // Two distinct creatures both controlled by P0.
        let a = create_creature(&mut state, PlayerId(0), CardId(1), "A");
        let b = create_creature(&mut state, PlayerId(0), CardId(2), "B");

        let ability = up_to_n_target_creatures(ObjectId(900), PlayerId(0), 2);
        let specs = target_slot_specs(&state, &ability);
        let target_slots = build_target_slots(&state, &ability).expect("slots build");

        // Distinct objects, but both controlled by P0 -> the constraint rejects.
        let same_controller = vec![Some(TargetRef::Object(a)), Some(TargetRef::Object(b))];
        assert!(
            validate_selected_slots_with_specs(
                &state,
                &ability,
                &specs,
                &target_slots,
                &same_controller,
                &[TargetSelectionConstraint::DifferentObjectControllers],
            )
            .is_err(),
            "DifferentObjectControllers must still reject two P0-controlled objects"
        );
    }

    /// CR 609.7 + CR 601.2c: A source-scoped `PreventDamage` ("prevent all
    /// damage target instant or sorcery spell would deal this turn") surfaces
    /// exactly one target slot whose legal targets are the spell(s) on the
    /// stack. Drives the real targeting pipeline — deleting the
    /// `prevent_damage_source_slot_filter` arm in `collect_target_slots` makes
    /// this fail.
    #[test]
    fn build_target_slots_surfaces_source_scoped_spell_slot() {
        use crate::types::ability::{PreventionAmount, PreventionScope};
        use crate::types::game_state::CastingVariant;
        let mut state = GameState::new_two_player(42);
        let dromoka = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dromoka's Command".into(),
            Zone::Stack,
        );
        // An instant spell on the stack — the choosable source.
        let spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Lightning Bolt".into(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.objects.get_mut(&spell).unwrap().card_types.core_types = vec![CoreType::Instant];

        let source_filter = TargetFilter::And {
            filters: vec![
                TargetFilter::ParentTargetSlot { index: 0 },
                TargetFilter::And {
                    filters: vec![
                        TargetFilter::StackSpell,
                        TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Instant)),
                    ],
                },
            ],
        };
        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(source_filter),
                prevention_duration: None,
            },
            vec![],
            dromoka,
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("source slot must build");
        assert_eq!(slots.len(), 1, "exactly one source-scope slot");
        assert!(
            slots[0].legal_targets.contains(&TargetRef::Object(spell)),
            "the stack spell must be a legal source target, got {:?}",
            slots[0].legal_targets
        );
    }

    /// CR 115.1 + CR 613.1b: non-trigger mass gain-control effects whose
    /// population filter references `target player` still need a stack target
    /// slot for that player. `GainControlAll::target_filter()` intentionally
    /// returns None because the field is not an object target slot, so this
    /// regression drives the companion-player-slot fallback.
    #[test]
    fn gain_control_all_target_player_filter_surfaces_player_slot() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainControlAll {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("player slot should build");
        assert_eq!(
            slots.len(),
            1,
            "GainControlAll needs exactly one player target slot"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Player(PlayerId(1))),
            "target player slot must offer P1, got {:?}",
            slots[0].legal_targets
        );
    }

    /// CR 700.2d + CR 608.2c: Chaining two modes via `build_chained_resolved`
    /// appends the later mode as the earlier mode's `sub_ability` with
    /// `sub_link == SequentialSibling` — chained modes are independent
    /// instructions, not continuations.
    #[test]
    fn build_chained_resolved_tags_appended_mode_sequential_sibling() {
        let mode_a = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mode_b = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        );
        let abilities = vec![mode_a, mode_b];
        let chained =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();
        let sub = chained
            .sub_ability
            .as_deref()
            .expect("second mode appended as sub");
        assert_eq!(
            sub.sub_link,
            SubAbilityLink::SequentialSibling,
            "appended mode root must be tagged SequentialSibling"
        );
    }
}
