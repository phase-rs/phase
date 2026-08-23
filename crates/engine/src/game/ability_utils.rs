#[cfg(test)]
use crate::types::ability::TapStateChange;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost,
    CardTypeSetSource, CastManaSpentMetric, CombatRelationSubject, ControllerRef,
    CounterMoveSelection, DamageSource, Effect, EffectKind, EffectScope, FilterProp,
    GameRestriction, ModalChoice, ModalSelectionCondition, ModalSelectionConstraint,
    MultiTargetSpec, ObjectScope, PlayerFilter, PlayerScope, PtValue, QuantityExpr, QuantityRef,
    ResolvedAbility, RestrictionPlayerScope, SpellContext, SubAbilityLink, TargetChoiceTiming,
    TargetFilter, TargetRef, TriggerDefinition, TypeFilter, TypedFilter,
};
// CR 601.2c: mana recipient / count-source role slot gate.
use crate::types::ability::mana_multi_role;
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::game_state::{
    GameState, PtDirection, TargetEffectDetail, TargetSelectionConstraint, TargetSelectionProgress,
    TargetSelectionSlot,
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

/// CR 601.2b + CR 602.2b: publish an announce-time-locked X onto an ability being
/// announced. **Single computation authority** for the announce-locked X channel.
///
/// A "where X is <count> as you cast this spell" / "… as you activate this ability"
/// clause defines X by the object's own text and pins the measurement to the
/// announcement step. CR 602.2b makes an activated ability's announcement identical to a
/// spell's (rules 601.2b–i), so ONE computation serves both surfaces — and a loyalty
/// ability, being an activated ability, rides the same path.
///
/// The value is published through `chosen_x`, the object's single X channel (CR 107.3i:
/// "all instances of X on an object have the same value"). Every
/// `QuantityRef::Variable("X")` on the ability already reads it, so the announced target
/// count (CR 601.2c), the divided-damage pool (CR 601.2d), and any resolution-time amount
/// (CR 608.2) all observe the SAME number — which is exactly what the printed qualifier
/// demands and what CR 107.3c would otherwise let drift ("the value of X may change while
/// that spell or ability is on the stack").
///
/// MUST be called BEFORE target selection: CR 601.2b precedes CR 601.2c, and
/// `resolve_multi_target_bounds` fails closed ("Target count requires a resolved quantity
/// before target selection") rather than silently counting an unresolved X as 0.
///
/// Idempotent via the `chosen_x.is_some()` gate, so a re-announced/resumed cast cannot
/// re-measure the count against a board that has since changed.
pub(crate) fn publish_announced_x(
    state: &GameState,
    resolved: &mut ResolvedAbility,
    controller: PlayerId,
    source_id: ObjectId,
) {
    if resolved.chosen_x.is_some() {
        return;
    }
    let Some(expr) = resolved.announced_x.clone() else {
        return;
    };
    let value = super::quantity::resolve_quantity(state, &expr, controller, source_id);
    // CR 107.3: X is never negative.
    resolved.set_chosen_x_recursive(u32::try_from(value.max(0)).unwrap_or(0));
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
    resolved.optional_player = def.optional_player.clone();
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
    // CR 601.2b + CR 602.2b: carry the announce-time-locked definition of X through
    // to the resolved ability, where the announcement step evaluates it once into
    // `chosen_x`. Without this copy the parsed definition never reaches the pending
    // cast/activation and every `Variable("X")` on the ability resolves to 0.
    resolved.announced_x = def.announced_x.clone();
    resolved.cant_be_copied = def.cant_be_copied;
    resolved.description = def.description.clone();
    resolved.forward_result = def.forward_result;
    resolved.unless_pay = def.unless_pay.clone();
    // CR 601.2d + CR 603.3d: Preserve the unassigned division unit until the
    // ordinary stack-announcement authority assigns concrete portions.
    resolved.distribute = def.distribute.clone();
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
    // CR 702.1c ("the same is true") + CR 608.2c (written order): Carry the
    // replication marker through so `resolve_chain_body` evaluates a
    // `ReplicatedOrBranch` per-item OR-branch (Mutable Pupa, Kathril)
    // independently of a preceding sibling's failed gate. Without this copy the
    // parser-stamped `SiblingCondition` never reaches the resolved sub and the
    // keyword list collapses after the first false gate.
    resolved.sibling_condition = def.sibling_condition;
    // CR 700.2b + CR 603.3c: Carry the reflexive modal choice + per-mode abilities
    // through so try_materialize_reflexive_trigger can route a gated modal
    // trigger (Caesar) to AbilityModeChoice instead of resolving the modes
    // unconditionally.
    resolved.modal = def.modal.clone();
    resolved.mode_abilities = def.mode_abilities.clone();
    resolved
}

/// CR 608.2c: Apply an "instead" swap from a sub-ability override
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
/// min_x_value, forward_result, unless_pay, distribution, distribute,
/// target_selection_mode.
///
/// Fields preserved from `parent`: controller, source_id, kind, context,
/// original_controller, scoped_player, chosen_x, cost_paid_object,
/// ability_index, may_trigger_origin.
///
/// `targets`: an override with its own declared target filter takes its
/// independently resolution-validated target list from `sub`; a context-ref
/// override preserves the parent's announced targets. CR 608.2b re-validates
/// every chain node against its own filter, so retaining a nonempty but
/// narrower parent list would silently discard targets legal only for the
/// override.
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
    overridden.distribute = sub.distribute.clone();
    overridden.target_selection_mode = sub.target_selection_mode;
    overridden.target_chooser = sub.target_chooser.clone();
    // CR 608.2b + CR 601.2c: a swapped-in effect with its own declared target
    // resolves against its OWN resolution-validated targets. The parent may
    // retain a subset that still meets its narrower filter while dropping other
    // targets that are legal only for the broad override, so emptiness is not a
    // sound proxy for whether to adopt the override list. Context refs have no
    // independently declared target and must retain the parent's target list.
    if sub
        .effect
        .target_filter()
        .is_some_and(|filter| !filter.is_context_ref())
    {
        overridden.targets = sub.targets.clone();
    }
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
pub fn ordered_selected_mode_indices(indices: &[usize]) -> Vec<usize> {
    let mut ordered = indices.to_vec();
    ordered.sort_unstable();
    ordered
}

/// CR 700.2a + CR 700.2b + CR 700.2d + CR 608.2c: Return selected mode
/// descriptions in the printed instruction order used to resolve them. Repeated
/// indices remain repeated, while a missing legacy description is omitted.
pub fn selected_mode_labels(mode_descriptions: &[String], indices: &[usize]) -> Vec<String> {
    ordered_selected_mode_indices(indices)
        .into_iter()
        .filter_map(|index| mode_descriptions.get(index).cloned())
        .collect()
}

pub fn build_chained_resolved(
    abilities: &[AbilityDefinition],
    indices: &[usize],
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<ResolvedAbility, EngineError> {
    if indices.is_empty() {
        // CR 700.2: the modes are the bulleted options, chosen per "instructions
        // for a player to choose A NUMBER of those options" — and under "choose up
        // to one" that number may be zero. The ability still resolves; it just has
        // no instructions to perform. (Not CR 700.2a, which is about WHEN modes are
        // chosen and illegal modes; not CR 700.2i, whose "choose up to" is specific
        // to pawprint {P} worth of modes.)
        return Ok(ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: Vec::new(),
                duration: None,
                target: None,
                end_cost: None,
            },
            Vec::new(),
            source_id,
            controller,
        ));
    }

    let ordered = ordered_selected_mode_indices(indices);

    let mut result: Option<ResolvedAbility> = None;
    for (ordinal, &idx) in ordered.iter().enumerate().rev() {
        let def = abilities
            .get(idx)
            .ok_or_else(|| EngineError::InvalidAction(format!("Mode index {idx} out of range")))?;
        let mut resolved = build_resolved_from_def(def, source_id, controller);
        // CR 700.2 ("each of those options is a mode") + CR 700.2d: stamp this
        // mode root with its OCCURRENCE ORDINAL within the ordered selection —
        // taken from `enumerate()`, never from `idx`. `ordered_selected_mode_indices`
        // preserves duplicates, so an `allow_repeat_modes` card (Eldrazi
        // Confluence, `[1, 1]`) has two distinct instructions at one printed
        // index; keying on `idx` would collapse them into one. This is the ONLY
        // write site for the field (see its doc on `ResolvedAbility`).
        resolved.modal_instruction_ordinal = Some(ordinal);
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
struct SlotAccumulator {
    slots: Vec<TargetSelectionSlot>,
    labels: Vec<Option<String>>,
    /// Mode label applied to every slot pushed until reset. Set by
    /// `build_target_slots_labelled` before collecting each mode; `None` for
    /// non-modal collection.
    current_label: Option<String>,
    /// CR 601.2c + CR 115.1: announcing player applied to every slot pushed while
    /// the currently-recursed link's `target_chooser` resolves to a non-controller
    /// player ("of an opponent's choice"). `None` means the controller is the
    /// announcer (the CR-601.2c default). Set/restored by `collect_target_slots`
    /// per link so each chained sub-ability stamps only its own slots.
    current_chooser: Option<PlayerId>,
    /// CR 115.1: effect kind of the link currently being recursed, applied to
    /// every slot it pushes. Scoped exactly like `current_chooser`: set before
    /// a link's slots are collected and restored afterwards, so a chained
    /// sub-ability's slots carry that sub-ability's effect rather than the
    /// head link's. `collect_target_slots` recurses per link, so every slot
    /// pushed within one frame belongs to that frame's `ability.effect`.
    current_effect_kind: EffectKind,
    /// CR 115.1: the discriminating payload of the link currently being
    /// recursed, read by `target_effect_detail`. Scoped exactly like
    /// `current_effect_kind`.
    current_effect_detail: TargetEffectDetail,
}

impl Default for SlotAccumulator {
    /// `current_effect_kind` seeds to `NoOp` purely to have a value: both
    /// constructors (`build_target_slots`, `build_target_slots_labelled`) call
    /// `collect_target_slots` before any push, and that sets the real kind, so
    /// the seed can never reach a slot. `EffectKind` has no `Default` of its
    /// own and should not gain one for this — a 231-variant effect tag has no
    /// meaningful default outside this accumulator.
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            labels: Vec::new(),
            current_label: None,
            current_chooser: None,
            current_effect_kind: EffectKind::NoOp,
            current_effect_detail: TargetEffectDetail::None,
        }
    }
}

impl SlotAccumulator {
    /// Push a slot and its mode label together. The label is `current_label` at
    /// push time, keeping `labels` and `slots` index-parallel by construction.
    /// The slot's `chooser` is stamped from `current_chooser` unless the slot
    /// already carries one (no producer sets it today, so the default path wins).
    fn push(&mut self, mut slot: TargetSelectionSlot) {
        if slot.chooser.is_none() {
            slot.chooser = self.current_chooser;
        }
        self.slots.push(slot);
        self.labels.push(self.current_label.clone());
    }
}

/// Result of target construction while an ability is being announced.
///
/// `RequiresChosenX` is distinct from an illegal target set: CR 601.2b requires
/// announcing X before the CR 601.2c target declaration can be evaluated.
pub(crate) enum TargetSlotBuildOutcome {
    Slots(Vec<TargetSelectionSlot>),
    RequiresChosenX,
}

enum TargetSlotBuildError {
    Engine(EngineError),
    RequiresChosenX,
}

impl From<EngineError> for TargetSlotBuildError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

pub(crate) fn unresolved_x_target_construction_error() -> EngineError {
    EngineError::ActionNotAllowed(
        "Target count requires a resolved quantity before target selection".to_string(),
    )
}

impl From<TargetSlotBuildError> for EngineError {
    fn from(error: TargetSlotBuildError) -> Self {
        match error {
            TargetSlotBuildError::Engine(error) => error,
            TargetSlotBuildError::RequiresChosenX => unresolved_x_target_construction_error(),
        }
    }
}

/// CR 601.2b/c + CR 602.2b: Collect target slots while preserving the
/// announce-time distinction between an unresolved X and an illegal target set.
pub(crate) fn build_target_slots_for_announcement(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<TargetSlotBuildOutcome, EngineError> {
    let mut acc = SlotAccumulator::default();
    match collect_target_slots(state, ability, &mut acc) {
        Ok(()) => Ok(TargetSlotBuildOutcome::Slots(acc.slots)),
        Err(TargetSlotBuildError::RequiresChosenX) => Ok(TargetSlotBuildOutcome::RequiresChosenX),
        Err(TargetSlotBuildError::Engine(error)) => Err(error),
    }
}

/// CR 601.2c / CR 602.2b: Collect all target slots for an ability chain. Each targeting
/// effect in the chain produces a slot whose legal targets are computed from the game state.
pub fn build_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<Vec<TargetSelectionSlot>, EngineError> {
    match build_target_slots_for_announcement(state, ability)? {
        TargetSlotBuildOutcome::Slots(slots) => Ok(slots),
        TargetSlotBuildOutcome::RequiresChosenX => {
            Err(TargetSlotBuildError::RequiresChosenX.into())
        }
    }
}

/// CR 601.2b + CR 702.33a/702.194c: "instead" spells with a target-dependent
/// additional cost (Kicker, e.g. Bloodchief's Thirst; or a queue-synthesized
/// cost such as Teamwork, e.g. Too Evil to Stay Dead) replace their base
/// targeting when the cost is paid. Castability must admit the paid-cost
/// target assignment when the unpaid assignment is unsatisfiable.
///
/// RESOLVED (finding #1): this used to gate the cast-time
/// `additional_cost_paid = true` propagation on `AdditionalCost::Kicker`
/// alone, so every other `AdditionalCost`-"instead" card's broad override was
/// silently skipped here. Generalized to admit Kicker OR a non-empty effective
/// queue (`build_effective_additional_cost_queue` — Casualty/Offspring/Squad/
/// Replicate/Bargain/Teamwork), mirroring the cast-time deferral gates in
/// `casting.rs`.
///
/// INHERITED LATENT GAP (out of scope, mirrors kicker's pre-existing
/// behavior): this reports the spell castable once a legal paid-cost target
/// assignment exists, without proving the additional cost itself is payable
/// (e.g. enough eligible creatures to tap for Teamwork's total-power
/// requirement) — actual payability is validated at payment time, not here.
pub fn additional_cost_instead_spell_has_legal_targets(
    state: &GameState,
    ability_def: &AbilityDefinition,
    object_id: ObjectId,
    player: PlayerId,
) -> bool {
    let has_kicker_cost = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.as_ref())
        .is_some_and(|additional| matches!(additional, AdditionalCost::Kicker { .. }));
    let has_queue_cost =
        !super::casting_costs::build_effective_additional_cost_queue(state, player, object_id)
            .is_empty();
    if !has_kicker_cost && !has_queue_cost {
        return false;
    }
    // Walk past GiftDelivery wrappers to find AdditionalCostPaidInstead.
    let mut instead_node = ability_def.sub_ability.as_deref();
    let mut found_instead = false;
    while let Some(sub) = instead_node {
        if matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        ) {
            found_instead = true;
            break;
        }
        instead_node = sub.sub_ability.as_deref();
    }
    if !found_instead {
        return false;
    }
    let mut resolved = build_resolved_from_def(ability_def, object_id, player);
    resolved.context.additional_cost_paid = true;
    resolved.set_context_recursive(resolved.context.clone());
    // CR 601.2c: a queue-synthesized "instead" cost only broadens castability when the
    // override re-selects a REAL (non-context-ref) target — mirror the cast-time gate
    // (requires_additional_cost_declaration_before_targets). A context-ref override
    // ("that permanent" = ParentTarget, e.g. Torch the Tower / Bargain) does NOT broaden;
    // it inherits the base clause's target requirement, so fall through to the base
    // castability check. Kicker is unaffected (has_kicker_cost short-circuits).
    if !has_kicker_cost
        && !crate::game::casting::requires_additional_cost_declaration_before_targets(&resolved)
    {
        return false;
    }
    match build_target_slots(state, &resolved) {
        Ok(slots) if slots.is_empty() => true,
        Ok(slots) => {
            let constraints = resolved
                .sub_ability
                .as_ref()
                .map(|sub| &sub.target_constraints)
                .unwrap_or(&resolved.target_constraints);
            has_legal_target_assignment_for_ability(state, &resolved, &slots, constraints)
        }
        Err(_) => false,
    }
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
    let ordered = ordered_selected_mode_indices(indices);

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
                let obj_opt = state.objects.get(id);
                // CR 608.2h: reset_for_battlefield_exit() reverts `controller`
                // to the owner when a permanent leaves the battlefield. For any
                // object that is no longer on the battlefield, the LKI snapshot
                // (captured just before the zone change) holds the correct
                // pre-exit controller. Prefer it over the live — post-reset —
                // value so that "its controller" anchors on who controlled the
                // permanent at departure, not the owner who now appears to
                // control the exiled/graved object.
                let off_battlefield = obj_opt.is_none_or(|obj| obj.zone != Zone::Battlefield);
                if off_battlefield {
                    state
                        .lki_cache
                        .get(id)
                        .map(|lki| lki.controller)
                        .or_else(|| obj_opt.map(|obj| obj.controller))
                } else {
                    obj_opt.map(|obj| obj.controller)
                }
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
    // CR 107.3m + CR 700.2d: dynamic modal max ("choose up to X") resolves the
    // cast {X} live and clamps to mode_count (a player can't choose more modes
    // than exist).
    if let Some(expr) = &modal.dynamic_max_choices {
        let resolved = super::quantity::resolve_quantity(state, expr, player, source_id);
        // CR 700.2i: pawprint modals reinterpret `max_choices` as a point budget,
        // not a mode-count cap — do not clamp dynamic budgets to `mode_count`.
        effective.max_choices = if modal.mode_pawprints.is_empty() {
            (resolved.max(0) as usize).min(modal.mode_count)
        } else {
            resolved.max(0) as usize
        };
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

/// CR 700.2a / CR 700.2e: every player the modal's `chooser` admits, in APNAP
/// order.
///
/// `PlayerFilter::Controller` — every standard modal and the `you choose —`
/// alias — is the controller alone, without consulting
/// `effects::matches_player_scope`. Any other filter (CR 700.2e, "an opponent
/// chooses …") is resolved through that canonical authority over APNAP order.
///
/// Spell announcement wants only the first admitted player, which is what
/// `casting::resolve_modal_chooser` takes; trigger construction needs the whole
/// set, because more than one non-controller candidate makes the controller's
/// CR 700.2e chooser selection a real choice rather than a derivation.
pub(crate) fn modal_chooser_candidates(
    state: &GameState,
    modal: &ModalChoice,
    controller: PlayerId,
    source_id: ObjectId,
) -> Vec<PlayerId> {
    if modal.chooser == PlayerFilter::Controller {
        return vec![controller];
    }
    players::apnap_order(state)
        .into_iter()
        .filter(|&p| {
            super::effects::matches_player_scope(state, p, &modal.chooser, controller, source_id)
        })
        .collect()
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
    modal_spell_mode_ability_refs(obj).cloned().collect()
}

/// Borrowing view of [`modal_spell_mode_abilities`] — the same predicate
/// without the per-call clone, for read-only consumers that only inspect the
/// modes (AI classification, coverage reporting). Both share this one
/// definition of "which abilities on this object are its printed modes" so the
/// owned and borrowed forms can never disagree.
pub fn modal_spell_mode_ability_refs(
    obj: &crate::game::game_object::GameObject,
) -> impl Iterator<Item = &AbilityDefinition> {
    obj.abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell)
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

/// CR 700.2a-b + CR 115.1: Cap a modal choice by the largest mode set whose
/// combined targeting slots can satisfy modal target constraints.
///
/// Per-mode filtering only proves each mode is individually legal. Modal
/// constraints such as `DifferentTargetPlayers` can make a larger selected set
/// impossible even when every selected mode is legal on its own.
pub fn modal_choice_with_target_assignment_limit(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    modal: &ModalChoice,
    mode_abilities: &[AbilityDefinition],
    unavailable_modes: &[usize],
) -> Option<ModalChoice> {
    let target_constraints = target_constraints_from_modal(modal);
    if target_constraints.is_empty() || !modal.mode_pawprints.is_empty() {
        return Some(modal.clone());
    }

    let max_legal_choices = generate_modal_index_sequences(modal)
        .into_iter()
        .filter(|indices| indices.iter().all(|idx| !unavailable_modes.contains(idx)))
        .filter(|indices| {
            modal_indices_have_legal_target_assignment(
                state,
                source_id,
                controller,
                mode_abilities,
                indices,
                &target_constraints,
            )
        })
        .map(|indices| indices.len())
        .max()?;

    let mut effective = modal.clone();
    effective.max_choices = effective.max_choices.min(max_legal_choices);
    Some(effective)
}

fn modal_indices_have_legal_target_assignment(
    state: &GameState,
    source_id: ObjectId,
    controller: PlayerId,
    mode_abilities: &[AbilityDefinition],
    indices: &[usize],
    target_constraints: &[TargetSelectionConstraint],
) -> bool {
    let Ok(resolved) = build_chained_resolved(mode_abilities, indices, source_id, controller)
    else {
        return false;
    };
    match build_target_slots(state, &resolved) {
        Ok(slots) if slots.is_empty() => true,
        Ok(slots) => {
            has_legal_target_assignment_for_ability(state, &resolved, &slots, target_constraints)
        }
        Err(_) => false,
    }
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
/// distinct objects or players; slots with DIFFERENT ids are separate instances
/// that may reuse the same object or player ("Destroy target artifact and
/// target land").
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

    let next_progress =
        build_target_selection_progress(target_slots, constraints, next_slot, selected_slots)?;
    if next_progress.current_slot >= target_slots.len() {
        validate_selected_slot_prefix(target_slots, &next_progress.selected_slots, constraints)?;
        return Ok(TargetSelectionAdvance::Complete(
            next_progress.selected_slots,
        ));
    }
    Ok(TargetSelectionAdvance::InProgress(next_progress))
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
    let skipped_current = target.is_none();
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

    let specs = target_slot_specs(state, ability);
    let mut next_slot = progress.current_slot + 1;
    // CR 601.2c: A variable "up to N target ..." phrase announces one target
    // count for a single target instance. Once the controller declines the next
    // optional slot in that same instance, they have announced no more targets
    // for the phrase; do not force one Skip click per remaining possible slot.
    if skipped_current {
        if let Some(skipped_instance) = specs.get(progress.current_slot).map(|spec| spec.instance) {
            while next_slot < target_slots.len()
                && target_slots[next_slot].optional
                && specs
                    .get(next_slot)
                    .is_some_and(|spec| spec.instance == skipped_instance)
            {
                selected_slots.push(None);
                next_slot += 1;
            }
        }
    }

    if next_slot == target_slots.len() {
        validate_selected_slots_with_specs(
            state,
            ability,
            &specs,
            target_slots,
            &selected_slots,
            constraints,
        )?;
        return Ok(TargetSelectionAdvance::Complete(selected_slots));
    }

    let next_progress = build_target_selection_progress_for_ability(
        state,
        ability,
        target_slots,
        constraints,
        next_slot,
        selected_slots,
    )?;
    if next_progress.current_slot >= target_slots.len() {
        validate_selected_slots_with_specs(
            state,
            ability,
            &specs,
            target_slots,
            &next_progress.selected_slots,
            constraints,
        )?;
        return Ok(TargetSelectionAdvance::Complete(
            next_progress.selected_slots,
        ));
    }
    Ok(TargetSelectionAdvance::InProgress(next_progress))
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
    // CR 601.2c + CR 115.1: if any slot is announced by a player other than the
    // controller ("of an opponent's choice"), the choice is not the controller's
    // to auto-resolve even when only one legal combination exists — force the
    // interactive per-slot walk so each announcer declares their own slot. Mirrors
    // the `TargetSelectionMode::Random` guard, which likewise bypasses auto-select.
    if target_slots.iter().any(|slot| slot.chooser.is_some()) {
        return Ok(None);
    }
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

pub fn simple_legal_target_assignment_exists_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    constraints: &[TargetSelectionConstraint],
) -> Option<bool> {
    if !constraints.is_empty() {
        return None;
    }

    let specs = target_slot_specs(state, ability);
    let [spec] = specs.as_slice() else {
        return None;
    };
    if spec.optional {
        return Some(true);
    }
    if target_filter_contains_chosen_x_ref(&spec.filter)
        || relative_controller_kind(&spec.filter).is_some()
        || target_filter_has_another_target_marker(&spec.filter)
        || is_per_opponent_target_fanout(ability)
        || matches!(ability.effect, Effect::PairWith { .. })
        || damage_any_target_legal_targets(state, ability, &spec.filter).is_some()
    {
        return None;
    }

    Some(targeting::has_legal_target_for_ability(
        state,
        &spec.filter,
        ability,
    ))
}

/// CR 603.3d: could `execute` — a trigger's ability, resolving from `source` —
/// either need no target at all, or find a legal target right now? A
/// mandatory-target trigger with no legal choice is removed from the stack
/// rather than producing its effect, so a payoff-eligibility preflight must not
/// credit it.
///
/// Answers only from *confirmed* legality — never from an "unknown" shape. The
/// cheap single-slot check is tried first as a guard; every shape it cannot
/// decide (multi-slot, relative-controller, distribution, `PairWith`, …) falls
/// through to [`has_legal_target_assignment_for_ability`], the same full
/// legal-assignment authority the interactive target walk uses, so a
/// two-mandatory-target trigger with no legal assignment is correctly rejected.
/// A slot-building error leaves legality unproven and is likewise not credited.
pub fn execute_targets_satisfiable(
    state: &GameState,
    source: &crate::game::game_object::GameObject,
    execute: &AbilityDefinition,
) -> bool {
    // CR 603.3c: a MODAL execute carries a placeholder root and its targets in
    // `mode_abilities` (which the root slot walk does not descend). Mirror the
    // live trigger dispatch: filter each mode by its own target legality, then
    // require a legal modal choice — a required "choose one/two …" whose modes
    // are all target-unavailable is dropped (`DroppedNoLegalMode`), so it is not
    // a live payoff.
    if let Some(modal) = &execute.modal {
        let mut unavailable_modes = Vec::new();
        filter_modes_by_target_legality(
            state,
            source.id,
            source.controller,
            &execute.mode_abilities,
            modal,
            &mut unavailable_modes,
        );
        if unavailable_modes.len() >= modal.mode_count {
            return false; // CR 603.3c: no legal mode
        }
        // CR 603.3d: the required choose-count must be satisfiable with legal
        // target assignments across the surviving modes.
        return modal_choice_with_target_assignment_limit(
            state,
            source.id,
            source.controller,
            modal,
            &execute.mode_abilities,
            &unavailable_modes,
        )
        .is_some();
    }
    // CR 603.3d: build the ability the same way the live trigger pipeline does
    // (`build_resolved_from_def`) so a sub-ability chain's own target slots are
    // preflighted too — not just the root effect's.
    let resolved = build_resolved_from_def(execute, source.id, source.controller);
    if target_slot_specs(state, &resolved).is_empty() {
        return true; // the effect requires no target
    }
    // CR 115.1 + CR 601.2c: preflight against the SAME cross-target constraints
    // the live trigger carries (`PendingTrigger::target_constraints`), so a
    // constrained multi-target execute is not judged against a broader target
    // space than it will actually receive.
    let constraints = execute.target_constraints.as_slice();
    // Cheap guard: `Some(false)` = a mandatory target with no legal choice;
    // `Some(true)` = legal or optional; `None` = a shape this cheap check
    // cannot decide (incl. any constrained set), which the full authority
    // below resolves exactly.
    if let Some(decided) =
        simple_legal_target_assignment_exists_for_ability(state, &resolved, constraints)
    {
        return decided;
    }
    build_target_slots(state, &resolved).is_ok_and(|slots| {
        has_legal_target_assignment_for_ability(state, &resolved, &slots, constraints)
    })
}

/// True when `def`'s entire ability tree is engine-supported — no
/// `Effect::Unimplemented` gap node at the root or in any nested sub-ability,
/// else-branch, or mode. The live trigger builder converts a `None` execute /
/// unsupported effect into an `Effect::Unimplemented` (`TriggerNoExecute`) no-op
/// that produces no payoff, so payoff eligibility (both the live fireability
/// preflight and the deck-feature classifier) must not credit such a trigger.
/// The single shared support authority both consult.
pub fn ability_definition_supported(def: &AbilityDefinition) -> bool {
    // CR 700.2: a modal ability carries a placeholder `Effect::Unimplemented`
    // (`modal_placeholder`) root — its real effects live in `mode_abilities`, so
    // the placeholder is NOT a gap. Only an `Unimplemented` root on a
    // non-modal ability is a true unsupported node.
    if matches!(*def.effect, Effect::Unimplemented { .. }) && def.modal.is_none() {
        return false;
    }
    if def
        .sub_ability
        .as_deref()
        .is_some_and(|sub| !ability_definition_supported(sub))
    {
        return false;
    }
    if def
        .else_ability
        .as_deref()
        .is_some_and(|els| !ability_definition_supported(els))
    {
        return false;
    }
    def.mode_abilities.iter().all(ability_definition_supported)
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

/// CR 700.2b (override) + CR 701.9b (analogous): Resolve a modal ability whose
/// `selection` is `Random` (Cult of Skaro "choose one at random") by uniformly
/// drawing mode index/indices from the legal set using the engine's seeded RNG
/// (`state.rng`). The game — not `modal.chooser` — makes the selection, so no
/// `WaitingFor::AbilityModeChoice` is emitted. Mirrors
/// `random_select_targets_for_ability` for the mode-selection axis.
///
/// The legal set is `0..mode_count` minus `unavailable_modes` (modes ruled out
/// by prior selection or unsatisfiable target legality, per CR 700.2b). A count
/// is first drawn uniformly from `min_choices..=max_choices` (capped to the
/// legal-set size), then that many distinct indices are drawn without
/// replacement unless `allow_repeat_modes` permits repeats (CR 700.2d).
///
/// Determinism: uses `state.rng` (`ChaCha20Rng`, seeded per game), preserving
/// replay/test reproducibility.
///
/// Returns `None` when no mode can legally be chosen (CR 603.3c: the ability is
/// removed from the stack); callers handle that the same way the all-modes-
/// unavailable branch does.
pub fn random_select_modal_indices(
    state: &mut GameState,
    modal: &ModalChoice,
    unavailable_modes: &[usize],
) -> Option<Vec<usize>> {
    use rand::seq::{IndexedRandom, SliceRandom}; // rand 0.9
    use rand::Rng; // random_bool for the "up to" stop coin flip

    let legal: Vec<usize> = (0..modal.mode_count)
        .filter(|idx| !unavailable_modes.contains(idx))
        .collect();
    if legal.is_empty() {
        // CR 603.3c: No legal mode — the ability is removed from the stack.
        return None;
    }

    if !modal.mode_pawprints.is_empty() {
        // CR 700.2i + CR 700.2b: random selection of a pawprint points-budget
        // modal respects the budget (`max_choices` is the point budget here, not
        // a mode count). Draw incrementally among modes that still fit, stopping
        // once `min_choices` is met and an "up to" coin flip lands, or when no
        // legal mode fits the remaining budget. No in-corpus card uses random
        // selection of a pawprint modal, so the exact stop-distribution is
        // unspecified by the CR; the only invariant the rules pin down is that
        // the result must be budget-legal (asserted below).
        let budget = modal.max_choices as u32;
        let mut spent = 0u32;
        let mut indices: Vec<usize> = Vec::new();
        loop {
            let affordable: Vec<usize> = legal
                .iter()
                .copied()
                .filter(|&i| spent + u32::from(modal.mode_pawprints[i]) <= budget)
                .filter(|&i| modal.allow_repeat_modes || !indices.contains(&i))
                .collect();
            if affordable.is_empty() {
                break;
            }
            let pick = *affordable.choose(&mut state.rng)?;
            spent += u32::from(modal.mode_pawprints[pick]);
            indices.push(pick);
            // "up to" — once the minimum is met, randomly decide to stop.
            if indices.len() >= modal.min_choices && state.rng.random_bool(0.5) {
                break;
            }
        }
        debug_assert!(pawprint_budget_satisfied(modal, &indices));
        return Some(indices);
    }

    // CR 700.2d: Without repeats the chosen count cannot exceed the legal-set
    // size; with repeats the same mode may be drawn up to `max_choices` times.
    let max = if modal.allow_repeat_modes {
        modal.max_choices
    } else {
        modal.max_choices.min(legal.len())
    };
    let min = modal.min_choices.min(max);
    if max == 0 {
        // "Choose up to one ... at random" with no legal mode to pick resolves
        // with no instructions (CR 700.2a) — represented by an empty index set.
        return Some(Vec::new());
    }

    let count = if min == max {
        min
    } else {
        // Uniform over the inclusive count range.
        (min..=max)
            .collect::<Vec<_>>()
            .choose(&mut state.rng)
            .copied()
            .unwrap_or(min)
    };

    let mut indices = Vec::with_capacity(count);
    if modal.allow_repeat_modes {
        for _ in 0..count {
            indices.push(*legal.choose(&mut state.rng)?);
        }
    } else {
        let mut pool = legal;
        pool.shuffle(&mut state.rng);
        indices.extend(pool.into_iter().take(count));
    }
    Some(indices)
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
        ability.capture_target_incarnations_recursive(state);
        return Ok(());
    }
    if !chain_has_target_sink(ability) {
        ability.targets = targets.to_vec();
        ability.capture_target_incarnations_recursive(state);
        return Ok(());
    }
    let mut next_target = 0usize;
    assign_targets_recursive(state, ability, targets, &mut next_target)?;
    if next_target != targets.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected targets".to_string(),
        ));
    }
    ability.capture_target_incarnations_recursive(state);
    Ok(())
}

pub fn assign_selected_slots_in_chain(
    state: &GameState,
    ability: &mut ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
) -> Result<(), EngineError> {
    if is_per_opponent_target_fanout(ability) {
        ability.targets = selected_slots.iter().flatten().cloned().collect();
        ability.capture_target_incarnations_recursive(state);
        return Ok(());
    }
    if !chain_has_target_sink(ability) {
        ability.targets = selected_slots.iter().flatten().cloned().collect();
        ability.capture_target_incarnations_recursive(state);
        return Ok(());
    }
    let mut next_slot = 0usize;
    assign_selected_slots_recursive(state, ability, selected_slots, &mut next_slot)?;
    if next_slot != selected_slots.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected target slots".to_string(),
        ));
    }
    ability.capture_target_incarnations_recursive(state);
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

/// CR 601.2d: The node whose effect divides damage/counters among its own
/// targets. Mirrors `extract_distribution_total`, which inspects only the
/// top-level `ability.effect`; a divided effect only reaches the
/// `WaitingFor::DistributeAmong` sites when it is the top-level node.
fn distributing_node(ability: &ResolvedAbility) -> Option<&ResolvedAbility> {
    matches!(
        ability.effect,
        Effect::DealDamage { .. } | Effect::PutCounter { .. }
    )
    .then_some(ability)
}

/// CR 601.2d: The targets a division is distributed among — the distributing
/// node's OWN targets only, excluding sibling-effect targets elsewhere in the
/// chain (e.g. a chained "tap two target permanents"). Per-opponent fanout
/// strips player refs (mirroring `flatten_targets_in_chain`); ordinary
/// player-targeted divided damage keeps its player targets.
pub fn distribution_targets(ability: &ResolvedAbility) -> Vec<TargetRef> {
    let Some(node) = distributing_node(ability) else {
        return Vec::new();
    };
    if is_per_opponent_target_fanout(node) {
        object_targets_only(&node.targets)
    } else {
        node.targets.clone()
    }
}

/// CR 608.2b: Re-validate targets on resolution — remove any that are no longer legal.
fn target_is_current(ability: &ResolvedAbility, target: &TargetRef, state: &GameState) -> bool {
    match target {
        TargetRef::Object(id) => {
            ability.target_pin_is_current(*id, state)
                && ability.selected_target_pin_is_current(*id, state)
        }
        TargetRef::Player(_) => true,
    }
}

fn validate_pinned_targets(
    state: &GameState,
    targets: &[TargetRef],
    filter: &TargetFilter,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    targeting::validate_targets_for_ability(state, targets, filter, ability)
        .into_iter()
        .filter(|target| target_is_current(ability, target, state))
        .collect()
}

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
                let legal = validate_pinned_targets(
                    state,
                    std::slice::from_ref(target_ref),
                    filter,
                    &validated,
                );
                legal.into_iter().next()
            })
            .collect()
    } else if let Effect::Attach { attachment, target } = &validated.effect {
        // CR 608.2b (phase#4767 review): `attachment`/`target` context-refs
        // (SelfRef, ParentTarget, ...) don't need their own target slot and
        // are skipped below — but `validated.targets` can carry MORE entries
        // than this Attach node's own two operands consume, propagated
        // through for a downstream sibling in the chain (e.g. a
        // CreateDelayedTrigger sub-ability reading the same ParentTarget).
        // Only the entries this node's own filters actually claim get
        // re-validated here; any remaining, un-claimed entries must pass
        // through UNCHANGED rather than being silently dropped, or a
        // sibling relying on them downstream loses its target.
        let mut kept = Vec::new();
        let mut target_iter = validated.targets.iter();
        for (is_attachment, filter) in [(true, attachment), (false, target)] {
            if !attach_side_needs_target_slot(filter, is_attachment) {
                continue;
            }
            let Some(target_ref) = target_iter.next() else {
                continue;
            };
            if let Some(legal) =
                validate_pinned_targets(state, std::slice::from_ref(target_ref), filter, &validated)
                    .into_iter()
                    .next()
            {
                kept.push(legal);
            }
        }
        kept.extend(target_iter.cloned());
        kept
    } else if let Some(role) = mana_multi_role(&validated.effect) {
        // CR 608.2b: THREE properties, all required.
        // (1) "Illegal targets won't be affected by parts of the effect for
        //     which they're illegal" — per-role, enforced at the consumption
        //     site by `ability_scoped_to_slot`, which re-validates each role
        //     against that role's OWN filter. So surviving targets must keep
        //     their POSITIONS: pruning would slide the count source into index
        //     0 and it would be read as the recipient, re-creating the
        //     collision this change removes. That is why the claimed positions
        //     do NOT use `Attach`'s `kept.push(legal)` shape.
        // (2) "If ALL its targets, for every instance of the word 'target', are
        //     now illegal, THE SPELL OR ABILITY doesn't resolve" — the subject
        //     is the whole ability, and `check_fizzle` is correspondingly
        //     chain-wide over `flatten_targets_in_chain`. So when every one of
        //     THIS node's roles is illegal we emit NOTHING for this node's own
        //     claimed positions — which fizzles the ability iff this node is
        //     the chain's only target sink, and correctly does NOT fizzle it
        //     when a sibling still holds a legal target.
        // (3) Same hazard `Attach` above documents — `validated.targets` may
        //     carry entries this node's filters never claimed, propagated for a
        //     downstream sibling. They pass through UNCHANGED in BOTH branches.
        //
        // GATED ON `mana_multi_role`, matching every other role-slot site
        // (collect, specs, both assigns, the reservation terms, the sink check).
        // A single-role mana MUST keep the generic branch below, for two
        // reasons:
        //   - It already receives clause (2) there: the
        //     `Some(filter) => validate_targets_for_ability(..)` arm prunes an
        //     illegal sole target to empty and the chain fizzles. Broadening
        //     this arm adds no CR 608.2b coverage it did not already have.
        //   - The generic branch is COMPANION-AWARE and this one is not.
        //     `ability_needs_companion_target_player_slot` fires on `unless_pay`
        //     COST shape independently of the effect, and the companion slot is
        //     pushed BEFORE the role slot — so `targets == [companion, role]`.
        //     Zipping `surfaced_filters()[0]` (the role filter) against
        //     `targets[0]` (the companion) would fail the role filter, clear
        //     `any_legal`, and discard a perfectly LEGAL companion target. The
        //     generic branch handles that layout correctly via `split_first`.
        // For a multi-role mana the companion slots are gated off everywhere, so
        // `targets` starts with this node's own role positions and the zip is
        // sound.
        let mut claimed: Vec<TargetRef> = Vec::new();
        let mut any_legal = false;
        let mut target_iter = validated.targets.iter();
        for (_slot, filter) in role.surfaced_filters() {
            let Some(target_ref) = target_iter.next() else {
                break;
            };
            if !validate_pinned_targets(state, std::slice::from_ref(target_ref), filter, &validated)
                .is_empty()
            {
                any_legal = true;
            }
            // Position-stable regardless of legality.
            claimed.push(target_ref.clone());
        }
        let mut kept = if any_legal { claimed } else { Vec::new() };
        kept.extend(target_iter.cloned());
        kept
    } else if let Effect::Fight { subject, target } = &validated.effect {
        // CR 608.2b + CR 701.14a: Dual-fighter fights validate each chosen
        // fighter against its own slot filter so one illegal fighter does not
        // collapse into the single-target "~ fights" fallback shape.
        if fight_subject_needs_target_slot(subject) {
            let filters = vec![subject, target];
            let mut kept = Vec::new();
            let mut target_iter = validated.targets.iter();
            for filter in filters {
                if matches!(filter, TargetFilter::SelfRef | TargetFilter::ParentTarget) {
                    continue;
                }
                let Some(target_ref) = target_iter.next() else {
                    continue;
                };
                if let Some(legal) = validate_pinned_targets(
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
        } else {
            // CR 701.14a + CR 608.2b: "~ fights" / anaphoric "it fights" / chained
            // "that creature … and fights" — the ally fighter is implicit. Propagated
            // targets are ordered [ally, opponent], but only the opponent must satisfy
            // this effect's `target` filter; pairing targets[0] against that filter
            // wrongly drops the ally (Ent's Fury, issue #1135). Nested chain links keep
            // chosen targets on the resolving spell, not on the fight sub-clause itself.
            let candidate_targets = state
                .resolving_stack_entry
                .as_ref()
                .and_then(|entry| entry.ability())
                .map(flatten_targets_in_chain)
                .filter(|targets| !targets.is_empty())
                .unwrap_or_else(|| validated.targets.clone());

            fn fight_creature_on_battlefield(
                state: &GameState,
                id: crate::types::identifiers::ObjectId,
            ) -> bool {
                state.objects.get(&id).is_some_and(|obj| {
                    obj.zone == crate::types::zones::Zone::Battlefield
                        && obj.is_phased_in()
                        && obj
                            .card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
            }

            let explicit: Vec<TargetRef> = candidate_targets
                .iter()
                .filter(|t| {
                    validate_pinned_targets(state, std::slice::from_ref(t), target, &validated)
                        .into_iter()
                        .next()
                        .is_some()
                })
                .cloned()
                .collect();

            let mut kept = Vec::new();
            if explicit.len() == 1 {
                if let Some(ally) = candidate_targets.iter().find(|t| {
                    let TargetRef::Object(id) = t else {
                        return false;
                    };
                    !explicit.contains(t)
                        && fight_creature_on_battlefield(state, *id)
                        && target_is_current(&validated, t, state)
                }) {
                    kept.push(ally.clone());
                }
            }
            kept.extend(explicit);
            kept
        }
    } else if let Some(src_leaf) = prevent_damage_source_slot_filter(&validated.effect).cloned() {
        // CR 608.2b + CR 609.7a: A source-scoped `PreventDamage` carries its
        // chosen source spell in `targets[0]`. `extract_target_filter_from_effect`
        // returns `None` for its `Any` recipient, so the generic `None` arm below
        // would fizzle-filter the spell to battlefield presence and drop it
        // (the spell lives on the STACK). Re-validate against the source leaf
        // (`InZone Stack`-aware) instead, preserving the spell target.
        validate_pinned_targets(state, &validated.targets, &src_leaf, &validated)
    } else if matches!(
        &validated.effect,
        Effect::ChangeZoneAll { target, .. } if crate::game::effects::filter_refs_parent_target(target)
    ) {
        // CR 115.1 + CR 608.2b: `ChangeZoneAll` is a resolution-time mass
        // instruction when its filter carries a delayed `ParentTarget` snapshot
        // inside a tracked set. Treating that internal filter as a target would
        // fizzle a valid Exile -> Battlefield return before the mass resolver
        // can inspect the tracked member. Ordinary ChangeZoneAll player filters
        // remain declared targets and follow the generic validation below.
        validated.targets.clone()
    } else if matches!(
        mass_all_target_filter(&validated.effect),
        Some(TargetFilter::Player)
    ) {
        // CR 115.1 + CR 608.2b: A bare `Player` mass-operation filter (such as
        // "exile target player's graveyard") is represented by a companion
        // declared-player slot. The mass filter is normally a resolution-time
        // population scan, so it has no `target_filter()` entry; validate this
        // exceptional declared target against the same legal-player set used
        // to build the slot.
        validate_pinned_targets(state, &validated.targets, &TargetFilter::Player, &validated)
    } else {
        match triggers::extract_target_filter_from_effect(&validated.effect) {
            Some(filter) if matches!(validated.effect, Effect::PairWith { .. }) => {
                let legal_choices = pair_with_legal_choices(state, &validated, filter);
                validated
                    .targets
                    .iter()
                    .filter(|target| {
                        legal_choices.contains(target)
                            && target_is_current(&validated, target, state)
                    })
                    .cloned()
                    .collect()
            }
            Some(filter) if ability_needs_companion_target_player_slot(&validated) => {
                let mut kept = Vec::new();
                let primary_targets = match validated.targets.split_first() {
                    Some((companion, rest))
                        if companion_target_player_legal_targets(state, &validated)
                            .contains(companion) =>
                    {
                        kept.push(companion.clone());
                        rest
                    }
                    Some((_, rest)) => rest,
                    None => &[],
                };
                if let Some(companion) = kept.first() {
                    if !target_is_current(&validated, companion, state) {
                        kept.clear();
                    }
                }
                kept.extend(validate_pinned_targets(
                    state,
                    primary_targets,
                    filter,
                    &validated,
                ));
                kept
            }
            Some(filter) => validate_pinned_targets(state, &validated.targets, filter, &validated),
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
            // CR 303.4a + CR 608.2b: A plain Aura spell has no separate on-cast
            // effect — its resolving `Effect` is the `Effect::Unimplemented`
            // placeholder built in `casting.rs`, so `extract_target_filter_from_effect`
            // returns `None` and lands here. Its legal targets are defined by its
            // enchant ability (`Keyword::Enchant`), NOT by the placeholder effect,
            // and that filter may be zone-scoped (e.g. Animate Dead's "creature
            // card in a graveyard"). Re-validate against the Enchant filter with
            // the SAME machinery cast-time targeting uses, so a graveyard-zone host
            // is not fizzle-filtered by the hardcoded battlefield check below.
            // Gated on `Effect::Unimplemented` specifically (not on Aura-ness): the
            // `None` arm also legitimately serves `Effect::Sacrifice`/`UnattachAll`/
            // `Bounce { selection: AtResolution }`, which must keep the plain
            // battlefield fizzle-check.
            None if matches!(validated.effect, Effect::Unimplemented { .. }) => {
                match crate::game::effects::change_targets::aura_enchant_filter(
                    state,
                    validated.source_id,
                ) {
                    Some(filter) => {
                        validate_pinned_targets(state, &validated.targets, &filter, &validated)
                    }
                    None => validated
                        .targets
                        .iter()
                        .filter(|target| match target {
                            TargetRef::Object(object_id) => {
                                state.battlefield.contains(object_id)
                                    && target_is_current(&validated, target, state)
                            }
                            TargetRef::Player(_) => true,
                        })
                        .cloned()
                        .collect(),
                }
            }
            None => validated
                .targets
                .iter()
                .filter(|target| match target {
                    TargetRef::Object(object_id) => {
                        state.battlefield.contains(object_id)
                            && target_is_current(&validated, target, state)
                    }
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

/// CR 701.14a: True when a fight's `subject` filter must surface its own target
/// slot ("target creature you control fights another target creature"). False
/// for "~ fights", ParentTarget anaphors, and enchanted/equipped hosts.
pub(crate) fn fight_subject_needs_target_slot(subject: &TargetFilter) -> bool {
    use crate::types::ability::FilterProp;
    if subject.is_context_ref() {
        return false;
    }
    match subject {
        TargetFilter::SelfRef | TargetFilter::ParentTarget | TargetFilter::AttachedTo => false,
        TargetFilter::Typed(tf)
            if tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::EnchantedBy | FilterProp::EquippedBy)) =>
        {
            false
        }
        _ => true,
    }
}

/// Legal targets for the companion `TargetFilter::Player` slot — the player
/// whose permanents a `ControllerRef::TargetPlayer` ("that player controls")
/// filter scopes to. Single authority shared by the static slot build
/// (`collect_target_slots`) and the dynamic selection-time recompute
/// (`legal_targets_for_selected_slot`); the two MUST agree or selection-time
/// recomputation would re-offer every player and reintroduce the hang.
///
/// For a damage-to-player trigger the slot is bound to the damaged player(s) of
/// the triggering event (CR 120.3a). Gated on `trigger_source` (carried only
/// by triggered abilities) so a stale event batch never constrains a spell's
/// genuine free-choice "target player". Otherwise every legal player is offered.
fn companion_target_player_legal_targets(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    // CR 115.1 + CR 118.12a: a payer declared as a target inside the unless clause
    // ("unless target opponent/target player pays") drives this slot directly — the
    // payer's own filter (opponent-only vs all players) determines who is legal,
    // taking precedence over the damage-to-player constraint (the unless clause has
    // its own declared target, independent of any triggering damage event).
    if let Some(payer) = ability
        .unless_pay
        .as_ref()
        .map(|m| &m.payer)
        .filter(|&payer| payer_is_declared_target(payer))
    {
        return targeting::find_legal_targets(state, payer, ability.controller, ability.source_id);
    }
    ability
        .trigger_source
        .as_ref()
        .and_then(|_| damaged_player_targets_for_companion_slot(state))
        .unwrap_or_else(|| {
            // CR 109.4 + CR 102.2 / CR 102.3: "target opponent controls" offers only
            // opponents (self excluded; any one opponent in >2p). Reuses the
            // Typed{controller:Opponent} legality path bare "target opponent" uses
            // (targeting.rs → players::is_opponent). Plain "target player" → any player.
            let slot_filter = if effect_references_target_opponent(&ability.effect) {
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
            } else {
                TargetFilter::Player
            };
            targeting::find_legal_targets(
                state,
                &slot_filter,
                ability.controller,
                ability.source_id,
            )
        })
}

/// CR 115.7 + CR 109.4: Legal replacement *players* for retargeting a stack
/// entry whose only target is a player derived from a mass-effect population
/// filter — e.g. "tap all creatures target player controls"
/// (`SetTapState { scope: All }`), "destroy all artifacts that player controls"
/// (`DestroyAll`). Such effects surface a player target slot via
/// `effect_references_target_player`, but their `Effect::target_filter()`
/// returns `None` (the `target` field is a resolution-time population scan, not
/// a targeting filter). Returns `Some(legal players)` for that class so
/// Deflecting Swat / Bolt Bend / Redirect can offer a different player, and
/// `None` otherwise (the caller falls back to the effect's declared target
/// filter). Reuses the same companion-slot authority the cast path uses so
/// retargeting and casting can never disagree about who is targetable.
pub(crate) fn companion_target_player_retarget_options(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<Vec<TargetRef>> {
    ability_needs_companion_target_player_slot(ability)
        .then(|| companion_target_player_legal_targets(state, ability))
}

/// CR 601.2c + CR 115.1: Collect the target slots contributed by `ability` (and
/// its chained sub-abilities), stamping each slot's announcing player from the
/// link's own `target_chooser`. The chooser is scoped per link: it is set before
/// this link's slots are collected and restored afterwards, so a downstream link
/// with `target_chooser == None` does not inherit an upstream link's opponent
/// announcer (and vice versa). This per-link scoping is what gives Volcanic
/// Offering its `[None, Some(opp), None, Some(opp)]` chooser vector.
fn collect_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    acc: &mut SlotAccumulator,
) -> Result<(), TargetSlotBuildError> {
    let resolved_chooser = ability.target_chooser.as_ref().and_then(|filter| {
        crate::game::targeting::resolve_effect_player_ref(state, ability, filter)
            // A chooser equal to the controller is the CR-601.2c default; leave the
            // slot's `chooser` as None so default routing and serde-omission apply.
            .filter(|&player| player != ability.controller)
    });
    let previous_chooser = std::mem::replace(&mut acc.current_chooser, resolved_chooser);
    // CR 115.1: stamp this link's own effect on the slots it is about to push.
    // Restored below so a chained sub-ability does not leak its kind upward.
    let previous_effect_kind = std::mem::replace(
        &mut acc.current_effect_kind,
        EffectKind::from(&ability.effect),
    );
    let previous_effect_detail = std::mem::replace(
        &mut acc.current_effect_detail,
        target_effect_detail(&ability.effect),
    );
    let result = collect_target_slots_inner(state, ability, acc);
    acc.current_chooser = previous_chooser;
    acc.current_effect_kind = previous_effect_kind;
    acc.current_effect_detail = previous_effect_detail;
    result
}

/// CR 115.1: Read the fact `EffectKind` cannot carry off the effect's own
/// payload, at the one point where the payload is in hand.
///
/// Only the kinds whose unit tag is genuinely ambiguous about what happens to
/// the target are covered; everything else is [`TargetEffectDetail::None`],
/// which is also the honest answer whenever the deciding value is not
/// statically known. This runs at construction (not projection) because
/// `WaitingFor::TriggerTargetSelection` carries no ability reference, so a
/// projection-time read would resolve spell targeting and not trigger
/// targeting — labelling the same effect differently depending on how it
/// reached the stack. Construction is symmetric: both `build_target_slots` and
/// `build_target_slots_labelled` route through `collect_target_slots`.
fn target_effect_detail(effect: &Effect) -> TargetEffectDetail {
    match effect {
        // The zone family's tag says "a zone change happened", never which
        // zone. Exile and return-to-hand share `EffectKind::ChangeZone`.
        Effect::ChangeZone { destination, .. } | Effect::ChangeZoneAll { destination, .. } => {
            TargetEffectDetail::Destination(*destination)
        }
        // CR 613.4: `Effect::Pump` is one kind for "+3/+3" and "-3/-3".
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => pt_direction(power, toughness)
            .map_or(TargetEffectDetail::None, TargetEffectDetail::Modification),
        _ => TargetEffectDetail::None,
    }
}

/// CR 613.4: Direction of a P/T modification, or `None` when no single
/// direction is true.
///
/// `None` covers two real populations rather than being a catch-all: a dynamic
/// magnitude (X or count-based, where the sign is not knowable at announcement
/// — CR 601.2c fixes targets before X is locked for many cards) and a genuinely
/// opposing modification such as "+2/-2", which is neither a buff nor a debuff.
/// A one-sided change like "-4/-0" IS directional and resolves.
fn pt_direction(power: &PtValue, toughness: &PtValue) -> Option<PtDirection> {
    let fixed = |value: &PtValue| match value {
        PtValue::Fixed(amount) => Some(*amount),
        PtValue::Variable(_) | PtValue::Quantity(_) => None,
    };
    let (power, toughness) = (fixed(power)?, fixed(toughness)?);
    match (power.signum(), toughness.signum()) {
        (1, 0 | 1) | (0, 1) => Some(PtDirection::Increase),
        (-1, 0 | -1) | (0, -1) => Some(PtDirection::Decrease),
        // Both zero (no change) or opposing signs: no direction is true.
        _ => None,
    }
}

fn collect_target_slots_inner(
    state: &GameState,
    ability: &ResolvedAbility,
    acc: &mut SlotAccumulator,
) -> Result<(), TargetSlotBuildError> {
    if let Some(sub_ability) = ability.sub_ability.as_deref().filter(|sub| {
        matches!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    }) {
        // CR 601.2b/c + CR 702.194c: the broad "instead" override is surfaced
        // here only when `additional_cost_paid` is set at slot-build time.
        // RESOLVED (finding #1): cast-time propagation of that flag used to be
        // gated on Kicker alone; the pre-target deferral gates in `casting.rs`
        // and `additional_cost_instead_spell_has_legal_targets` now propagate
        // it for every `AdditionalCost`-"instead" card with a non-empty
        // effective queue (Teamwork/Bargain today), not just Kicker. This
        // function's own logic (reading `additional_cost_paid` variant-
        // agnostically) was already correct and needed no change.
        if ability.context.additional_cost_paid {
            collect_target_slots(state, sub_ability, acc)?;
            return Ok(());
        }
    }

    if target_slot_construction_needs_chosen_x_at_announcement(state, ability) {
        return Err(TargetSlotBuildError::RequiresChosenX);
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
        return Ok(());
    }

    // CR 701.14a + CR 115.1: "Target creature you control fights another target
    // creature" names two chosen fighters. "~ fights …" and "enchanted creature
    // fights …" only surface the opponent as a target slot — the fighter is the
    // ability source or the host permanent.
    if let Effect::Fight { subject, target } = &ability.effect {
        let mut filters: Vec<&TargetFilter> = Vec::new();
        if fight_subject_needs_target_slot(subject) {
            filters.push(subject);
        }
        filters.push(target);
        for filter in filters {
            // CR 608.2c + CR 701.14a: A context-ref fighter (SelfRef, ParentTarget,
            // ParentTargetSlot, TrackedSet — the reciprocal "those creatures fight
            // each other") resolves from chain context, never a cast-time choice,
            // so it surfaces no target slot. Broadened from the narrow
            // SelfRef|ParentTarget check: the reciprocal-fight lowering re-keys the
            // target to a TrackedSet, which is equally a context ref — generating a
            // slot for it produced a spurious all-players slot that panicked the
            // cast (Malamet Battle Glyph).
            if filter.is_context_ref() {
                continue;
            }
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
    } else if let Some(role) = mana_multi_role(&ability.effect) {
        // CR 601.2c + CR 115.1: A mana sentence may name a recipient AND a count
        // source as two separate instances of "target"; each is announced
        // independently. Context-ref recipients surface no slot. Declaration
        // order: recipient, then count source. GATED ON `mana_multi_role` —
        // single-role manas (every printed card) fall through to the generic
        // branch below via `Effect::target_filter`, unchanged. Heads into the
        // existing else-if group so the shared sub-ability descent still runs
        // (Jetfire: Mana → Convert) — do NOT early-return like the
        // ExchangeLifeTotals/Fight arms. Keep in lockstep with
        // `collect_target_slot_specs`: NO assertion links the two, so divergence
        // fails silently as misaligned TargetInstanceIds at runtime.
        for (_slot, filter) in role.surfaced_filters() {
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
    } else if let Effect::Attach { attachment, target } = &ability.effect {
        collect_attach_attachment_target_slots(state, ability, attachment, acc)?;
        if attach_host_filter_needs_target_slot(target) {
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, target, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
    } else if let Effect::EachDealsDamageEqualToPower {
        sources,
        recipient,
        extra_source,
    } = &ability.effect
    {
        // CR 115.1d + CR 115.1: "Up to two target creatures you control each deal
        // damage equal to their power to target creature." `target_filter()`
        // returns None for this effect, so surface both axes here.
        //
        // ORDER IS LOAD-BEARING: the variable-count SOURCE slots are declared
        // first (Oracle text order), then the single mandatory RECIPIENT slot
        // last. The resolver (`deal_damage::resolve_each_deals_equal_to_power`)
        // reads `ability.targets` as `[source.., recipient]`, treating the final
        // object target as the recipient.
        if ability.target_choice_timing == TargetChoiceTiming::Stack {
            // CR 601.2c + CR 115.1d: the source count ("up to two" → 0..=2, or
            // "two" → exactly 2) lives in the ability's `multi_target` spec.
            let source_legal =
                legal_targets_for_ability_filter(state, ability, sources, &acc.slots);
            if let Some(spec) = ability.multi_target.as_ref() {
                let bounds = resolve_multi_target_bounds(state, ability, spec, source_legal.len())?;
                for slot_index in 0..bounds.max {
                    acc.push(TargetSelectionSlot {
                        legal_targets: source_legal.clone(),
                        optional: slot_index >= bounds.min,
                        chooser: None,
                        effect_kind: acc.current_effect_kind,
                        effect_detail: acc.current_effect_detail,
                    });
                }
            } else {
                // No spec means a single mandatory source (defensive — the parser
                // always attaches an "up to two"/"two" spec for this effect).
                if source_legal.is_empty() {
                    return Err(no_legal_target_slots());
                }
                acc.push(TargetSelectionSlot {
                    legal_targets: source_legal,
                    optional: false,
                    chooser: None,
                    effect_kind: acc.current_effect_kind,
                    effect_detail: acc.current_effect_detail,
                });
            }

            // CR 115.4 + CR 601.2c: group B — one optional slot, AFTER the
            // group-A sources and BEFORE the recipient. Its `FilterProp::Another`
            // enforces distinctness from every group-A pick at selection time
            // (see `legal_targets_for_selected_slot`). Kept before the recipient
            // push so the resolver's `[source.., recipient]` split treats a
            // chosen group-B creature as a source, not the recipient.
            if let Some(extra) = extra_source {
                let extra_legal =
                    legal_targets_for_ability_filter(state, ability, extra, &acc.slots);
                acc.push(TargetSelectionSlot {
                    legal_targets: extra_legal,
                    optional: true,
                    chooser: None,
                    effect_kind: acc.current_effect_kind,
                    effect_detail: acc.current_effect_detail,
                });
            }

            // CR 115.1: the recipient is exactly one mandatory target.
            let recipient_legal =
                legal_targets_for_ability_filter(state, ability, recipient, &acc.slots);
            if recipient_legal.is_empty() {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets: recipient_legal,
                optional: false,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
    } else {
        if is_per_opponent_target_fanout(ability) {
            collect_per_opponent_target_fanout_slots(state, ability, acc)?;
            if let Some(sub_ability) = ability.sub_ability.as_deref() {
                if !defers_conditional_target_selection(sub_ability)
                    && !sub_ability_inherits_parent_creature_target_only(ability, sub_ability)
                {
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
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets: player_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_target_creature_quantity_slot(&ability.effect)
            && !one_sided_fight_source_supplies_quantity_creature(&ability.effect)
        {
            let filter = effect_target_slot_filter(&ability.effect)
                .expect("slot filter present when gate true");
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, &filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
        if ability.target_choice_timing == TargetChoiceTiming::Stack
            && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        {
            let filter = parent_target_combat_relation_slot_filter();
            let legal_targets =
                legal_targets_for_ability_filter(state, ability, &filter, &acc.slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(no_legal_target_slots());
            }
            acc.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
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
                            chooser: None,
                            effect_kind: acc.current_effect_kind,
                            effect_detail: acc.current_effect_detail,
                        });
                    }
                } else {
                    if legal_targets.is_empty() && !ability.optional_targeting {
                        return Err(no_legal_target_slots());
                    }
                    acc.push(TargetSelectionSlot {
                        legal_targets,
                        optional: ability.optional_targeting,
                        chooser: None,
                        effect_kind: acc.current_effect_kind,
                        effect_detail: acc.current_effect_detail,
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
        if !defers_conditional_target_selection(sub_ability)
            && !sub_ability_inherits_parent_creature_target_only(ability, sub_ability)
        {
            collect_target_slots(state, sub_ability, acc)?;
        }
    }
    Ok(())
}

fn no_legal_target_slots() -> TargetSlotBuildError {
    EngineError::ActionNotAllowed("No legal targets available".to_string()).into()
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

pub(crate) fn multi_target_needs_quantity_choice(
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
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => exprs
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
    target_slot_construction_needs_chosen_x(ability)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_target_legality_needs_chosen_x_inner)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_target_legality_needs_chosen_x_inner)
}

/// CR 601.2b/c: The current chain link cannot determine either its target
/// filter or its target count until its announced X value is available.
///
/// This deliberately excludes sub-abilities. `collect_target_slots` traverses
/// links in declaration order, so an earlier missing mandatory target remains
/// an immediate illegal-target error instead of being masked by a later
/// X-dependent target instruction.
fn target_slot_construction_needs_chosen_x(ability: &ResolvedAbility) -> bool {
    ability.chosen_x.is_none()
        && (triggers::extract_target_filter_from_effect(&ability.effect)
            .is_some_and(|filter| target_filter_needs_chosen_x(ability, filter))
            || ability.multi_target.as_ref().is_some_and(|spec| {
                quantity_expr_has_unresolved_x(ability, &spec.min)
                    || spec
                        .max
                        .as_ref()
                        .is_some_and(|expr| quantity_expr_has_unresolved_x(ability, expr))
            }))
}

/// CR 107.3m + CR 603.3b: A triggered ability's target filter can refer to the
/// X paid for the spell that produced its source. That X is not `chosen_x` on
/// the trigger, but it is already bound on the trigger source before targets
/// are chosen; only defer target construction when neither authority exists.
fn target_slot_construction_needs_chosen_x_at_announcement(
    state: &GameState,
    ability: &ResolvedAbility,
) -> bool {
    target_slot_construction_needs_chosen_x(ability)
        && ability
            .trigger_source
            .as_ref()
            .and_then(|source| source.source_read(state).cost_x_paid())
            .is_none()
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

fn target_filter_contains_amassed_army_ref(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(filter_prop_contains_amassed_army_ref),
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            target_filter_contains_amassed_army_ref(filter)
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_contains_amassed_army_ref)
        }
        _ => false,
    }
}

fn filter_prop_contains_amassed_army_ref(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::Cmc { value, .. }
        | FilterProp::Counters { count: value, .. }
        | FilterProp::PtComparison { value, .. } => quantity_expr_contains_amassed_army_ref(value),
        FilterProp::CanEnchant { target } => target_filter_contains_amassed_army_ref(target),
        FilterProp::DifferentNameFrom { filter }
        | FilterProp::TargetsOnly { filter }
        | FilterProp::Targets { filter } => target_filter_contains_amassed_army_ref(filter),
        FilterProp::SharesQuality { reference, .. } => reference
            .as_deref()
            .is_some_and(target_filter_contains_amassed_army_ref),
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_contains_amassed_army_ref),
        FilterProp::Not { prop } => filter_prop_contains_amassed_army_ref(prop),
        _ => false,
    }
}

fn quantity_expr_contains_amassed_army_ref(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref {
            qty:
                QuantityRef::Power {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::Toughness {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::ObjectManaValue {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::ObjectColorCount {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::ObjectNameWordCount {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::ObjectTypelineComponentCount {
                    scope: ObjectScope::AmassedArmy,
                }
                | QuantityRef::ManaSymbolsInManaCost {
                    scope: ObjectScope::AmassedArmy,
                    ..
                },
        } => true,
        QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => false,
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_contains_amassed_army_ref(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_expr_contains_amassed_army_ref)
        }
        QuantityExpr::Difference { left, right } => {
            quantity_expr_contains_amassed_army_ref(left)
                || quantity_expr_contains_amassed_army_ref(right)
        }
    }
}

fn target_filter_needs_ability_context(filter: &TargetFilter) -> bool {
    target_filter_contains_chosen_x_ref(filter)
        || target_filter_contains_amassed_army_ref(filter)
        || target_filter_contains_scoped_player_ref(filter)
        || filter_needs_trigger_source(filter)
}

/// CR 508.5 + CR 508.5a + CR 603.3d: a filter whose evaluation asks
/// `combat::defending_player_cr508_5` for the attacked-player anaphor needs the
/// resolving ability's `trigger_source`, because that authority's binding rule is
/// `trigger_source.and_then(|_| detection.or(state.current_trigger_event))` — with
/// no `trigger_source` the binding is `DefenderBinding::None`, which skips the
/// attack-entry tiers entirely and leaves only `resolve_defending_player`. That
/// tail resolves a non-attacking source through `extract_source_from_event`,
/// whose `AttackersDeclared` arm is gated on `attacker_ids.len() == 1`, so ANY
/// declaration with two or more attackers yields `None`,
/// `filter::attacking_defender_matches`'s `is_some_and` is false for every
/// candidate, the target slot is empty, and CR 603.3d removes the triggered
/// ability from the stack ("If a choice is required when the triggered ability
/// goes on the stack but no legal choices can be made for it ... the ability is
/// simply removed from the stack").
///
/// Routing these filters to `find_legal_targets_for_ability`
/// (`FilterContext::from_ability`) supplies the `trigger_source` that
/// `set_trigger_source_recursive` already put on every instantiated triggered
/// ability, making SLOT-BUILD agree with the CR 608.2b re-validation door
/// (`targeting::validate_targets_for_ability`), which has always used
/// `from_ability`. The disagreement between those two doors is what made this
/// failure silent.
///
/// SCOPE — exactly the refs that consume `trigger_source`, and no others.
/// `filter::source_controller_ref_player` special-cases three refs:
/// `DefendingPlayer` (reads `trigger_source`), `SourceChosenPlayer` (reads the
/// source), and `EnchantedPlayer` (reads `source.attached_to`); everything else
/// routes to `controller_ref_player`. Only `DefendingPlayer` needs the ability
/// context, so only `DefendingPlayer` is matched here — leaving the existing
/// corpus producers of `Attacking { defender: Some(You | Opponent |
/// SourceChosenPlayer | EnchantedPlayer) }` on their existing door, unchanged.
///
/// `FilterProp::CombatRelation` is deliberately EXCLUDED: it is evaluated by
/// `filter::matches_combat_relation`, which reads `source.id` and
/// `source.ability` and never calls `source_defending_player`.
///
/// `TypedFilter { controller: Some(ControllerRef::DefendingPlayer) }` is ALSO
/// deliberately excluded despite having the identical door bug (Greatsword of
/// Tyr class). The deferral is SCOPED AND MEASURED, not open-ended — measured
/// against `data/card-data.json`, the exported engine corpus:
///
/// | population | cards |
/// |---|---|
/// | reference `ControllerRef::DefendingPlayer` anywhere | 116 |
/// | …of those, inside a TRIGGER's definition chain | 104 |
/// | …of those, inside a trigger's TARGET slot (the door this predicate gates) | 97 |
/// | the `FilterProp::Attacking { defender: DefendingPlayer }` shape fixed here | 3 |
///
/// So the follow-up's exact enumeration delta is 97 cards (Greatsword of Tyr,
/// Thraximundar, Kogla, Warkite Marauder, …) moving from the bare
/// `find_legal_targets` door to `find_legal_targets_for_ability`. It is a
/// separate change because 97 re-routed target enumerations need their own
/// multi-attacker fixtures and their own blast-radius measurement — not because
/// the size is unknown. The tripwire test
/// `filter_needs_trigger_source_does_not_widen_to_defending_player_controller`
/// keeps the omission a decision rather than an oversight.
///
/// STRUCTURAL TRAVERSAL IS NOT RE-IMPLEMENTED HERE. The "does this filter
/// mention X anywhere" question has exactly one authority — `filter::
/// filter_contains` and its `filter_prop_contains` / `player_filter_contains`
/// halves, whose matches are exhaustive (no `_` arm) precisely so a future
/// nesting variant cannot be silently classified as a leaf. A hand-rolled
/// `Typed` / `And` / `Or` / `Not` walk with a `_ => false` tail would miss the
/// prop under `TrackedSetFiltered`, `ChosenDamageSource`, `PlayerMatching {
/// ControlsCount { filter } }`, or any of the six `TargetFilter`-boxing props
/// (`Targets`, `TargetsOnly`, `SharesQuality`, `DistinctFrom`,
/// `DifferentNameFrom`, `CanEnchant`) — each of which would keep the bare
/// `find_legal_targets` door and reproduce the CR 603.3d removal above.
///
/// What remains local is a pure LEAF-VALUE test ("is this prop the
/// defending-player anaphor?"), including the two prop-level combinators
/// (`AnyOf` / `Not`) that can wrap it. Its `_ => false` is a value verdict on a
/// prop that carries no `defender` axis, not a containment claim about nesting.
fn filter_needs_trigger_source(filter: &TargetFilter) -> bool {
    fn prop_needs(prop: &FilterProp) -> bool {
        match prop {
            FilterProp::Attacking {
                defender: Some(ControllerRef::DefendingPlayer),
            }
            | FilterProp::AttackedThisTurn {
                defender: Some(ControllerRef::DefendingPlayer),
            } => true,
            FilterProp::AnyOf { props } => props.iter().any(prop_needs),
            FilterProp::Not { prop } => prop_needs(prop),
            _ => false,
        }
    }

    crate::game::filter::filter_contains(
        filter,
        &|inner| matches!(inner, TargetFilter::Typed(typed) if typed.properties.iter().any(prop_needs)),
    )
}

// CR 102.1 + CR 608.2c: "that player controls" filters lowered to
// ControllerRef::ScopedPlayer need the resolving ability's scoped-player binding
// when enumerating legal targets; source-controller-only enumeration would fall
// back to "you".
fn target_filter_contains_scoped_player_ref(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => {
            typed.controller == Some(ControllerRef::ScopedPlayer)
                || typed.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::Owned {
                            controller: ControllerRef::ScopedPlayer
                        }
                    )
                })
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_contains_scoped_player_ref)
        }
        TargetFilter::Not { filter } => target_filter_contains_scoped_player_ref(filter),
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
        FilterProp::Cmc { value, .. }
        | FilterProp::Counters { count: value, .. }
        | FilterProp::PtComparison { value, .. } => value.contains_x(),
        FilterProp::CanEnchant { target } => target_filter_contains_chosen_x_ref(target),
        FilterProp::DifferentNameFrom { filter }
        | FilterProp::TargetsOnly { filter }
        | FilterProp::Targets { filter } => target_filter_contains_chosen_x_ref(filter),
        FilterProp::SharesQuality { reference, .. } => reference
            .as_deref()
            .is_some_and(target_filter_contains_chosen_x_ref),
        FilterProp::AnyOf { props } => props.iter().any(filter_prop_contains_chosen_x_ref),
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
/// Mass-placement `target` field that is NOT surfaced as a normal target slot
/// (`PutCounterAll`, `DestroyAll`, …). Their `target_filter()` returns `None`
/// because the field is a resolution-time population scan, not a targeting filter.
/// Single authority for the mass-target variant list.
fn mass_all_target_filter(effect: &Effect) -> Option<&TargetFilter> {
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
        // CR 701.27a + CR 115.10a: mass Transform's `target` is a resolution-time
        // population scan (`target_filter()`==None), exactly like `TapAll`/`DestroyAll`.
        | Effect::Transform {
            scope: EffectScope::All,
            target,
            ..
        }
        | Effect::DoublePTAll { target, .. }
        // CR 508.1d + CR 109.4: the mass forced-attack population (Gideon Jura's
        // "creatures that player controls"). Listed here — not just excluded from
        // `target_filter()` — so its `ControllerRef::TargetOpponent` still
        // surfaces the COMPANION PLAYER slot the ability genuinely targets.
        | Effect::ForceAttack {
            scope: EffectScope::All,
            target,
            ..
        } => Some(target),
        _ => None,
    }
}

/// Shared walker behind the target-player / target-opponent companion-slot
/// detectors. Returns true if any filter seam (`Attach` operands, a non-targeted
/// `GenericEffect`'s static `affected`, the effect's own `target_filter()`, or a
/// mass-placement `target`) satisfies `pred`.
fn effect_bound_filter_matches(effect: &Effect, pred: fn(&TargetFilter) -> bool) -> bool {
    if let Effect::Attach { attachment, target } | Effect::UnattachAll { attachment, target } =
        effect
    {
        if pred(attachment) || pred(target) {
            return true;
        }
    }
    if let Effect::GenericEffect {
        static_abilities,
        target: None,
        ..
    } = effect
    {
        if static_abilities
            .iter()
            .any(|static_def| static_def.affected.as_ref().is_some_and(pred))
        {
            return true;
        }
    }
    if effect.target_filter().is_some_and(pred) {
        return true;
    }
    mass_all_target_filter(effect).is_some_and(pred)
}

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
    // CR 115.1 + CR 404 + CR 406: A mass filter set to a bare `TargetFilter::Player`
    // (e.g. `ChangeZoneAll { origin: Graveyard, target: Player }` for "exile target
    // player's graveyard" — Nihil Spellbomb class) parameterizes the scan by a player
    // target and needs a companion slot even though no controller ref is present.
    if mass_all_target_filter(effect).is_some_and(|t| matches!(t, TargetFilter::Player)) {
        return true;
    }
    effect_bound_filter_matches(effect, filter_references_target_player)
}

/// CR 109.4 + CR 102.2 / CR 102.3: opponent-constrained sibling of
/// `effect_references_target_player` — no `AddRestriction` / bare-`Player` branch
/// (those are opponent-agnostic). Drives the companion-slot legal-target
/// discriminator only.
fn effect_references_target_opponent(effect: &Effect) -> bool {
    effect_bound_filter_matches(effect, filter_references_target_opponent)
}

fn ability_needs_companion_target_player_slot(ability: &ResolvedAbility) -> bool {
    // Triggered abilities carry an exact trigger source. Hellkite-style
    // GainControlAll uses "that player" from the triggering event, not a
    // declared target player, so surfacing a stack target here makes it fizzle.
    if matches!(ability.effect, Effect::GainControlAll { .. }) && ability.trigger_source.is_some() {
        return false;
    }
    effect_references_target_player(&ability.effect)
        // CR 115.1 + CR 118.12a: a targeted unless-payer declared inside the unless
        // clause surfaces its own player target slot even when the primary effect
        // references no target player (e.g. Athreos, God of Passage).
        || ability
            .unless_pay
            .as_ref()
            .is_some_and(|m| payer_is_declared_target(&m.payer))
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
            TargetFilter::LastCreated | TargetFilter::LastRevealed | TargetFilter::LastZoneChanged
        )
}

fn attach_side_needs_target_slot(filter: &TargetFilter, is_attachment: bool) -> bool {
    if is_attachment {
        attach_attachment_filter_needs_target_slot(filter)
    } else {
        attach_host_filter_needs_target_slot(filter)
    }
}

/// CR 115.1d: "attach any number of target Equipment" carries a `multi_target`
/// spec with min 0. Honor it on the attachment operand instead of the single-slot
/// `optional_targeting` path so the controller can choose zero Equipment.
fn collect_attach_attachment_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    attachment: &TargetFilter,
    acc: &mut SlotAccumulator,
) -> Result<(), EngineError> {
    if !attach_attachment_filter_needs_target_slot(attachment) {
        return Ok(());
    }
    let legal_targets = legal_targets_for_ability_filter(state, ability, attachment, &acc.slots);
    if legal_targets.is_empty() && !ability.targeting_is_optional() {
        return Err(EngineError::ActionNotAllowed(
            "No legal targets available".to_string(),
        ));
    }
    if let Some(spec) = ability.multi_target.as_ref() {
        let bounds = resolve_multi_target_bounds(state, ability, spec, legal_targets.len())?;
        for slot_index in 0..bounds.max {
            acc.push(TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: slot_index >= bounds.min,
                chooser: None,
                effect_kind: acc.current_effect_kind,
                effect_detail: acc.current_effect_detail,
            });
        }
    } else {
        acc.push(TargetSelectionSlot {
            legal_targets,
            optional: ability.targeting_is_optional(),
            chooser: None,
            effect_kind: acc.current_effect_kind,
            effect_detail: acc.current_effect_detail,
        });
    }
    Ok(())
}

fn collect_attach_attachment_target_slot_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    attachment: &TargetFilter,
    specs: &mut Vec<TargetSlotSpec>,
    next_instance: &mut usize,
) {
    if !attach_attachment_filter_needs_target_slot(attachment) {
        return;
    }
    if let Some(spec) = ability.multi_target.as_ref() {
        let legal_targets = legal_targets_for_ability_filter(state, ability, attachment, &[]);
        if let Ok(bounds) = resolve_multi_target_bounds(state, ability, spec, legal_targets.len()) {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            for slot_index in 0..bounds.max {
                specs.push(TargetSlotSpec {
                    filter: attachment.clone(),
                    optional: slot_index >= bounds.min,
                    instance: id,
                });
            }
        }
    } else {
        let id = TargetInstanceId(*next_instance);
        *next_instance += 1;
        specs.push(TargetSlotSpec {
            filter: attachment.clone(),
            optional: ability.targeting_is_optional(),
            instance: id,
        });
    }
}

/// Slot bounds for the attachment operand of `Effect::Attach`, mirroring
/// `collect_attach_attachment_target_slots` so assignment consumes exactly the
/// surfaced attachment window and does not bleed into a trailing host slot.
fn attach_attachment_slot_bounds(
    state: &GameState,
    ability: &ResolvedAbility,
    attachment: &TargetFilter,
) -> Result<Option<MultiTargetBounds>, EngineError> {
    if !attach_attachment_filter_needs_target_slot(attachment) {
        return Ok(None);
    }
    if let Some(spec) = &ability.multi_target {
        let legal_targets = legal_targets_for_ability_filter(state, ability, attachment, &[]);
        let bounds = resolve_multi_target_bounds(state, ability, spec, legal_targets.len())?;
        Ok(Some(bounds))
    } else {
        Ok(Some(MultiTargetBounds {
            min: usize::from(!ability.targeting_is_optional()),
            max: 1,
        }))
    }
}

fn assign_attach_attachment_selected_slots(
    state: &GameState,
    ability: &mut ResolvedAbility,
    attachment: &TargetFilter,
    selected_slots: &[Option<TargetRef>],
    next_slot: &mut usize,
) -> Result<(), EngineError> {
    let Some(bounds) = attach_attachment_slot_bounds(state, ability, attachment)? else {
        return Ok(());
    };
    let allow_skip = ability.targeting_is_optional();
    if ability.multi_target.is_some() {
        let attachment_slot_count = bounds.max;
        let end_slot = *next_slot + attachment_slot_count;
        let Some(window) = selected_slots.get(*next_slot..end_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        if window.len() < bounds.min
            || window[..bounds.min.min(window.len())]
                .iter()
                .any(Option::is_none)
        {
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
            None if allow_skip => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }
    Ok(())
}

fn attach_declared_host_slot_reserve(
    state: &GameState,
    ability: &ResolvedAbility,
    host: &TargetFilter,
    targets: &[TargetRef],
    next_target: usize,
) -> usize {
    if !attach_host_filter_needs_target_slot(host) {
        return 0;
    }
    if !ability.optional_targeting {
        return 1;
    }
    let Some(last) = targets.last() else {
        return 0;
    };
    if targets.len() <= next_target {
        return 0;
    }
    let legal_hosts = legal_targets_for_ability_filter(state, ability, host, &[]);
    usize::from(legal_hosts.contains(last))
}

fn assign_attach_attachment_declared_targets(
    state: &GameState,
    ability: &mut ResolvedAbility,
    attachment: &TargetFilter,
    host: &TargetFilter,
    targets: &[TargetRef],
    next_target: &mut usize,
) -> Result<(), EngineError> {
    let Some(bounds) = attach_attachment_slot_bounds(state, ability, attachment)? else {
        return Ok(());
    };
    let allow_skip = ability.targeting_is_optional();
    if ability.multi_target.is_some() {
        let remaining = targets.len().saturating_sub(*next_target);
        let host_reserved =
            attach_declared_host_slot_reserve(state, ability, host, targets, *next_target);
        let attachment_window = remaining.saturating_sub(host_reserved).min(bounds.max);
        if remaining.saturating_sub(host_reserved) < bounds.min {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
        for slot_index in 0..attachment_window {
            if let Some(target) = targets.get(*next_target) {
                ability.targets.push(target.clone());
                *next_target += 1;
            } else if slot_index < bounds.min {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            } else {
                break;
            }
        }
    } else if let Some(target) = targets.get(*next_target) {
        ability.targets.push(target.clone());
        *next_target += 1;
    } else if !allow_skip {
        return Err(EngineError::InvalidAction(
            "Missing required target".to_string(),
        ));
    }
    Ok(())
}

/// Tree-walks a `TargetFilter` and returns true if any `TypedFilter` inside it
/// carries a `controller` (or `Owned` property) satisfying `pred`. Shared walker
/// behind `filter_references_target_player` / `filter_references_target_opponent`.
fn filter_binds_controller(filter: &TargetFilter, pred: fn(&ControllerRef) -> bool) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter {
            controller,
            properties,
            ..
        }) => {
            controller.as_ref().is_some_and(pred)
                || properties.iter().any(
                    |prop| matches!(prop, FilterProp::Owned { controller } if pred(controller)),
                )
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(|f| filter_binds_controller(f, pred))
        }
        TargetFilter::Not { filter } => filter_binds_controller(filter, pred),
        _ => false,
    }
}

/// CR 109.4 + CR 115.1: Returns true if any `TypedFilter` binds to a
/// declared-target player scope — `TargetPlayer` (any player) or `TargetOpponent`
/// (opponent-only). Both surface a companion player slot; they differ only in that
/// slot's legal targets, so slot *detection* treats them alike.
pub(crate) fn filter_references_target_player(filter: &TargetFilter) -> bool {
    filter_binds_controller(filter, |c| {
        matches!(
            c,
            ControllerRef::TargetPlayer | ControllerRef::TargetOpponent
        )
    })
}

/// CR 109.4 + CR 102.2 / CR 102.3: Opponent-constrained mirror — true only for
/// `TargetOpponent`. Drives the companion-slot legal-target discriminator
/// (opponent-only vs. any player).
fn filter_references_target_opponent(filter: &TargetFilter) -> bool {
    filter_binds_controller(filter, |c| matches!(c, ControllerRef::TargetOpponent))
}

/// CR 115.1 + CR 118.12a: True when an `UnlessPayModifier` payer was DECLARED as a
/// target inside the unless clause ("unless target opponent/target player pays"),
/// as opposed to an anaphoric payer ("they pay" -> `Player`, "that player pays" ->
/// `TriggeringPlayer`). The declared-target forms are the only player-typed `Typed`
/// payers with empty type filters/properties and a None/Opponent controller; no
/// anaphoric path emits that shape, so the match is unambiguous.
///
/// Single authority for the declared-target shape: slot creation here, the
/// payer resolver in `effects::resolve_unless_payer`, and the `Typed` arm in
/// `targeting::resolve_effect_player_ref` all gate on this one predicate so the
/// structural guard cannot drift as new parser shapes are added.
pub(crate) fn payer_is_declared_target(payer: &TargetFilter) -> bool {
    matches!(
        payer,
        TargetFilter::Typed(tf)
            if tf.type_filters.is_empty()
                && tf.properties.is_empty()
                && matches!(tf.controller, None | Some(ControllerRef::Opponent))
    )
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
                // CR 109.4: TargetPlayer / TargetOpponent are ambiguous here (player
                // targets are resolved from ability.targets directly); fail closed.
                Some(ControllerRef::TargetPlayer | ControllerRef::TargetOpponent) => false,
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
                // CR 303.4b: The player the source Aura is attached to.
                Some(ControllerRef::EnchantedPlayer) => {
                    state
                        .objects
                        .get(&ability.source_id)
                        .and_then(|source| source.attached_to)
                        .and_then(|host| host.as_player())
                        == Some(p.id)
                }
                // CR 102.1 + CR 109.4: the active player, resolvable directly
                // (unlike the fail-closed DefendingPlayer arm above).
                Some(ControllerRef::ActivePlayer) => p.id == state.active_player,
                // CR 109.4 + CR 611.2: a snapshotted id, resolvable directly.
                Some(ControllerRef::SpecificPlayer { id }) => p.id == *id,
                None => true,
            })
            .map(|p| p.id)
            .collect(),
        _ => Vec::new(),
    }
}

fn parent_target_combat_relation_slot_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn effect_needs_parent_target_combat_relation_slot(effect: &Effect) -> bool {
    effect_references_parent_target_combat_relation(effect)
}

fn effect_needs_target_creature_quantity_slot(effect: &Effect) -> bool {
    effect_target_slot_filter(effect).is_some()
        && !effect_primary_target_supplies_creature_target(effect)
}

/// CR 608.2c + CR 115.1: Chained riders like Swords to Plowshares ("Exile target
/// creature. Its controller gains life equal to its power.") reuse the parent's
/// chosen object for the life-gain magnitude. They must not surface a second
/// creature target slot for the `Power {{ Target }}` quantity ref — the parent's
/// slot is the only player choice (issue #3864; same class as #3310 Condemn).
fn sub_ability_inherits_parent_creature_target_only(
    parent: &ResolvedAbility,
    sub: &ResolvedAbility,
) -> bool {
    if !chain_has_target_sink(parent) {
        return false;
    }
    if triggers::extract_target_filter_from_effect(&sub.effect).is_some() {
        return false;
    }
    if sub.multi_target.is_some() {
        return false;
    }
    if ability_needs_companion_target_player_slot(sub) {
        return false;
    }
    if matches!(
        &sub.effect,
        Effect::Attach { .. } | Effect::ExchangeControl { .. }
    ) {
        return false;
    }
    effect_needs_target_creature_quantity_slot(&sub.effect)
        && effect_player_filter_is_parent_target_anaphor(&sub.effect)
}

/// CR 115.1 + CR 115.10a + CR 608.2c: A one-sided-fight `DealDamage` ("Target
/// creature you control deals damage equal to its power to target creature or
/// planeswalker you don't control") reuses the parent-declared source creature
/// (`targets[0]`) for BOTH the damage source (`damage_source: Target`) and the
/// `Power { Target }` / `Toughness { Target }` magnitude. The amount's per-target
/// creature-quantity slot would therefore surface a SECOND "target creature" —
/// the bug in GH #4234, where Bite Down asked for one target too many (CR 601.2c:
/// one slot per distinct instance of "target", and the magnitude here is NOT a
/// distinct instance — "its power" anaphorically reuses the source).
///
/// Sibling of `sub_ability_inherits_parent_creature_target_only`, which handles
/// the Swords to Plowshares GainLife rider whose ONLY slot is the redundant
/// magnitude; here the genuine recipient slot remains, so we drop only the
/// magnitude slot. The boost variant (Bite Down on Crime / Ambuscade) reads
/// `Power { Anaphoric }`, which surfaces no quantity slot, so it is unaffected.
///
/// `damage_source: Some(Target)` is only emitted for a one-sided-fight clause
/// whose damage-dealing object was named with "target" in an earlier clause (the
/// subject, e.g. "Target creature you control deals…"), so that earlier slot
/// always supplies `targets[0]`; the magnitude `Power { Target }` reads the SAME
/// `targets[0]` and never needs a slot of its own. This is purely a function of
/// the effect shape, so every slot-mapping site (producer, spec builder, both
/// consumers, the minimum-count) can apply it identically and stay in lockstep
/// (CR 700.2 slot-mapping invariant). It only ever fires when
/// `effect_needs_target_creature_quantity_slot` is already true — i.e. the
/// recipient filter (here `Or[Creature, Planeswalker] you don't control`) failed
/// `effect_primary_target_supplies_creature_target`, the exact gap that left Bite
/// Down asking for an extra target while Rabid Bite (plain creature recipient)
/// was already correct — so it can only drop a redundant slot, never a real one.
fn one_sided_fight_source_supplies_quantity_creature(effect: &Effect) -> bool {
    let amount = match effect {
        Effect::DealDamage {
            damage_source: Some(DamageSource::Target),
            amount,
            ..
        }
        | Effect::DamageAll {
            damage_source: Some(DamageSource::Target),
            amount,
            ..
        } => amount,
        _ => return false,
    };
    // CR 208.1: the magnitude reads the Target-scoped source object's P/T — the
    // same object `damage_source: Target` reads. Recipient-scoped or fixed
    // magnitudes keep their own slots.
    quantity_expr_reads_target_object_pt(amount)
}

/// CR 208.1: whether a magnitude reads the Target-scoped object's power or
/// toughness (`Power { Target }` / `Toughness { Target }`), recursing through the
/// arithmetic wrappers `quantity_expr_target_slot_filter` already traverses.
fn quantity_expr_reads_target_object_pt(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => matches!(
            qty,
            QuantityRef::Power {
                scope: ObjectScope::Target,
            } | QuantityRef::Toughness {
                scope: ObjectScope::Target,
            }
        ),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_reads_target_object_pt(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_expr_reads_target_object_pt)
        }
        QuantityExpr::Difference { left, right } => {
            quantity_expr_reads_target_object_pt(left)
                || quantity_expr_reads_target_object_pt(right)
        }
        QuantityExpr::Fixed { .. } => false,
    }
}

fn effect_player_filter_is_parent_target_anaphor(effect: &Effect) -> bool {
    match effect {
        Effect::GainLife { player, .. } => matches!(
            player,
            TargetFilter::ParentTargetController | TargetFilter::ParentTargetOwner
        ),
        _ => false,
    }
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
        // CR 701.27a + CR 115.10a: parity with the other mass-population `target`
        // filters — mass Transform's population filter is walked here too.
        | Effect::Transform {
            scope: EffectScope::All,
            target,
            ..
        }
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

/// CR 115.1: Derive the `TargetFilter` for the count-derived target slot an
/// effect needs, if any. Walks each amount/target arm and maps the inner
/// quantity/filter through `quantity_ref_target_slot_spec` (the spec authority),
/// returning the FIRST `Some`. `Some(filter)` means the effect's magnitude/scope
/// references a value that requires its own surfaced target slot whose legal
/// candidates are `filter`; `None` means no count-derived slot is needed.
fn effect_target_slot_filter(effect: &Effect) -> Option<TargetFilter> {
    if let Some(filter) = effect.target_filter().and_then(filter_target_slot_filter) {
        return Some(filter);
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
        | Effect::Sacrifice { count: amount, .. } => quantity_expr_target_slot_filter(amount),
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
        // CR 701.27a + CR 115.10a: mass Transform's population filter is a
        // resolution-time scan, walked here like the other mass-`All` effects.
        | Effect::Transform {
            scope: EffectScope::All,
            target,
            ..
        }
        | Effect::DoublePTAll { target, .. } => filter_target_slot_filter(target),
        _ => None,
    }
}

fn filter_target_slot_filter(filter: &TargetFilter) -> Option<TargetFilter> {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => {
            properties.iter().find_map(filter_prop_target_slot_filter)
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().find_map(filter_target_slot_filter)
        }
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            filter_target_slot_filter(filter)
        }
        _ => None,
    }
}

/// The first target-slot filter reachable through a [`CardTypeSetSource`]
/// population.
///
/// Only the object-filter and journal-filter arms carry a `TargetFilter` that
/// could name a target slot; the zone / linked-exile / tracked-set arms are
/// fixed-vocabulary. `AnyOf` recurses so a union member's slot is not dropped.
///
/// Deliberately UNCITED. This is a structural query over the AST — which arms
/// hold a filter — not a rule implementation. It previously cited CR 109.2,
/// which says a bare type description means a permanent on the battlefield;
/// that rule has nothing to say about target-slot extraction, and a citation
/// that does not support its code is worse than none because it reads as
/// evidence the behavior was checked against the rules.
fn characteristic_source_target_slot_filter(source: &CardTypeSetSource) -> Option<TargetFilter> {
    // FIRST match wins, preserving the previous `find_map` semantics: the walker
    // visits members in declaration order, and later members do not overwrite an
    // earlier hit. Truncation needs no conservative branch here — this returns a
    // slot to wire, and inventing one would be worse than finding none.
    let mut found: Option<TargetFilter> = None;
    source.try_for_each_member(crate::types::ability::UNION_DEPTH_BUDGET, &mut |leaf| {
        if found.is_some() {
            return;
        }
        found = match leaf {
            CardTypeSetSource::Objects { filter } => filter_target_slot_filter(filter),
            CardTypeSetSource::TurnJournal { filter, .. } => {
                filter.as_ref().and_then(filter_target_slot_filter)
            }
            CardTypeSetSource::Zone { .. }
            | CardTypeSetSource::ExiledBySource
            | CardTypeSetSource::TrackedSet { .. }
            | CardTypeSetSource::AnyOf { .. } => None,
        };
    });
    found
}

fn filter_prop_target_slot_filter(
    prop: &crate::types::ability::FilterProp,
) -> Option<TargetFilter> {
    match prop {
        crate::types::ability::FilterProp::Counters { count, .. }
        | crate::types::ability::FilterProp::Cmc { value: count, .. }
        | crate::types::ability::FilterProp::PtComparison { value: count, .. } => {
            quantity_expr_target_slot_filter(count)
        }
        crate::types::ability::FilterProp::CanEnchant { target } => {
            filter_target_slot_filter(target)
        }
        crate::types::ability::FilterProp::AnyOf { props } => {
            props.iter().find_map(filter_prop_target_slot_filter)
        }
        // CR 608.2c: Negation reads the inner prop's references — recurse (mirrors AnyOf).
        crate::types::ability::FilterProp::Not { prop } => filter_prop_target_slot_filter(prop),
        crate::types::ability::FilterProp::DifferentNameFrom { filter } => {
            filter_target_slot_filter(filter)
        }
        crate::types::ability::FilterProp::SharesQuality { reference, .. } => {
            reference.as_deref().and_then(filter_target_slot_filter)
        }
        crate::types::ability::FilterProp::TargetsOnly { filter }
        | crate::types::ability::FilterProp::Targets { filter } => {
            filter_target_slot_filter(filter)
        }
        _ => None,
    }
}

fn quantity_expr_target_slot_filter(expr: &QuantityExpr) -> Option<TargetFilter> {
    match expr {
        QuantityExpr::Ref { qty } => quantity_ref_target_slot_spec(qty),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => quantity_expr_target_slot_filter(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().find_map(quantity_expr_target_slot_filter)
        }
        QuantityExpr::Difference { left, right } => quantity_expr_target_slot_filter(left)
            .or_else(|| quantity_expr_target_slot_filter(right)),
        QuantityExpr::Fixed { .. } => None,
    }
}

/// CR 115.1: The single authority mapping a count `QuantityRef` to the
/// `TargetFilter` of the target slot that count requires (if any). A `Some`
/// result means this ref references a TARGET object/player and the surfaced
/// slot's legal candidates are the returned filter; the slot filter is DERIVED
/// from the ref itself, never assumed to be "creature". `None` means the ref
/// reads a value that needs no target slot.
fn quantity_ref_target_slot_spec(qty: &QuantityRef) -> Option<TargetFilter> {
    match qty {
        // CR 208.1: power/toughness are creature numbers — the target slot is a creature.
        QuantityRef::Power {
            scope: ObjectScope::Target,
        }
        | QuantityRef::BasePower {
            scope: ObjectScope::Target,
        }
        | QuantityRef::Toughness {
            scope: ObjectScope::Target,
        } => Some(TargetFilter::Typed(TypedFilter::creature())),
        QuantityRef::Power { .. }
        | QuantityRef::BasePower { .. }
        | QuantityRef::Toughness { .. } => None,
        // CR 202.3 + CR 115.1: the ref carries its own slot filter.
        QuantityRef::TargetObjectManaValue { filter } => Some((**filter).clone()),
        // CR 701.9 + CR 115.1: cards a single targeted opponent discarded this
        // turn (Discard keyword action; NOT 121.1, which is Draw). Other player
        // scopes are not target-bearing and fall through.
        QuantityRef::CardsDiscardedThisTurn {
            player: PlayerScope::Target,
        } => Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        )),
        QuantityRef::CardsDiscardedThisTurn { .. } => None,
        // CR 115.1 + CR 109.4: surface an OPPONENT-scoped PLAYER slot (enumerable);
        // TargetPlayer is non-enumerable (targeting.rs fails closed) since
        // ability.targets is empty at selection. The derived slot must be a BARE
        // opponent filter (identical to the CardsDiscardedThisTurn{Target} arm
        // above) — NOT the record-match `And{[Player, Typed(controller=TargetPlayer)]}`
        // rewritten in place. A player cannot satisfy a `Typed` (object) leaf, so an
        // And-wrapped `Typed` slot enumerates ZERO players; for a required trigger
        // slot that empties legal targets and `collect_target_slots` errors (CR
        // 603.3d), so the trigger silently resolves target-less. The parser-stored
        // record-match filter on the `QuantityRef` is UNCHANGED; resolution reads
        // the chosen opponent from `ability.targets`. The non-targeted "your
        // opponents" class (And{[Player, controller=Opponent]}) returns None here —
        // it surfaces no slot.
        QuantityRef::DamageDealtThisTurn { target, .. }
            if relative_controller_kind(target) == Some(ControllerRef::TargetPlayer) =>
        {
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        }
        // CR 120.9: a DamageDealtThisTurn whose source or target embeds a
        // target-creature quantity (e.g. aggregate over "target creature") still
        // needs a creature slot, matching the legacy behavior; otherwise no slot.
        QuantityRef::DamageDealtThisTurn { source, target, .. } => {
            filter_target_slot_filter(source).or_else(|| filter_target_slot_filter(target))
        }
        // Count-over-filter refs: the slot is creature-typed when a nested filter
        // references a target-creature quantity (preserves today's behavior).
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. }
        | QuantityRef::EnteredThisTurn { filter }
        // CR 608.2i: the look-back sibling of `EnteredThisTurn` carries the same
        // kind of population filter, so it must reach the same recursion instead
        // of dropping into the `_ => None` fallback. Direct precedent in this
        // group: `SacrificedThisTurn` / `TokensCreatedThisTurn`, both
        // `PlayerScope`-carrying history refs.
        | QuantityRef::BattlefieldEntriesThisTurn { filter, .. }
        | QuantityRef::SacrificedThisTurn { filter, .. }
        | QuantityRef::ZoneChangeCountThisTurn { filter, .. }
        | QuantityRef::ZoneChangeAggregateThisTurn { filter, .. }
        | QuantityRef::CounterAddedThisTurn { target: filter, .. }
        | QuantityRef::TokensCreatedThisTurn { filter, .. }
        | QuantityRef::DistinctCounterKindsAmong { filter } => filter_target_slot_filter(filter),
        QuantityRef::SpellsCastThisTurn { filter, .. }
        | QuantityRef::SpellsCastBeforeTriggeringSpell { filter, .. }
        | QuantityRef::SpellsCastThisGame { filter, .. } => {
            filter.as_ref().and_then(filter_target_slot_filter)
        }
        QuantityRef::DistinctCardTypes { source }
        | QuantityRef::DistinctSubtypes { source, .. }
        | QuantityRef::DistinctColorsAmong { source } => {
            characteristic_source_target_slot_filter(source)
        }
        QuantityRef::ManaSpentToCast { metric, .. } => match metric {
            CastManaSpentMetric::FromSource { source_filter } => {
                filter_target_slot_filter(source_filter)
            }
            CastManaSpentMetric::Total
            | CastManaSpentMetric::DistinctColors
            | CastManaSpentMetric::OfColor { .. } => None,
        },
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::ControlsCount { filter, .. },
        } => filter_target_slot_filter(filter),
        // CR 402.1 / 119.1 / 122.1f / 404.1: a player-scalar predicate is read
        // off each candidate player, never off a target creature, so it cannot
        // reference the resolving ability's target-creature slot.
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::PlayerAttribute { .. },
        } => None,
        _ => None,
    }
}

/// Thin `.is_some()` wrapper over `quantity_expr_target_slot_filter` so the
/// `#[cfg(test)]` assertions below still read as "references a target creature".
#[cfg(test)]
fn quantity_expr_references_target_creature(expr: &QuantityExpr) -> bool {
    quantity_expr_target_slot_filter(expr).is_some()
}

/// Thin `.is_some()` wrapper retained for the `#[cfg(test)]` assertions.
#[cfg(test)]
fn filter_references_target_creature_quantity(filter: &TargetFilter) -> bool {
    filter_target_slot_filter(filter).is_some()
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

    // CR 701.14a + CR 115.1: Mirror the dual-fighter `Fight` branch in
    // `collect_target_slots` so per-slot specs line up one-for-one.
    if let Effect::Fight { subject, target } = &ability.effect {
        let mut filters: Vec<&TargetFilter> = Vec::new();
        if fight_subject_needs_target_slot(subject) {
            filters.push(subject);
        }
        filters.push(target);
        for filter in filters {
            // Keep per-slot metadata aligned with the surfaced cast-time slots.
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
    } else if let Some(role) = mana_multi_role(&ability.effect) {
        // CR 601.2c: EXACT MIRROR of the `mana_multi_role` arm in
        // `collect_target_slots` — same gate, same `surfaced_filters()` order
        // (recipient, then count source), same else-if placement and
        // fall-through. NO assertion links spec count/order to slot count/order,
        // so any divergence here fails SILENTLY as misaligned
        // `TargetInstanceId`s at runtime.
        for (_slot, filter) in role.surfaced_filters() {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
        }
    } else if let Effect::Attach { attachment, target } = &ability.effect {
        collect_attach_attachment_target_slot_specs(
            state,
            ability,
            attachment,
            specs,
            next_instance,
        );
        if attach_host_filter_needs_target_slot(target) {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: target.clone(),
                optional: ability.optional_targeting,
                instance: id,
            });
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
    } else if let Effect::EachDealsDamageEqualToPower {
        sources,
        recipient,
        extra_source,
    } = &ability.effect
    {
        // CR 115.1d + CR 115.1: Mirror the `collect_target_slots` branch
        // one-for-one — the variable-count SOURCE slots first (sharing one
        // instance per CR 115.3 so the same creature can't fill two source
        // slots), then the single mandatory RECIPIENT slot (its own instance).
        if ability.target_choice_timing == TargetChoiceTiming::Stack {
            let source_legal = legal_targets_for_ability_filter(state, ability, sources, &[]);
            if let Some(spec) = ability.multi_target.as_ref() {
                if let Ok(bounds) =
                    resolve_multi_target_bounds(state, ability, spec, source_legal.len())
                {
                    let id = TargetInstanceId(*next_instance);
                    *next_instance += 1;
                    for slot_index in 0..bounds.max {
                        specs.push(TargetSlotSpec {
                            filter: sources.clone(),
                            optional: slot_index >= bounds.min,
                            instance: id,
                        });
                    }
                }
            } else {
                let id = TargetInstanceId(*next_instance);
                *next_instance += 1;
                specs.push(TargetSlotSpec {
                    filter: sources.clone(),
                    optional: false,
                    instance: id,
                });
            }
            // CR 115.4 + CR 601.2c: group-B spec — its OWN instance, between the
            // group-A specs and the recipient spec. Mirrors the `collect_target_slots`
            // group-B slot so specs line up one-for-one (the slot-count
            // debug_assert in `build_target_slots_labelled` stays balanced).
            if let Some(extra) = extra_source {
                let id = TargetInstanceId(*next_instance);
                *next_instance += 1;
                specs.push(TargetSlotSpec {
                    filter: extra.clone(),
                    optional: true,
                    instance: id,
                });
            }
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: recipient.clone(),
                optional: false,
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
            && !one_sided_fight_source_supplies_quantity_creature(&ability.effect)
        {
            let id = TargetInstanceId(*next_instance);
            *next_instance += 1;
            specs.push(TargetSlotSpec {
                filter: effect_target_slot_filter(&ability.effect)
                    .expect("slot filter present when gate true"),
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
        if !defers_conditional_target_selection(sub_ability)
            && !sub_ability_inherits_parent_creature_target_only(ability, sub_ability)
        {
            collect_target_slot_specs(state, sub_ability, specs, next_instance);
        }
    }
}

/// CR 601.2c / CR 602.2b: Targets are chosen before costs are paid. This
/// engine pays a non-self Sacrifice/Discard/Exile activation cost BEFORE
/// target selection as a documented architectural shortcut (see the ordering
/// note in `push_activated_ability_to_stack`), so any object that cost just
/// moved off the battlefield must not become newly eligible for an unrelated
/// target slot just because it now sits in the destination zone. Cauldron of
/// Essence's official ruling states this explicitly: "the target ... can't be
/// the creature sacrificed to pay its cost." Costs that leave the object on
/// the battlefield (Tap, Blight, RemoveCounter) never made it newly eligible
/// for a different zone, so they are correctly left untouched by this gate.
///
/// Issue #4948 — Samwise Gamgee: checks EVERY object the cost
/// consumed (`ability.cost_paid_object_ids`), not just the single referent in
/// `ability.cost_paid_object`. A multi-object non-self cost (e.g. "Sacrifice
/// three Foods") can move several objects into the same zone this ability's
/// own target searches at once; excluding only the first left the rest
/// eligible, so a just-sacrificed token could be chosen as its own ability's
/// target and then cease to exist (CR 704.5d) before resolution, silently
/// fizzling it (CR 608.2b). `cost_paid_object`'s id is folded in too as a
/// defense-in-depth fallback for any cost-payment site that stamps the
/// singular referent without also calling
/// `add_cost_paid_object_ids_recursive`.
fn exclude_cost_paid_object_that_left_battlefield(
    state: &GameState,
    ability: &ResolvedAbility,
    targets: Vec<TargetRef>,
) -> Vec<TargetRef> {
    if ability.cost_paid_object_ids.is_empty() && ability.cost_paid_object.is_none() {
        return targets;
    }
    let left_battlefield = |id: ObjectId| match state.objects.get(&id) {
        Some(obj) => obj.zone != Zone::Battlefield,
        None => true,
    };
    targets
        .into_iter()
        .filter(|target| match target {
            TargetRef::Object(id) => {
                let was_paid_as_cost = ability.cost_paid_object_ids.contains(id)
                    || ability
                        .cost_paid_object
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.object_id == *id);
                !(was_paid_as_cost && left_battlefield(*id))
            }
            TargetRef::Player(_) => true,
        })
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

    let needs_ability_context = target_filter_needs_ability_context(filter);
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
        Some(crate::types::ability::ControllerRef::TargetPlayer) => {
            rewrite_declared_target_player(filter, crate::types::ability::ControllerRef::You)
        }
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
            // CR 109.4 + CR 102.2 / CR 102.3: normalize the opponent-constrained scope
            // to TargetPlayer so the per-player re-enumeration guards / rewrite args
            // (hardcoded on TargetPlayer) fire; the opponent constraint already lives
            // in the companion slot's legal-target set.
            Some(ControllerRef::TargetPlayer) | Some(ControllerRef::TargetOpponent) => {
                Some(ControllerRef::TargetPlayer)
            }
            _ => tf.properties.iter().find_map(|prop| match prop {
                FilterProp::Owned {
                    controller: ControllerRef::You,
                } => Some(ControllerRef::You),
                FilterProp::Owned {
                    controller: ControllerRef::TargetPlayer | ControllerRef::TargetOpponent,
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

pub(crate) fn is_per_opponent_target_fanout(ability: &ResolvedAbility) -> bool {
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
        // Hygiene routing, behaviour-neutral BY CONSTRUCTION: the inline pair
        // `!is_eliminated && !is_phased_out()` under a membership `any` is exactly what
        // `players::player_exists_for_choice` spells (`is_alive` is itself membership ∧
        // ¬eliminated). Routed so an existence fix propagates here rather than leaving a
        // fifth hand-inlined copy of the predicate.
        .filter(|&id| id != controller && players::player_exists_for_choice(state, id))
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
    ability
        .effect
        .target_filter()
        .map(|filter| rewrite_declared_target_player(filter, ControllerRef::You))
}

fn per_opponent_fanout_legal_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    bound_player: PlayerId,
) -> Vec<TargetRef> {
    let Some(object_filter) = per_opponent_fanout_object_filter(ability) else {
        return Vec::new();
    };
    targeting::find_legal_object_targets_for_ability_with_filter_controller(
        state,
        &object_filter,
        ability,
        bound_player,
    )
}

fn collect_per_opponent_target_fanout_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    acc: &mut SlotAccumulator,
) -> Result<(), EngineError> {
    if per_opponent_fanout_object_filter(ability).is_none() {
        return Ok(());
    }

    for opponent in per_opponent_fanout_players(state, ability.controller) {
        let legal_targets = per_opponent_fanout_legal_object_targets(state, ability, opponent);
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
            chooser: None,
            effect_kind: acc.current_effect_kind,
            effect_detail: acc.current_effect_detail,
        });
        acc.push(TargetSelectionSlot {
            legal_targets,
            optional: ability.targeting_is_optional(),
            chooser: None,
            effect_kind: acc.current_effect_kind,
            effect_detail: acc.current_effect_detail,
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
            && per_opponent_fanout_legal_object_targets(state, ability, opponent).is_empty()
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
    if per_opponent_fanout_object_filter(ability).is_none() {
        return Vec::new();
    }

    let mut current_player = None;
    let mut legal = Vec::new();
    for target in &ability.targets {
        match target {
            TargetRef::Player(player_id) => current_player = Some(*player_id),
            TargetRef::Object(object_id) => {
                let Some(player_id) = current_player else {
                    continue;
                };
                let legal_targets =
                    per_opponent_fanout_legal_object_targets(state, ability, player_id);
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

/// CR 109.4 + CR 102.2 / CR 102.3: rewrite BOTH declared-target-player scopes
/// (`TargetPlayer` and `TargetOpponent`) to `to`. `relative_controller_kind`
/// normalizes `TargetOpponent` → `TargetPlayer`, so a naive single-`TargetPlayer`
/// rewrite would leave a `TargetOpponent` occurrence behind and the per-player
/// enumeration would fail closed.
// ponytail: covers the dependent-object-slot subclass ("destroy target creature
// target opponent controls"); the mass class (Quick Draw / DamageAll) has
// target_filter() == None and never reaches these rewrite sites.
fn rewrite_declared_target_player(
    filter: &TargetFilter,
    to: crate::types::ability::ControllerRef,
) -> TargetFilter {
    use crate::types::ability::ControllerRef;
    let rewritten = rewrite_relative_controller(filter, ControllerRef::TargetPlayer, to.clone());
    rewrite_relative_controller(&rewritten, ControllerRef::TargetOpponent, to)
}

/// CR 201.5a + CR 613.1f: Concretize `TargetFilter::GrantingObject` → the live
/// granting object once a granted ability is cloned onto its recipient at a
/// Layer-6 grant (`game/layers.rs` GrantAbility/GrantTrigger). `granter` is the
/// granting object's id (`effect.source_id` at the grant site). Walks the
/// definition's cost, effect, and nested sub/else/mode abilities.
///
/// This is the single concretization point: at parse time the granted body's
/// by-name reference to its granting object is a symbolic `GrantingObject`; here
/// it becomes a concrete `SpecificObject { id }`, so no new runtime resolution
/// logic is required. Host self-references (`SelfRef`) and every other filter
/// are left untouched — the dual binding (granter vs. host) is preserved.
/// Idempotent and re-minted each layer pass (CR 613.1f: Layer 6 ability-adding
/// effects are applied fresh each pass).
///
/// ZONE-MOVE SCOPING (CR 201.5a second sentence + CR 400.7): the snapshot binds
/// the granter's CURRENT battlefield id. It is correct only while the granter is
/// not moved-then-re-referenced within a single resolution. CR 201.5a's second
/// sentence — "if the second ability also moved the first ability's source to a
/// different public zone, the name refers to the object the source became in its
/// new zone" — is not modeled: a granter that leaves the battlefield becomes a
/// new object (CR 400.7), so a later reference would need the new-zone object.
/// No R4 card requires this today: Hammer/Bracelet move as a *cost* (paid and
/// gone before the effect, never re-referenced); Trusty/Razor/Toralf Boomerang
/// return themselves as their final action. A future card that exiles-or-moves
/// its granter and then references it again in the same resolution must extend
/// this to carry the post-move incarnation.
pub(crate) fn concretize_granting_object(def: &mut AbilityDefinition, granter: ObjectId) {
    if let Some(cost) = def.cost.as_mut() {
        concretize_granting_object_in_cost(cost, granter);
    }
    concretize_granting_object_in_effect(def.effect.as_mut(), granter);
    if let Some(sub) = def.sub_ability.as_mut() {
        concretize_granting_object(sub, granter);
    }
    if let Some(els) = def.else_ability.as_mut() {
        concretize_granting_object(els, granter);
    }
    for mode in def.mode_abilities.iter_mut() {
        concretize_granting_object(mode, granter);
    }
}

/// CR 201.5a: Concretize `GrantingObject` inside a granted *trigger's* execute
/// chain (`game/layers.rs` GrantTrigger — e.g. a "you may sacrifice <granter>"
/// action). The trigger's condition/metadata filters never carry a granter
/// by-name self-reference, so only `execute` is walked.
pub(crate) fn concretize_granting_object_in_trigger(
    trigger: &mut TriggerDefinition,
    granter: ObjectId,
) {
    if let Some(execute) = trigger.execute.as_mut() {
        concretize_granting_object(execute, granter);
    }
}

fn concretize_granting_object_in_filter(filter: &mut TargetFilter, granter: ObjectId) {
    match filter {
        TargetFilter::GrantingObject => *filter = TargetFilter::SpecificObject { id: granter },
        TargetFilter::Not { filter } => concretize_granting_object_in_filter(filter, granter),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for f in filters {
                concretize_granting_object_in_filter(f, granter);
            }
        }
        _ => {}
    }
}

fn concretize_granting_object_in_cost(cost: &mut AbilityCost, granter: ObjectId) {
    match cost {
        AbilityCost::Sacrifice(sac) => {
            concretize_granting_object_in_filter(&mut sac.target, granter)
        }
        AbilityCost::Exile {
            filter: Some(f), ..
        }
        | AbilityCost::ReturnToHand {
            filter: Some(f), ..
        }
        | AbilityCost::RemoveCounter {
            target: Some(f), ..
        } => concretize_granting_object_in_filter(f, granter),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for c in costs.iter_mut() {
                concretize_granting_object_in_cost(c, granter);
            }
        }
        AbilityCost::EffectCost { effect } => concretize_granting_object_in_effect(effect, granter),
        _ => {}
    }
}

/// Mirrors the canonical target-bearing `Effect` list
/// (`oracle_effect::rewrite_parent_targets_to_tracked_set`). Effects with no
/// `target` slot cannot carry a `GrantingObject`, so `_ => {}` is complete for
/// the emitting parser paths; any future target-bearing effect that is missed
/// degrades fail-safe (runtime resolves an un-concretized `GrantingObject` to
/// the ability source — the pre-fix host binding), never worse.
fn concretize_granting_object_in_effect(effect: &mut Effect, granter: ObjectId) {
    match effect {
        Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        }
        | Effect::Destroy { target, .. }
        | Effect::GainControl { target }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Counter { target, .. }
        // CR 701.27a: only single-scope Transform carries a targetable slot that
        // can bind a GrantingObject anaphor; the mass (`All`) scope's `target` is a
        // population filter (mirrors the SetTapState Single-gate above).
        | Effect::Transform {
            scope: EffectScope::Single,
            target,
            ..
        }
        // CR 710.4: same single-target-slot shape as `Transform`'s single scope.
        | Effect::FlipPermanent { target, .. }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::PhaseIn { target }
        | Effect::ForceBlock { target, .. }
        | Effect::ForceAttack { target, .. }
        | Effect::CastCopyOfCard { target, .. }
        | Effect::CopyTokenOf { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::ChangeZone { target, .. }
        | Effect::ChangeZoneAll { target, .. }
        | Effect::CastFromZone { target, .. }
        | Effect::Attach { target, .. }
        | Effect::UnattachAll { target, .. } => {
            concretize_granting_object_in_filter(target, granter)
        }
        // Parity with `rewrite_parent_targets_to_tracked_set`: walk both the
        // GenericEffect target and any granted static's `affected` filter.
        Effect::GenericEffect {
            target,
            static_abilities,
            ..
        } => {
            if let Some(t) = target {
                concretize_granting_object_in_filter(t, granter);
            }
            for static_def in static_abilities.iter_mut() {
                if let Some(affected) = static_def.affected.as_mut() {
                    concretize_granting_object_in_filter(affected, granter);
                }
            }
        }
        _ => {}
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
    let per_opponent_fanout_object_targets = if is_per_opponent_target_fanout(ability) {
        match per_opponent_fanout_object_filter(ability) {
            Some(object_filter) if spec.filter == object_filter => {
                if let Some(TargetSlotSpec {
                    filter: TargetFilter::SpecificPlayer { id },
                    ..
                }) = prior_specs.last()
                {
                    if let Some(Some(TargetRef::Player(selected_id))) = selected_slots.last() {
                        if id == selected_id {
                            Some(per_opponent_fanout_legal_object_targets(
                                state,
                                ability,
                                *selected_id,
                            ))
                        } else {
                            Some(Vec::new())
                        }
                    } else {
                        Some(Vec::new())
                    }
                } else {
                    None
                }
            }
            _ => None,
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
    } else if let Some(targets) = per_opponent_fanout_object_targets {
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
            Some(ControllerRef::TargetPlayer) => {
                rewrite_declared_target_player(&spec.filter, ControllerRef::You)
            }
            _ => spec.filter.clone(),
        };

        if target_filter_needs_ability_context(&enumeration_filter) {
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
                Some(aura_id),
            ),
        });
    }

    // CR 601.2c + CR 115.3: within one instance of "target", the same target —
    // object OR player — can't be chosen twice. Remove targets already chosen
    // in prior slots of THIS instance (issue #6459: Scheming Symmetry's
    // "Choose two target players." accepted the same player for both slots
    // because this set was narrowed to `ObjectId` and dropped
    // `TargetRef::Player`). Prior slots of a DIFFERENT instance (separate
    // "target") do not constrain this slot — they may legally reuse the same
    // object or player (CR 601.2c "Destroy target artifact and target land"
    // Example).
    let already_in_instance: std::collections::HashSet<TargetRef> = prior_specs
        .iter()
        .zip(selected_slots)
        .filter(|(prior, _)| prior.instance == spec.instance)
        .filter_map(|(_, sel)| sel.clone())
        .collect();
    legal.retain(|t| !already_in_instance.contains(t));

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
        // CR 608.2d + CR 601.2c: "You may" sub-instructions (Nahiri, the
        // Lithomancer +2 attach) choose whether to perform the action at
        // resolution; their targets are announced only if the controller
        // accepts, not when the loyalty ability is activated.
        || sub.optional
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
) -> Result<(), TargetSlotBuildError> {
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

    if current_legal_targets.is_empty() {
        let mut skipped_slots = selected_slots.clone();
        skipped_slots.push(None);
        let can_skip = slot.optional
            && has_legal_completion(target_slots, constraints, current_slot + 1, &skipped_slots);
        if !can_skip {
            return Err(EngineError::ActionNotAllowed(
                "No legal target combinations available".to_string(),
            ));
        }
        // CR 115.6: Optional slots with no remaining legal targets are
        // auto-skipped — do not surface an interactive step with an empty
        // `current_legal_targets` (the field is omitted on the wire when empty,
        // which crashes clients that read it unconditionally).
        return build_target_selection_progress(
            target_slots,
            constraints,
            current_slot + 1,
            skipped_slots,
        );
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

    if current_legal_targets.is_empty() {
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
        if !can_skip {
            return Err(EngineError::ActionNotAllowed(
                "No legal target combinations available".to_string(),
            ));
        }
        // CR 115.6: Optional slots with no remaining legal targets are
        // auto-skipped — do not surface an interactive step with an empty
        // `current_legal_targets` (the field is omitted on the wire when empty,
        // which crashes clients that read it unconditionally).
        return build_target_selection_progress_for_ability(
            state,
            ability,
            target_slots,
            constraints,
            current_slot + 1,
            skipped_slots,
        );
    }

    // CR 115.10a: "Just because an object or player is being affected by a spell or
    // ability doesn't make that object or player a target … Unless that object or
    // player is identified by the word 'target'…". The `SpecificPlayer` half of a
    // per-opponent target fanout (`collect_per_opponent_target_fanout_specs`) is a
    // BINDER: it is pinned to one player by construction so the following object
    // slot's `ControllerRef::TargetPlayer` scope resolves ("that player's
    // graveyard"). The opponent is affected but never identified by "target" —
    // Diluvian Primordial's "target" attaches to the card. CR 115.1d: only the card
    // slot is an announced target. Announce the pinned player on the controller's
    // behalf and advance, so the first prompt they see is the real choice.
    //
    // Same conjunction `legal_targets_for_selected_slot` already uses to identify a
    // binder (fanout predicate + `SpecificPlayer` filter) — not a looser
    // filter-shape test, so an ordinary pinned-player slot on a non-fanout ability
    // still prompts.
    //
    // CR 115.6: `!slot.optional` — declining an optional slot is a real choice the
    // controller owns ("may allow zero targets to be chosen"), so an optional slot
    // is never a non-choice even with a singleton legal set.
    // CR 601.2c + CR 115.1: `chooser.is_none()` — a slot announced by another player
    // is not the controller's to auto-resolve.
    if is_per_opponent_target_fanout(ability) && !slot.optional && slot.chooser.is_none() {
        if let Some(TargetSlotSpec {
            filter: TargetFilter::SpecificPlayer { id },
            ..
        }) = specs.get(current_slot)
        {
            // `TargetRef` is NOT `Copy`, so `== [pinned]` would MOVE `pinned` into
            // the array literal and make the `push` below E0382. `from_ref`
            // borrows instead, and allocates nothing.
            let pinned = TargetRef::Player(*id);
            if current_legal_targets == std::slice::from_ref(&pinned) {
                let mut bound_slots = selected_slots;
                bound_slots.push(Some(pinned));
                return build_target_selection_progress_for_ability(
                    state,
                    ability,
                    target_slots,
                    constraints,
                    current_slot + 1,
                    bound_slots,
                );
            }
        }
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
    if target_slots[index..].iter().all(|slot| slot.optional) {
        let mut completed_slots = selected_slots.to_vec();
        completed_slots.resize(target_slots.len(), None);
        return validate_selected_slot_prefix(target_slots, &completed_slots, constraints).is_ok();
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
    if target_slots[index..].iter().all(|slot| slot.optional) {
        let mut completed_slots = selected_slots.to_vec();
        completed_slots.resize(target_slots.len(), None);
        return validate_selected_slots_with_specs(
            state,
            ability,
            specs,
            target_slots,
            &completed_slots,
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

        match selected_slot {
            Some(target) => {
                let legal_targets = specs
                    .get(index)
                    .map(|spec| {
                        // CR 601.2c + CR 115.3: `&specs[..index]` (prior specs)
                        // lines up one-for-one with `&selected_slots[..index]`
                        // (prior selections), so validation enforces the same
                        // per-instance distinctness as the offered-set path.
                        legal_targets_for_selected_slot(
                            state,
                            ability,
                            spec,
                            &specs[..index],
                            &selected_slots[..index],
                        )
                    })
                    .unwrap_or_else(|| slot.legal_targets.clone());
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
        let attachment = attachment.clone();
        let target = target.clone();
        assign_attach_attachment_declared_targets(
            state,
            ability,
            &attachment,
            &target,
            targets,
            next_target,
        )?;
        if attach_host_filter_needs_target_slot(&target) {
            if let Some(target) = targets.get(*next_target) {
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
        return Ok(());
    }

    if let Effect::Fight { subject, target } = &ability.effect {
        let mut filters: Vec<&TargetFilter> = Vec::new();
        if fight_subject_needs_target_slot(subject) {
            filters.push(subject);
        }
        filters.push(target);
        for filter in filters {
            // Mirror `collect_target_slots`: a context-ref fighter (SelfRef,
            // ParentTarget, ParentTargetSlot, reciprocal-fight TrackedSet)
            // surfaces no slot, so it consumes no selected target here either —
            // otherwise the assign/slot-gen counts diverge and a valid two-target
            // selection reports a spurious "Missing required target".
            if filter.is_context_ref() {
                continue;
            }
            if let Some(chosen) = targets.get(*next_target) {
                ability.targets.push(chosen.clone());
                *next_target += 1;
            } else if !ability.optional_targeting {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        if let Some(sub_ability) = ability.sub_ability.as_mut() {
            if defers_conditional_target_selection(sub_ability) {
                return Ok(());
            }
            assign_targets_recursive(state, sub_ability, targets, next_target)?;
        }
        return Ok(());
    }

    // CR 601.2c: Assign one target per surfaced mana role slot onto THIS node's
    // own `targets`, base-0, in `collect_target_slots` order (recipient, then
    // count source). PLACEMENT IS LOAD-BEARING: this block sits AHEAD of the
    // four predicate-gated branches below (prevent-damage source, companion
    // target player, target-creature quantity, parent-combat relation). Any of
    // those pushing first would put a non-role target at index 0, and
    // `slot_index(Recipient) == 0` would read it as the recipient — the exact
    // defect this models away. A multi-role mana therefore FORGOES those
    // companion slots; `collect_target_slots` makes the same exclusion by taking
    // the else-if branch, so the two agree by construction, with
    // `minimum_targets_in_chain` and `chain_has_target_sink` gated identically
    // so the reservation arithmetic agrees too. Mirrors `MoveCounters`'
    // structure exactly, including the deferral checks and its own sub-ability
    // descent — there is no shared tail to fall through to.
    if let Some(role) = mana_multi_role(&ability.effect) {
        let surfaced = role.surfaced_filters().count();
        for _ in 0..surfaced {
            if let Some(target) = targets.get(*next_target) {
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
        && !one_sided_fight_source_supplies_quantity_creature(&ability.effect)
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
            // CR 601.2c + issue #3864: An inheriting rider (Solitude's life-gain)
            // surfaces no slot of its own, so it reserves no minimum here. Mirror
            // the filter in `minimum_targets_in_chain`'s `rest` term and the
            // step-by-step `assign_selected_slots_recursive` path.
            let remaining_minimum = ability
                .sub_ability
                .as_deref()
                .filter(|sub| !sub_ability_inherits_parent_creature_target_only(ability, sub))
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
    let inherits_parent_creature_target = ability
        .sub_ability
        .as_ref()
        .is_some_and(|sub| sub_ability_inherits_parent_creature_target_only(ability, sub));
    let parent_creature_target = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(TargetRef::Object(*id)),
        _ => None,
    });
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        if defers_conditional_target_selection(sub_ability) {
            return Ok(());
        }
        if inherits_parent_creature_target {
            if let Some(creature) = parent_creature_target {
                sub_ability.targets.push(creature);
            }
        } else {
            assign_targets_recursive(state, sub_ability, targets, next_target)?;
        }
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

    // CR 601.2c: Mirror of the `mana_multi_role` block in
    // `assign_targets_recursive` against `selected_slots` — one slot consumed
    // per surfaced role filter, recipient first, onto THIS node's own base-0
    // `targets`, ahead of this function's own pre-generic branches so no
    // companion target can land at index 0 and be misread as the recipient.
    if let Some(role) = mana_multi_role(&ability.effect) {
        let surfaced = role.surfaced_filters().count();
        for _ in 0..surfaced {
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
        let attachment = attachment.clone();
        let target = target.clone();
        assign_attach_attachment_selected_slots(
            state,
            ability,
            &attachment,
            selected_slots,
            next_slot,
        )?;
        if attach_host_filter_needs_target_slot(&target) {
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

    if let Effect::Fight { subject, target } = &ability.effect {
        let mut filters: Vec<&TargetFilter> = Vec::new();
        if fight_subject_needs_target_slot(subject) {
            filters.push(subject);
        }
        filters.push(target);
        for filter in filters {
            // Mirror `collect_target_slots` and `assign_targets_recursive`:
            // context-reference fighters resolve from the ability chain, so they
            // consume no interactive target-selection slot.
            if filter.is_context_ref() {
                continue;
            }
            let Some(selected_slot) = selected_slots.get(*next_slot) else {
                return Err(EngineError::InvalidAction(
                    "Missing target selection".to_string(),
                ));
            };
            match selected_slot {
                Some(chosen) => ability.targets.push(chosen.clone()),
                None if ability.optional_targeting => {}
                None => {
                    return Err(EngineError::InvalidAction(
                        "Missing required target".to_string(),
                    ));
                }
            }
            *next_slot += 1;
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
        && !one_sided_fight_source_supplies_quantity_creature(&ability.effect)
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
            // CR 601.2c + issue #3864: A rider that inherits the parent's chosen
            // creature ("exile up to one target creature. That creature's
            // controller gains life equal to its power." — Solitude) surfaces no
            // target slot of its own, so it reserves no minimum here. Filtering
            // it out mirrors `minimum_targets_in_chain`'s own `rest` term; without
            // the filter its phantom `Power{Target}` companion minimum (1) cancels
            // this node's slot, leaving the chosen target unassigned and hard-
            // erroring with "Unused selected target slots".
            let remaining_minimum = ability
                .sub_ability
                .as_deref()
                .filter(|sub| !sub_ability_inherits_parent_creature_target_only(ability, sub))
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
    let inherits_parent_creature_target = ability
        .sub_ability
        .as_ref()
        .is_some_and(|sub| sub_ability_inherits_parent_creature_target_only(ability, sub));
    let parent_creature_target = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(TargetRef::Object(*id)),
        _ => None,
    });
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        if defers_conditional_target_selection(sub_ability) {
            return Ok(());
        }
        if inherits_parent_creature_target {
            if let Some(creature) = parent_creature_target {
                sub_ability.targets.push(creature);
            }
        } else {
            assign_selected_slots_recursive(state, sub_ability, selected_slots, next_slot)?;
        }
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
            TargetSelectionConstraint::SameZoneOwner { zone } => {
                let Some(state) = state else {
                    continue;
                };
                let mut zone_owner = None;
                for target in targets {
                    let TargetRef::Object(object_id) = target else {
                        continue;
                    };
                    let object = state.objects.get(object_id).ok_or_else(|| {
                        EngineError::InvalidAction("Selected object target is missing".into())
                    })?;
                    if object.zone != *zone {
                        return Err(EngineError::InvalidAction(format!(
                            "Selected object targets must be in {zone:?}"
                        )));
                    }
                    match zone_owner {
                        Some(owner) if owner != object.owner => {
                            return Err(EngineError::InvalidAction(
                                "Selected object targets must come from the same zone owner"
                                    .to_string(),
                            ));
                        }
                        Some(_) => {}
                        None => zone_owner = Some(object.owner),
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
                        // CR 202.3d + CR 709.4b: object targets may be off the
                        // stack (cards in a graveyard), where a split card's mana
                        // value is its combined halves; chosen X on the stack.
                        TargetRef::Object(id) => state
                            .objects
                            .get(id)
                            .map(|o| o.effective_mana_value() as i32),
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
    if let Effect::Fight { subject, target } = &ability.effect {
        if fight_subject_needs_target_slot(subject) {
            return true;
        }
        return !matches!(target, TargetFilter::SelfRef | TargetFilter::ParentTarget);
    }

    if let Effect::Attach { attachment, target } = &ability.effect {
        if attach_side_needs_target_slot(attachment, true)
            || attach_side_needs_target_slot(target, false)
        {
            return true;
        }
    }

    // CR 601.2c + CR 115.1: A multi-role mana IS a target sink — it claims one
    // target per surfaced role slot in `assign_targets_recursive`'s dedicated
    // block. This check cannot be left to the generic
    // `extract_target_filter_from_effect` test below: that reads the FIRST
    // DECLARED role filter, so a `Both` whose recipient is a context ref (the
    // subject-predicate shape "That player adds {R} for each card in target
    // opponent's hand") yields `None` there while still surfacing a real
    // count-source slot. Without this arm `assign_targets_in_chain` would
    // early-return with a blanket `ability.targets = targets.to_vec()` and the
    // multi-role assign block would be unreachable. Ungated by
    // `target_choice_timing` to match `collect_target_slots` and
    // `assign_targets_recursive`, both of which gate on `mana_multi_role` alone.
    if mana_multi_role(&ability.effect).is_some() {
        return true;
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
    // CR 601.2c: A multi-role mana forgoes companion / quantity /
    // combat-relation slots — `collect_target_slots` excludes it from all three
    // by taking the else-if branch, and `assign_targets_recursive` early-returns
    // ahead of them. Keep this predicate on the same footing so "is this a sink,
    // and why" cannot disagree with "how many slots does it surface"
    // (`minimum_targets_in_chain` is gated identically).
    //
    // Gating these three off is NOT a no-op, which is why the dedicated
    // multi-role sink check below exists. For a `Both` whose recipient is a
    // context ref, `extract_target_filter_from_effect` reads the FIRST DECLARED
    // filter (the context-ref recipient) and its `!is_context_ref()` filter
    // yields `None` — so the generic check below does NOT fire either, and
    // without an explicit multi-role arm this function would fall through and
    // return `false`. `assign_targets_in_chain` then early-returns with a blanket
    // `ability.targets = targets.to_vec()`, bypassing the multi-role assign block
    // entirely and leaving its slot ordering unenforced.
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
        && mana_multi_role(&ability.effect).is_none()
    {
        return true;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
        && mana_multi_role(&ability.effect).is_none()
    {
        return true;
    }
    if ability.target_choice_timing == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        && mana_multi_role(&ability.effect).is_none()
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

/// CR 115.7a: "each target can be changed only to another legal target." A
/// multi-slot node's replacement targets are submitted positionally, but
/// `legal_new_targets_for_stack_entry` can only return a FLAT union pool
/// (one `Vec<TargetRef>`, no slot structure), so the union alone would let a
/// count-source-legal player be assigned into the recipient slot. This is the
/// seam where slot identity IS available: re-validate each submitted target
/// against the filter of the slot it actually lands in.
///
/// Takes the prompt's `current_targets` and **exempts positions whose submission
/// is unchanged**: CR 115.7d ("the player may leave any number of the targets
/// unchanged, even if those targets would be illegal") licenses this outright for
/// the "choose new targets" scope. CR 115.7a does not grant an equivalent licence
/// — its unchanged-target allowance is conditional ("if a target can't be changed
/// to another legal target") — but it does not need to: it constrains only targets
/// that ARE changed, so a slot already holding its own submission was never
/// changed and "changed only to another legal target" has nothing to bite on.
/// The exemption is therefore correct without a scope parameter, by licence under
/// 115.7d and by non-application under 115.7a.
///
/// CR 115.7d's SECOND sentence — new targets "must not cause any unchanged targets
/// to become illegal" — is vacuous under today's model and is deliberately not
/// enforced here: `validate_targets_for_ability` evaluates each slot's filter
/// against the state and the ability, never against a sibling slot's choice, so no
/// submission can invalidate a neighbour. A future filter that reads sibling slots
/// must revisit this.
///
/// Consumed by `engine::apply_retarget` AND by
/// `ai_support::candidates::retarget_actions`, so the reducer and the AI
/// generator cannot disagree about which submissions are legal.
///
/// Returns `Some(slot_index)` for the first positionally-illegal CHANGED
/// submission. `None` = the submission is slot-legal, every illegal position was
/// left unchanged, or this node declares no per-slot structure this function
/// knows about.
///
/// SCOPE: today this recognizes any node `mana_multi_role` admits — both the
/// two-surfaced-slot `Both` and the one-surfaced-slot context-ref recipient
/// `Both` (surfaced == 1, generic == 0), which is parser-reachable. `Attach`,
/// `MoveCounters`, and `Fight` are multi-slot too and share the same
/// pre-existing flat-pool gap; they are deliberately left on today's behavior
/// so this change's blast radius stays zero for shipping cards. This function is
/// the seam they extend into when that gap is fixed on its own merits.
pub fn retarget_slot_violation(
    state: &GameState,
    ability: &ResolvedAbility,
    current_targets: &[TargetRef],
    new_targets: &[TargetRef],
) -> Option<usize> {
    let role = mana_multi_role(&ability.effect)?;
    role.surfaced_filters()
        .zip(new_targets.iter())
        .enumerate()
        .find_map(|(slot, ((_slot, filter), submitted))| {
            // CR 115.7d: "the player may leave any number of the targets
            // unchanged, even if those targets would be illegal." CR 115.7a says
            // the same thing for the other scope from the other direction: a
            // target is "changed only to another legal target", and a slot
            // already holding its own submission was not changed at all. So a
            // position whose submission equals its current target is exempt
            // under BOTH retarget scopes, which is why this authority needs no
            // scope parameter.
            //
            // `apply_retarget`'s pool-membership stage already exempts exactly
            // these positions (its `All` arm's `continue` on
            // `current_targets.get(idx) == Some(target)`); before this, the
            // per-slot stage re-rejected them, so the one submission CR 115.7d
            // guarantees — leave everything unchanged — was refused for every
            // node `mana_multi_role` admits whose current target had become
            // slot-illegal. The forced seam
            // (`change_targets::forced_retarget_targets`) has always conjoined
            // "changes" with "legal", and its doc already claims parity with
            // this function; this is that same conjunction, here.
            //
            // Index `current_targets` rather than zipping it: a third `.zip`
            // would truncate the scan and silently skip validation for any
            // position beyond `current_targets.len()`, where `get` correctly
            // yields `None` (no current target cannot be "unchanged").
            let changes = current_targets.get(slot) != Some(submitted);
            let illegal = targeting::validate_targets_for_ability(
                state,
                std::slice::from_ref(submitted),
                filter,
                ability,
            )
            .is_empty();
            (changes && illegal).then_some(slot)
        })
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

    // CR 601.2c: A multi-role mana surfaces a DIFFERENT number of slots than the
    // generic `extract_target_filter_from_effect` term below reserves. Add only
    // the excess, so the two terms sum to the true surfaced count. This feeds
    // `remaining_minimum`, the arithmetic deciding how many targets an upstream
    // `multi_target` node may claim — under-reserving lets that node consume
    // this node's targets.
    //
    // The excess is measured against `ManaTargetRole::generic_path_slots()`, the
    // single authority `mana_multi_role`'s gate also consumes — NOT against a
    // hard-coded 1. The generic term contributes 1 only when the FIRST DECLARED
    // role filter is a non-context-ref (that is precisely
    // `extract_target_filter_from_effect`'s `.filter(|t| !t.is_context_ref())`
    // over `target_filter()`). For `Both { recipient: <context ref>, count_source:
    // <real> }` — a shape the gate admits and the parser produces via
    // subject-predicate classification — the generic term is 0, so a hard-coded
    // `surfaced - 1` reserved 0 while collect surfaced 1 and assign consumed 1.
    //
    // `multi_target.is_none()` guard: when a node carries a `multi_target` spec
    // the generic term computes `resolve_multi_target_min` instead of a flat 1,
    // and this excess term's arithmetic would compound against a base it did not
    // predict. The parser cannot produce that shape, so the guard makes an
    // unexercised assumption into a checked one.
    //
    // Mana is deliberately NOT added to the `Attach | MoveCounters` zeroing
    // group below — unlike those two, Mana's generic term is still live and must
    // keep contributing its `generic_path_slots()`; zeroing it would under-reserve
    // by exactly that amount.
    let mana_extra_roles = mana_multi_role(&ability.effect)
        .filter(|_| {
            ability.target_choice_timing == TargetChoiceTiming::Stack
                && !ability.optional_targeting
                && ability.multi_target.is_none()
        })
        .map_or(0, |role| {
            role.surfaced_filters()
                .count()
                .saturating_sub(role.generic_path_slots())
        });

    // CR 109.4: Companion player slot for `ControllerRef::TargetPlayer` filters
    // contributes one required slot (or zero when targeting is optional).
    //
    // CR 601.2c: each companion term is gated off for a multi-role mana, which
    // is excluded from the companion pushes by `collect_target_slots`' else-if
    // branch and by `assign_targets_recursive`'s early return. Without the gate
    // an effect-agnostic companion predicate (notably
    // `ability_needs_companion_target_player_slot`'s `unless_pay` branch, which
    // fires on COST shape, not effect shape) would reserve a slot that neither
    // collect surfaces nor assign consumes, so `remaining_minimum`
    // over-reserves and an upstream `multi_target` sibling is starved of a
    // target it is entitled to.
    let player_companion = if ability.target_choice_timing == TargetChoiceTiming::Stack
        && ability_needs_companion_target_player_slot(ability)
        && mana_multi_role(&ability.effect).is_none()
        && !ability.optional_targeting
    {
        1
    } else {
        0
    };
    let target_creature_quantity_companion = if ability.target_choice_timing
        == TargetChoiceTiming::Stack
        && effect_needs_target_creature_quantity_slot(&ability.effect)
        && !one_sided_fight_source_supplies_quantity_creature(&ability.effect)
        && mana_multi_role(&ability.effect).is_none()
        && !ability.optional_targeting
    {
        1
    } else {
        0
    };
    let parent_target_combat_relation_companion = if ability.target_choice_timing
        == TargetChoiceTiming::Stack
        && effect_needs_parent_target_combat_relation_slot(&ability.effect)
        && mana_multi_role(&ability.effect).is_none()
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
        + mana_extra_roles
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
            .filter(|sub| !sub_ability_inherits_parent_creature_target_only(ability, sub))
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
/// CR 700.2i: For a pawprint points-budget modal, returns whether a chosen
/// index sequence respects the budget: Σ mode_pawprints[idx] ≤ max_choices.
/// Returns `true` unconditionally for non-pawprint modals (`mode_pawprints`
/// empty) so callers can apply it uniformly.
///
/// Indexing `mode_pawprints[i]` is safe at every call site: `validate_modal_indices`
/// runs the per-index range check (`idx < mode_count`, which equals
/// `mode_pawprints.len()` for pawprint modals) before invoking this; the candidate
/// generator and the random path only ever produce indices in `0..mode_count`.
pub fn pawprint_budget_satisfied(modal: &ModalChoice, indices: &[usize]) -> bool {
    if modal.mode_pawprints.is_empty() {
        return true;
    }
    let spent: u32 = indices
        .iter()
        .map(|&i| u32::from(modal.mode_pawprints[i]))
        .sum();
    spent <= modal.max_choices as u32
}

/// CR 700.2d: A player normally can't choose the same mode more than once.
pub fn validate_modal_indices(
    modal: &ModalChoice,
    indices: &[usize],
    unavailable_modes: &[usize],
) -> Result<(), EngineError> {
    // Lower bound (min_choices) applies to both modal kinds.
    if indices.len() < modal.min_choices {
        return Err(EngineError::InvalidAction(format!(
            "Must choose at least {} modes, got {}",
            modal.min_choices,
            indices.len()
        )));
    }
    if modal.mode_pawprints.is_empty() {
        // CR 700.2d: count-capped modal — the upper bound is a mode count.
        if indices.len() > modal.max_choices {
            return Err(EngineError::InvalidAction(format!(
                "Must choose between {} and {} modes, got {}",
                modal.min_choices,
                modal.max_choices,
                indices.len()
            )));
        }
    }
    // CR 700.2i: for pawprint modals the count-cap is REPLACED by the budget gate
    // below (not augmented), so `max_choices` is reinterpreted as the point budget.

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

    // CR 700.2i: budget check runs AFTER the per-index range check guarantees
    // every `idx < mode_count`, so `pawprint_budget_satisfied` can index safely.
    if !pawprint_budget_satisfied(modal, indices) {
        return Err(EngineError::InvalidAction(format!(
            "Pawprint budget exceeded: chosen modes total more than {} {{P}}",
            modal.max_choices
        )));
    }

    Ok(())
}

/// CR 700.2d: Generate all valid mode selection sequences for a modal spell/ability.
pub fn generate_modal_index_sequences(modal: &ModalChoice) -> Vec<Vec<usize>> {
    if !modal.mode_pawprints.is_empty() {
        // CR 700.2i: `max_choices` is the pawprint point budget (Σ weight ≤ budget),
        // not a mode-count cap. Enumerate every budget-legal index sequence whose
        // length meets `min_choices`.
        let mut actions = Vec::new();
        let mut current = Vec::new();
        build_pawprint_budget_sequences(modal, 0, &mut current, &mut actions);
        return actions;
    }

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

fn build_pawprint_budget_sequences(
    modal: &ModalChoice,
    spent: u32,
    current: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    let budget = modal.max_choices as u32;
    if current.len() >= modal.min_choices && spent <= budget {
        out.push(current.clone());
    }
    if spent >= budget {
        return;
    }

    if modal.allow_repeat_modes {
        for idx in 0..modal.mode_count {
            let weight = u32::from(modal.mode_pawprints[idx]);
            if spent + weight > budget {
                continue;
            }
            current.push(idx);
            build_pawprint_budget_sequences(modal, spent + weight, current, out);
            current.pop();
        }
    } else {
        let start_index = if let Some(&last) = current.last() {
            last + 1
        } else {
            0
        };
        for idx in start_index..modal.mode_count {
            let weight = u32::from(modal.mode_pawprints[idx]);
            if spent + weight > budget {
                continue;
            }
            current.push(idx);
            build_pawprint_budget_sequences(modal, spent + weight, current, out);
            current.pop();
        }
    }
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
    use crate::types::ability::{CombatRelation, CombatRelationSubject};

    fn typed_with(props: Vec<FilterProp>) -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            properties: props,
            ..Default::default()
        })
    }

    fn attacking(defender: ControllerRef) -> FilterProp {
        FilterProp::Attacking {
            defender: Some(defender),
        }
    }

    /// V18 — `filter_needs_trigger_source` is PRECISE: it routes the CR 508.5
    /// defending-player anaphor to the context-carrying enumeration door and
    /// leaves every other value on the existing bare door.
    ///
    /// This is the zero-blast-radius proof. `filter::source_controller_ref_player`
    /// resolves `Opponent` via `source.controller`, `EnchantedPlayer` via
    /// `source.attached_to`, and `SourceChosenPlayer` via the source object —
    /// none of them reads `trigger_source` — so leaving the existing corpus
    /// producers on the bare door is behaviour-preserving by construction, not
    /// by luck. Only `DefendingPlayer` reaches
    /// `combat::defending_player_cr508_5`, whose binding rule requires a
    /// `trigger_source` to consult the attack entries at all.
    #[test]
    fn filter_needs_trigger_source_routes_only_the_defending_player_anaphor() {
        // Positive: the new value, bare and under every recursive shape.
        let bare = typed_with(vec![attacking(ControllerRef::DefendingPlayer)]);
        assert!(filter_needs_trigger_source(&bare));
        assert!(filter_needs_trigger_source(&TargetFilter::Or {
            filters: vec![typed_with(vec![]), bare.clone()],
        }));
        assert!(filter_needs_trigger_source(&TargetFilter::And {
            filters: vec![typed_with(vec![]), bare.clone()],
        }));
        assert!(filter_needs_trigger_source(&TargetFilter::Not {
            filter: Box::new(bare.clone()),
        }));
        assert!(filter_needs_trigger_source(&typed_with(vec![
            FilterProp::AnyOf {
                props: vec![FilterProp::Token, attacking(ControllerRef::DefendingPlayer)],
            }
        ])));
        assert!(filter_needs_trigger_source(&typed_with(vec![
            FilterProp::Not {
                prop: Box::new(attacking(ControllerRef::DefendingPlayer)),
            }
        ])));
        // Sibling prop that shares the same `attacking_defender_matches` door.
        assert!(filter_needs_trigger_source(&typed_with(vec![
            FilterProp::AttackedThisTurn {
                defender: Some(ControllerRef::DefendingPlayer),
            }
        ])));

        // Negative: every `Attacking`/`AttackedThisTurn` value that exists in the
        // corpus today must stay on the bare door, unchanged.
        for prop in [
            FilterProp::Attacking { defender: None },
            attacking(ControllerRef::You),
            attacking(ControllerRef::Opponent),
            attacking(ControllerRef::SourceChosenPlayer),
            attacking(ControllerRef::EnchantedPlayer),
            FilterProp::AttackedThisTurn { defender: None },
            FilterProp::AttackedThisTurn {
                defender: Some(ControllerRef::You),
            },
            FilterProp::CombatRelation {
                relation: CombatRelation::BlockingOrBlockedBy,
                subject: CombatRelationSubject::Source,
            },
        ] {
            assert!(
                !filter_needs_trigger_source(&typed_with(vec![prop.clone()])),
                "{prop:?} does not consume trigger_source and must stay on the bare door"
            );
        }

        // And the predicate composes into the existing routing disjunction.
        assert!(target_filter_needs_ability_context(&bare));
    }

    /// V18b — the traversal is DELEGATED, so the anaphor is found at every
    /// nesting depth `filter::filter_contains` knows about, not only at the top
    /// level where the three unlocked cards happen to put it today.
    ///
    /// Revert-failing: restore the hand-rolled `Typed`/`And`/`Or`/`Not` match
    /// with a `_ => false` tail and every row below flips to `false` — each one
    /// then keeps the bare `find_legal_targets` door with `trigger_source:
    /// None`, which is the empty-slot / CR 603.3d removal this predicate exists
    /// to prevent.
    #[test]
    fn filter_needs_trigger_source_descends_every_nesting_variant() {
        use crate::types::ability::PlayerRelation;

        let bare = typed_with(vec![attacking(ControllerRef::DefendingPlayer)]);
        let controls_bare = PlayerFilter::ControlsCount {
            relation: PlayerRelation::All,
            filter: bare.clone(),
            comparator: Comparator::GE,
            count: Box::new(QuantityExpr::Fixed { value: 1 }),
        };

        // The six `TargetFilter`-boxing props, plus the two player-axis
        // crossings. Each is a nesting site `filter_prop_contains` /
        // `player_filter_contains` enumerate exhaustively and the hand-rolled
        // walk skipped entirely.
        for prop in [
            FilterProp::Targets {
                filter: Box::new(bare.clone()),
            },
            FilterProp::TargetsOnly {
                filter: Box::new(bare.clone()),
            },
            FilterProp::CanEnchant {
                target: Box::new(bare.clone()),
            },
            FilterProp::DistinctFrom {
                reference: Box::new(bare.clone()),
            },
            FilterProp::DifferentNameFrom {
                filter: Box::new(bare.clone()),
            },
            FilterProp::SharesQuality {
                quality: SharedQuality::CreatureType,
                reference: Some(Box::new(bare.clone())),
                relation: SharedQualityRelation::default(),
            },
            FilterProp::ControllerMatches {
                player: Box::new(controls_bare.clone()),
            },
        ] {
            assert!(
                filter_needs_trigger_source(&typed_with(vec![prop.clone()])),
                "{prop:?} nests a defending-player anaphor and must route to the \
                 ability-context door"
            );
        }

        // Filter-level nesting variants outside `Typed`/`And`/`Or`/`Not`.
        assert!(filter_needs_trigger_source(
            &TargetFilter::TrackedSetFiltered {
                id: TrackedSetId(1),
                filter: Box::new(bare.clone()),
                caused_by: None,
            }
        ));
        assert!(filter_needs_trigger_source(
            &TargetFilter::ChosenDamageSource {
                filter: Some(Box::new(bare.clone())),
            }
        ));
        assert!(filter_needs_trigger_source(&TargetFilter::PlayerMatching {
            player: Box::new(controls_bare),
        }));

        // Negative control at the same depths: nesting alone does not route.
        assert!(!filter_needs_trigger_source(
            &TargetFilter::ChosenDamageSource {
                filter: Some(Box::new(typed_with(vec![attacking(
                    ControllerRef::Opponent
                )]))),
            }
        ));
    }

    /// V19 — TRIPWIRE. `TypedFilter { controller: Some(DefendingPlayer) }`
    /// (Greatsword of Tyr class) has the IDENTICAL slot-build door bug, and is
    /// deliberately NOT covered here. The deferral is measured, not open-ended:
    /// 97 corpus cards put that shape in a triggered ability's TARGET slot
    /// (versus 3 for the shape fixed here) — see the table on
    /// `filter_needs_trigger_source`. Widening the predicate re-routes all 97
    /// enumerations at once and needs its own multi-attacker fixtures.
    ///
    /// A future pass that widens the predicate must delete this assertion on
    /// purpose — that is the point. It exists so the omission reads as a
    /// decision, not an oversight.
    #[test]
    fn filter_needs_trigger_source_does_not_widen_to_defending_player_controller() {
        let controller_scoped = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::DefendingPlayer),
            ..Default::default()
        });
        assert!(
            !filter_needs_trigger_source(&controller_scoped),
            "deliberately out of scope; see the doc comment on filter_needs_trigger_source"
        );
    }

    /// CR 700.2a / CR 700.2e: `modal_chooser_candidates` is the one authority
    /// both spell announcement and trigger construction read.
    ///
    /// Announcement is single-valued and takes `.first()`, so this row proves
    /// the head of the returned order is byte-identical to the historic
    /// `resolve_modal_chooser` result on both branches, and that the tail — the
    /// part only trigger construction consumes — really is the complete
    /// admitted set rather than that same single value. A regression that
    /// truncates the extraction back to one candidate fails the three-player
    /// length assertion while leaving both head assertions green.
    #[test]
    fn modal_chooser_candidates_are_the_complete_admitted_set_in_apnap_order() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 42);
        state.active_player = PlayerId(0);
        let source = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Modal chooser source".to_string(),
            Zone::Battlefield,
        );

        let mut modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            ..Default::default()
        };

        // CR 700.2a: the controller branch never consults `matches_player_scope`
        // and never admits anyone else, in any seat count.
        modal.chooser = PlayerFilter::Controller;
        assert_eq!(
            modal_chooser_candidates(&state, &modal, PlayerId(1), source),
            vec![PlayerId(1)],
            "the controller branch is the controller alone"
        );

        // CR 700.2e: "an opponent chooses —" with two opponents is a real
        // choice, and APNAP order decides which one announcement would take.
        modal.chooser = PlayerFilter::Opponent;
        let candidates = modal_chooser_candidates(&state, &modal, PlayerId(0), source);
        assert_eq!(
            candidates,
            vec![PlayerId(1), PlayerId(2)],
            "every opponent is admitted, in APNAP order"
        );
        assert_eq!(
            candidates.first().copied(),
            Some(PlayerId(1)),
            "announcement's single-valued head is the first APNAP opponent"
        );

        // Two-player: the same authority collapses to the unambiguous opponent.
        let mut two = GameState::new_two_player(42);
        two.active_player = PlayerId(0);
        let two_source = create_object(
            &mut two,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Modal chooser source".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(
            modal_chooser_candidates(&two, &modal, PlayerId(0), two_source),
            vec![PlayerId(1)]
        );
    }

    /// Matrix rows 5 + 6 — the slot/spec mirror must agree in COUNT **and**
    /// ORDER, and the context-ref skip must agree between the two sites.
    ///
    /// Finding 1: NO existing assertion links `collect_target_slot_specs` to
    /// `collect_target_slots`. The `debug_assert_eq!` in the modal path compares
    /// slots to slots; specs are not in that path. So a divergent mirror fails
    /// SILENTLY as misaligned `TargetInstanceId`s at runtime. Order is asserted
    /// explicitly, not just length — a recipient/count-source swap preserves
    /// length.
    #[test]
    fn mana_role_slots_and_specs_agree_in_count_and_order() {
        use crate::types::ability::{ManaProduction, ManaTargetRole, ManaTargetSlot};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Role Split Source".to_string(),
            Zone::Battlefield,
        );

        let build = |role: ManaTargetRole| {
            ResolvedAbility::new(
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: Some(role),
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        // Case (i): two REAL filters ⇒ two slots, recipient first.
        let both = ManaTargetRole::Both {
            recipient: TargetFilter::Player,
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        };
        let ability = build(both.clone());
        let specs = target_slot_specs(&state, &ability);
        let expected: Vec<&TargetFilter> = both.surfaced_filters().map(|(_, f)| f).collect();
        assert_eq!(
            specs.iter().map(|s| &s.filter).collect::<Vec<_>>(),
            expected,
            "spec ORDER must equal surfaced_filters order (recipient, then count source)"
        );
        assert_eq!(
            build_target_slots(&state, &ability).unwrap().len(),
            specs.len(),
            "slot count and spec count must agree"
        );
        assert_eq!(specs.len(), 2);

        // Case (ii): a CONTEXT-REF recipient surfaces NO slot, so the count
        // source lands at surfaced index 0. This is where naive
        // "recipient == index 0" math breaks, and it also pins context-ref-skip
        // agreement between the two collect sites.
        let ctx_both = ManaTargetRole::Both {
            recipient: TargetFilter::ScopedPlayer,
            count_source: TargetFilter::Player,
        };
        let ability = build(ctx_both.clone());
        let specs = target_slot_specs(&state, &ability);
        assert_eq!(
            specs.iter().map(|s| &s.filter).collect::<Vec<_>>(),
            vec![&TargetFilter::Player],
            "the context-ref recipient surfaces nothing; only the count source does"
        );
        assert_eq!(build_target_slots(&state, &ability).unwrap().len(), 1);
        assert_eq!(ctx_both.slot_index(ManaTargetSlot::CountSource), Some(0));

        // Paired over-application negative (row 7b's spirit at the slot layer):
        // a SINGLE-role mana keeps today's generic single-slot path — the arms
        // are gated on `mana_multi_role`, not on `matches!(effect, Mana { .. })`.
        let single = build(ManaTargetRole::Recipient {
            recipient: TargetFilter::Player,
        });
        assert!(
            mana_multi_role(&single.effect).is_none(),
            "a single-role mana must not enter the explicit multi-slot arms"
        );
        assert_eq!(build_target_slots(&state, &single).unwrap().len(), 1);
        assert_eq!(target_slot_specs(&state, &single).len(), 1);
    }

    /// Build a mana `ResolvedAbility` carrying `role`, controlled by P0.
    fn mana_ability_with_role(
        role: crate::types::ability::ManaTargetRole,
        source: ObjectId,
    ) -> ResolvedAbility {
        use crate::types::ability::ManaProduction;
        ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: Some(role),
            },
            vec![],
            source,
            PlayerId(0),
        )
    }

    /// CR 118.12a: a declared-target unless-payer, the shape that surfaces a
    /// companion player slot from COST shape alone, independent of the effect.
    fn declared_target_payer(payer: TargetFilter) -> UnlessPayModifier {
        UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            payer,
        }
    }

    /// Matrix row 7c — a mana node whose `unless_pay` declares a TARGETED payer.
    /// This is the collision seam between the effect-agnostic companion player
    /// slot (CR 118.12a, driven by COST shape) and the role slots (CR 601.2c,
    /// driven by effect shape). Two independent regressions live here.
    ///
    /// (a) SINGLE-ROLE — `validate_targets_in_chain` must keep the generic,
    ///     companion-aware branch. The companion slot is pushed BEFORE the role
    ///     slot, so `targets == [companion, role]`; a Mana arm keyed on
    ///     `Some(role)` instead of `mana_multi_role` zips the ROLE filter against
    ///     the COMPANION target, fails it, clears `any_legal`, and discards the
    ///     legal companion.
    ///
    /// (b) MULTI-ROLE — `minimum_targets_in_chain` must reserve exactly the
    ///     surfaced slot count. The companion term is gated off by
    ///     `mana_multi_role`, and for a context-ref recipient the GENERIC term is
    ///     also 0 (`extract_target_filter_from_effect` reads the first declared
    ///     filter and drops context refs), so a hard-coded `surfaced - 1` excess
    ///     reserves 0 while collect surfaces 1 and assign consumes 1.
    #[test]
    fn mana_role_targeted_unless_payer_keeps_companion_and_reserves_every_slot() {
        use crate::types::ability::{ManaTargetRole, ManaTargetSlot};

        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Unless-Pay Mana Source".to_string(),
            Zone::Battlefield,
        );

        // ---- (a) SINGLE-ROLE: the companion target must survive validation ----
        let mut single = mana_ability_with_role(
            ManaTargetRole::Recipient {
                // Deliberately OPPONENT-scoped so P0 fails it — that is what makes
                // the mis-zip observable.
                recipient: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            source,
        );
        // "unless target player pays" — controller-inclusive, so P0 is a LEGAL
        // companion while failing the role's opponent-only filter.
        single.unless_pay = Some(declared_target_payer(TargetFilter::Typed(
            TypedFilter::default(),
        )));

        // Reach guards: this fixture really does take the companion + single-role
        // path. Without these the assertion below could pass vacuously.
        assert!(
            ability_needs_companion_target_player_slot(&single),
            "reach guard: the targeted unless-payer must surface a companion slot"
        );
        assert!(
            mana_multi_role(&single.effect).is_none(),
            "reach guard: this is the SINGLE-role path"
        );
        let companion_legal = companion_target_player_legal_targets(&state, &single);
        assert!(
            companion_legal.contains(&TargetRef::Player(PlayerId(0))),
            "reach guard: P0 must be a legal companion payer, got {companion_legal:?}"
        );

        // Slot layout: [companion = P0, role = P1].
        single.targets = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let validated = validate_targets_in_chain(&state, &single);
        assert_eq!(
            validated.targets,
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            "CR 608.2b: the legal companion (P0) and the legal role target (P1) must \
             BOTH survive, in order — an ungated Mana arm drops the companion"
        );

        // Paired positive/negative: an ILLEGAL role target is still pruned, so
        // the assertion above is not just "validation does nothing".
        let mut illegal_role = single.clone();
        illegal_role.targets = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(0)),
        ];
        assert_eq!(
            validate_targets_in_chain(&state, &illegal_role).targets,
            vec![TargetRef::Player(PlayerId(0))],
            "CR 608.2b: P0 is not an opponent, so the ROLE position is pruned while \
             the companion survives"
        );

        // ---- (b) MULTI-ROLE: reservation must equal the surfaced count ----
        // `Both` with a CONTEXT-REF recipient: surfaced == 1, but the generic
        // term contributes 0, so the excess term must contribute the full 1.
        let ctx_role = ManaTargetRole::Both {
            recipient: TargetFilter::ScopedPlayer,
            count_source: TargetFilter::Player,
        };
        let mut ctx = mana_ability_with_role(ctx_role.clone(), source);
        ctx.unless_pay = Some(declared_target_payer(TargetFilter::Typed(
            TypedFilter::default(),
        )));

        // Reach guards.
        assert!(
            mana_multi_role(&ctx.effect).is_some(),
            "reach guard: a context-ref recipient + real count source IS multi-role"
        );
        assert!(
            ability_needs_companion_target_player_slot(&ctx),
            "reach guard: the companion predicate still fires here and must be gated off"
        );
        assert_eq!(
            triggers::extract_target_filter_from_effect(&ctx.effect),
            None,
            "reach guard: the GENERIC reservation term contributes 0 for a \
             context-ref recipient — this is what falsifies a hard-coded `surfaced - 1`"
        );
        assert_eq!(ctx_role.generic_path_slots(), 0);
        assert_eq!(ctx_role.slot_index(ManaTargetSlot::CountSource), Some(0));

        let surfaced = ctx_role.surfaced_filters().count();
        assert_eq!(surfaced, 1);
        assert_eq!(
            build_target_slots(&state, &ctx).unwrap().len(),
            surfaced,
            "collect must surface one slot per surfaced role"
        );
        assert_eq!(
            minimum_targets_in_chain(&state, &ctx),
            surfaced,
            "CR 601.2c: reserved count must equal surfaced count — `surfaced - 1` \
             reserves 0 here and lets an upstream multi_target sibling claim this \
             node's target"
        );

        // Sibling: `Both` with TWO real filters — generic term is 1, excess is 1.
        let two_real = ManaTargetRole::Both {
            recipient: TargetFilter::Player,
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        };
        let two = mana_ability_with_role(two_real.clone(), source);
        assert_eq!(two_real.generic_path_slots(), 1);
        assert_eq!(
            minimum_targets_in_chain(&state, &two),
            two_real.surfaced_filters().count(),
            "reserved count must equal surfaced count for the two-real-filter shape too"
        );
    }

    /// Finding 2 — a multi-role mana must register as a target SINK, or
    /// `assign_targets_in_chain` early-returns with a blanket
    /// `ability.targets = targets.to_vec()` and the dedicated multi-role assign
    /// block is unreachable. For a context-ref recipient the generic
    /// `extract_target_filter_from_effect` sink check yields `None` and the three
    /// companion checks are gated off, so without the explicit arm the function
    /// falls through to `false`.
    ///
    /// Discriminating shape: the mana node is the chain's ONLY sink (case A). A
    /// sub-ability that is itself a sink would supply the sink through the
    /// recursive tail and mask the arm entirely — case B pins that separately, so
    /// the two routes cannot be confused.
    ///
    /// The revert-failing assertions are in case A: `chain_has_target_sink`
    /// itself, and the over-submission rejection. Under the blanket
    /// `ability.targets = targets.to_vec()` early return, a two-target submission
    /// against a one-slot node is silently accepted and the node carries a bogus
    /// second target into resolution instead of being rejected.
    #[test]
    fn multi_role_mana_is_a_target_sink_so_the_chain_distributes_targets() {
        use crate::types::ability::ManaTargetRole;

        let mut state = GameState::new_two_player(11);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chained Mana Source".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        let ctx_role = ManaTargetRole::Both {
            recipient: TargetFilter::ScopedPlayer,
            count_source: TargetFilter::Player,
        };

        // ---- Case A: the mana node is the chain's ONLY sink ----
        let mut solo = mana_ability_with_role(ctx_role.clone(), source);

        // Reach guards: every OTHER route to `true` is inert for this shape, so
        // the explicit multi-role arm is the only thing that can supply the sink.
        assert_eq!(
            triggers::extract_target_filter_from_effect(&solo.effect),
            None,
            "reach guard: the generic sink check does NOT fire for a context-ref recipient"
        );
        assert!(
            mana_multi_role(&solo.effect).is_some(),
            "reach guard: this IS a multi-role mana"
        );
        assert!(
            solo.sub_ability.is_none(),
            "reach guard: no sub-ability, so the recursive tail cannot supply the sink"
        );
        assert!(
            chain_has_target_sink(&solo),
            "a multi-role mana must be recognized as a target sink in its own right"
        );

        // Positive: the one surfaced role slot is claimed.
        assign_targets_in_chain(&state, &mut solo, &[TargetRef::Player(PlayerId(1))])
            .expect("the single surfaced role slot must be assignable");
        assert_eq!(solo.targets, vec![TargetRef::Player(PlayerId(1))]);

        // Negative, paired with the positive above: a node surfacing ONE slot must
        // REJECT a two-target submission. The blanket no-sink copy accepts it and
        // silently carries a bogus second target into resolution.
        let mut over = mana_ability_with_role(ctx_role.clone(), source);
        let err = assign_targets_in_chain(
            &state,
            &mut over,
            &[
                TargetRef::Player(PlayerId(1)),
                TargetRef::Player(PlayerId(0)),
            ],
        )
        .expect_err("a one-slot node must reject a two-target submission");
        assert!(
            matches!(err, EngineError::InvalidAction(ref m) if m == "Unused selected targets"),
            "expected the unused-target rejection, got {err:?}"
        );

        // ---- Case B: a sink-bearing sub-ability, pinned as a SEPARATE route ----
        // Here `chain_has_target_sink` would be true even without the explicit arm
        // (via the recursive tail), so this case pins distribution, not the arm.
        let mut chained = mana_ability_with_role(ctx_role, source);
        // `TargetFilter::Any` on a non-damage effect is a mass-broadcast sentinel
        // that surfaces no slot, so the sub-ability declares a real typed filter.
        let sub = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        assert!(
            chain_has_target_sink(&sub),
            "reach guard: the sub-ability is itself a sink on this route"
        );
        chained.sub_ability = Some(Box::new(sub));

        let targets = vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(victim)];
        assign_targets_in_chain(&state, &mut chained, &targets).expect("assignment must succeed");
        assert_eq!(
            chained.targets,
            vec![TargetRef::Player(PlayerId(1))],
            "the mana node claims exactly its one surfaced role slot, base-0"
        );
        assert_eq!(
            chained
                .sub_ability
                .as_ref()
                .expect("sub-ability preserved")
                .targets,
            vec![TargetRef::Object(victim)],
            "the remainder descends to the sub-ability"
        );
    }

    /// Matrix row 8b — CR 115.7a: "each target can be changed only to another
    /// legal target." A flat `legal_new_targets_for_stack_entry` union pool
    /// cannot express per-slot legality, so `retarget_slot_violation` re-checks
    /// each submission against the filter of the slot it actually lands in.
    #[test]
    fn retarget_slot_violation_rejects_slot_legal_only_for_the_other_slot() {
        use crate::types::ability::ManaTargetRole;

        let mut state = GameState::new_two_player(23);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Retarget Mana Source".to_string(),
            Zone::Battlefield,
        );

        // Recipient: any player. Count source: an OPPONENT of P0 (i.e. P1 only).
        // P0 is therefore legal for slot 0 and ILLEGAL for slot 1.
        let role = ManaTargetRole::Both {
            recipient: TargetFilter::Player,
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        };
        let ability = mana_ability_with_role(role.clone(), source);

        // Reach guard: two surfaced slots with DIFFERENT filters, so the two
        // positions are genuinely discriminable.
        assert!(mana_multi_role(&ability.effect).is_some());
        assert_eq!(role.surfaced_filters().count(), 2);

        // Positive: a slot-legal submission is accepted. Without this the
        // negative below could pass because EVERYTHING is rejected. Both slots
        // genuinely CHANGE against the current targets passed here, so this case
        // proves legality rather than the CR 115.7d unchanged-position exemption.
        assert_eq!(
            retarget_slot_violation(
                &state,
                &ability,
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0)),
                ],
                &[
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
            ),
            None,
            "P0 is a legal recipient and P1 a legal count source"
        );

        // Negative: P0 is in the flat union pool (legal for the recipient slot)
        // but illegal in the COUNT SOURCE slot it was submitted into. Slot 1
        // genuinely changes (P1 -> P0) against the current targets, so the
        // exemption does not apply and the violation must be reported.
        assert_eq!(
            retarget_slot_violation(
                &state,
                &ability,
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(1)),
                ],
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0)),
                ],
            ),
            Some(1),
            "CR 115.7a: P0 is not an opponent, so it is illegal in slot 1 even though \
             the flat union pool contains it"
        );
    }

    /// Matrix row 2d — CR 115.7d: "the player may leave any number of the
    /// targets unchanged, even if those targets would be illegal." A submission
    /// that changes nothing must never be rejected for slot legality, even when
    /// its current target is illegal for the slot it sits in. CR 115.7a licenses
    /// the same exemption for the "change the target(s)" scope: a slot already
    /// holding its own submission was not changed at all.
    #[test]
    fn retarget_slot_violation_exempts_an_unchanged_illegal_target() {
        use crate::types::ability::ManaTargetRole;

        let mut state = GameState::new_two_player(24);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Retarget Mana Source".to_string(),
            Zone::Battlefield,
        );

        // Recipient: any player. Count source: an OPPONENT of P0 (i.e. P1 only).
        // P0 is therefore legal for slot 0 and ILLEGAL for slot 1.
        let role = ManaTargetRole::Both {
            recipient: TargetFilter::Player,
            count_source: TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ),
        };
        let ability = mana_ability_with_role(role.clone(), source);

        // Reach guard: the node is admitted and has two discriminable slots.
        assert!(mana_multi_role(&ability.effect).is_some());
        assert_eq!(role.surfaced_filters().count(), 2);

        // Reach guard: the function still DISCRIMINATES. Slot 1 genuinely
        // changes P1 -> P0 and is illegal there, so a violation is still
        // reported. Without this, the exemption assertion below could pass in a
        // world where this authority stopped rejecting anything at all.
        assert_eq!(
            retarget_slot_violation(
                &state,
                &ability,
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(1)),
                ],
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0)),
                ],
            ),
            Some(1),
            "reach guard: a CHANGED slot-illegal submission is still a violation"
        );

        // CR 115.7d: slot 1 holds P0, which is illegal for the opponent-only
        // count-source slot — but the submission leaves it unchanged, so there
        // is no violation to report.
        assert_eq!(
            retarget_slot_violation(
                &state,
                &ability,
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0)),
                ],
                &[
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0)),
                ],
            ),
            None,
            "CR 115.7d: an unchanged position is exempt from slot legality even \
             though P0 is illegal in slot 1"
        );
    }

    use crate::types::ability::{
        AbilityCost, AbilityKind, AggregateFunction, BounceSelection, CardTypeSetSource,
        CastManaObjectScope, CastManaSpentMetric, Comparator, ContinuousModification,
        ControllerRef, CountScope, CounterTransferMode, DamageChannel, DamageKindFilter, Duration,
        Effect, FilterProp, GameRestriction, LibraryPosition, ModalChoice,
        ModalSelectionConstraint, MultiTargetSpec, ObjectProperty, ObjectScope, ProhibitedActivity,
        PtStat, PtValue, PtValueScope, QuantityExpr, QuantityRef, RestrictionExpiry,
        RestrictionPlayerScope, SearchSelectionConstraint, SharedQuality, SharedQualityRelation,
        StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter, UnlessPayModifier,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        GameState, PayCostKind, StackEntryKind, TargetSelectionConstraint, TargetSelectionSlot,
        WaitingFor,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::keywords::{HexproofFilter, Keyword};
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;
    use crate::types::{FormatConfig, GameAction};

    /// A pawprint points-budget modal mirroring a "Season of …" card: three
    /// modes weighted {P}/{P}{P}/{P}{P}{P}, budget 5, repeats allowed.
    fn season_pawprint_modal() -> ModalChoice {
        ModalChoice {
            min_choices: 0,
            max_choices: 5, // CR 700.2i: the point budget, not a mode count.
            mode_count: 3,
            allow_repeat_modes: true,
            mode_pawprints: vec![1, 2, 3],
            ..Default::default()
        }
    }

    #[test]
    fn pawprint_budget_satisfied_sums_chosen_weights() {
        let modal = season_pawprint_modal();
        // CR 700.2i: Σ weight ≤ budget.
        assert!(pawprint_budget_satisfied(&modal, &[0, 0, 0, 0, 0])); // Σ = 5
        assert!(pawprint_budget_satisfied(&modal, &[2, 0, 0])); // Σ = 5
        assert!(!pawprint_budget_satisfied(&modal, &[2, 2])); // Σ = 6
        assert!(!pawprint_budget_satisfied(&modal, &[2, 0, 0, 0])); // Σ = 6
    }

    #[test]
    fn pawprint_budget_satisfied_is_vacuous_for_non_pawprint_modals() {
        // Empty `mode_pawprints` → always true (callers apply it uniformly).
        let plain = ModalChoice {
            min_choices: 1,
            max_choices: 2,
            mode_count: 3,
            ..Default::default()
        };
        assert!(pawprint_budget_satisfied(&plain, &[0, 1, 2, 2, 2]));
    }

    /// A 4-mode "choose up to X —" modal carrying a `dynamic_max_choices` of
    /// `CostXPaid`, mirroring The Ruinous Wrecking Crew's ETB.
    fn dynamic_cost_x_modal() -> ModalChoice {
        ModalChoice {
            min_choices: 0,
            // CR 700.2 + CR 107.3m: the static placeholder is mode_count; the
            // live cap is resolved from `dynamic_max_choices`.
            max_choices: 4,
            mode_count: 4,
            dynamic_max_choices: Some(QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            }),
            ..Default::default()
        }
    }

    /// Spawn a battlefield source object whose stashed cast {X} (CR 107.3m) is
    /// `x`, returning its id for use as the modal source.
    fn spawn_source_with_cost_x(state: &mut GameState, x: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(999),
            PlayerId(0),
            "Dynamic Modal Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().cost_x_paid = Some(x);
        id
    }

    /// T2 — CR 107.3m + CR 700.2d: `modal_choice_for_player` resolves the
    /// dynamic "choose up to X —" cap from the source's cast {X} and clamps it
    /// to `mode_count`. Reverting the injection in `modal_choice_for_player`
    /// leaves `max_choices` at the static 4 for every X, so the X=3 and X=0
    /// assertions below both fail — this discriminates the resolution value,
    /// not just the clamp.
    #[test]
    fn modal_choice_for_player_resolves_dynamic_cost_x_cap() {
        let modal = dynamic_cost_x_modal();

        // X = 3 → cap 3 (below mode_count, no clamp).
        let mut state = GameState::new_two_player(42);
        let source = spawn_source_with_cost_x(&mut state, 3);
        let effective = modal_choice_for_player(
            &state,
            PlayerId(0),
            source,
            &modal,
            &SpellContext::default(),
        );
        assert_eq!(effective.max_choices, 3, "X=3 resolves to cap 3");

        // X = 0 → cap 0 (player chose X=0; declines all modes).
        let mut state = GameState::new_two_player(42);
        let source = spawn_source_with_cost_x(&mut state, 0);
        let effective = modal_choice_for_player(
            &state,
            PlayerId(0),
            source,
            &modal,
            &SpellContext::default(),
        );
        assert_eq!(effective.max_choices, 0, "X=0 resolves to cap 0");

        // X = 10 → clamped to mode_count 4 (CR 700.2d — can't pick >4 modes).
        let mut state = GameState::new_two_player(42);
        let source = spawn_source_with_cost_x(&mut state, 10);
        let effective = modal_choice_for_player(
            &state,
            PlayerId(0),
            source,
            &modal,
            &SpellContext::default(),
        );
        assert_eq!(
            effective.max_choices, 4,
            "X=10 clamps to mode_count 4, not 10"
        );
    }

    /// T3 regression — a fixed "choose up to two —" modal (no
    /// `dynamic_max_choices`) is untouched by the injection: the resolved cap
    /// equals the static `max_choices`, independent of any source cost {X}.
    #[test]
    fn modal_choice_for_player_skips_injection_for_fixed_cap() {
        let modal = ModalChoice {
            min_choices: 0,
            max_choices: 2,
            mode_count: 4,
            dynamic_max_choices: None,
            ..Default::default()
        };
        let mut state = GameState::new_two_player(42);
        // Even with a large stashed X, the fixed cap must not move.
        let source = spawn_source_with_cost_x(&mut state, 10);
        let effective = modal_choice_for_player(
            &state,
            PlayerId(0),
            source,
            &modal,
            &SpellContext::default(),
        );
        assert_eq!(
            effective.max_choices, 2,
            "fixed cap is unaffected by source cost X"
        );
    }

    #[test]
    fn validate_modal_indices_enforces_pawprint_budget_not_count() {
        let modal = season_pawprint_modal();
        // Five 1-point modes is COUNT 5 > a naive 3-mode cap, but budget-legal.
        assert!(validate_modal_indices(&modal, &[0, 0, 0, 0, 0], &[]).is_ok());
        // Overspend by budget is rejected even though the count (2) is small.
        assert!(validate_modal_indices(&modal, &[2, 2], &[]).is_err());
        // Empty selection is legal (min_choices == 0).
        assert!(validate_modal_indices(&modal, &[], &[]).is_ok());
        // Out-of-range index is caught before the budget indexing.
        assert!(validate_modal_indices(&modal, &[3], &[]).is_err());
    }

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
        // Triggered abilities carry an exact source context; the constraint is
        // gated on it so only triggers (not spells) read the pending event batch.
        ability.set_test_trigger_source_recursive(1, CardId(0));

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

    /// CR 115.1 + CR 118.12a (V3): a declared-target unless-payer surfaces its
    /// own player target slot even when the primary effect references no target
    /// player. Athreos's body is a return-to-hand (`Draw`-shape here stands in
    /// for any no-target-player primary effect); the `Typed { Opponent }` payer
    /// is what makes the slot necessary.
    #[test]
    fn declared_target_unless_payer_needs_companion_player_slot() {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        // Baseline: a no-target-player effect with no unless-pay needs no slot.
        assert!(
            !ability_needs_companion_target_player_slot(&ability),
            "baseline: a Draw effect references no target player"
        );

        // A declared-target opponent payer (Athreos) surfaces the slot.
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        });
        assert!(
            ability_needs_companion_target_player_slot(&ability),
            "a declared-target opponent unless-payer must surface a companion player slot"
        );
    }

    /// CR 118.12a (V3 regression): a bare anaphoric `Player` payer (Tergrid's
    /// Lantern shape) must NOT, by itself, add a companion player slot — the
    /// effect that references the target player owns that slot. With a
    /// no-target-player effect, the anaphoric `Player` payer adds nothing.
    #[test]
    fn anaphoric_player_unless_payer_adds_no_companion_slot() {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Player,
        });
        assert!(
            !ability_needs_companion_target_player_slot(&ability),
            "an anaphoric Player payer must not add a slot on a no-target-player effect"
        );
    }

    /// CR 115.1 + CR 118.12a (V4): the companion player slot for a declared-
    /// target opponent payer offers only the controller's opponents — in a
    /// 3-player game with P0 as controller, that's {P1, P2}, never P0.
    #[test]
    fn declared_target_opponent_companion_slot_lists_opponents_only() {
        let state = GameState::new(FormatConfig::duel_commander(), 3, 7);
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(99),
            PlayerId(0),
        );
        ability.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        });

        let targets = companion_target_player_legal_targets(&state, &ability);
        assert_eq!(
            targets.len(),
            2,
            "exactly the two opponents are legal payers, got {targets:?}"
        );
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(2))));
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(0))),
            "the controller (P0) must never be a legal opponent payer"
        );
    }

    /// CR 102.1 (Test 2b, coerced-attack-punisher): an empty-type-filter
    /// controller-only `ActivePlayer` filter resolves through
    /// `collect_player_targets` to EXACTLY the active player. Reverting the
    /// `Some(ControllerRef::ActivePlayer)` arm is a compile error (exhaustive
    /// match); this test also proves it resolves (not fail-closed) by contrast
    /// with the fail-closed `DefendingPlayer` sibling.
    #[test]
    fn collect_player_targets_active_player_resolves_live() {
        let mut state = GameState::new(FormatConfig::duel_commander(), 3, 7);
        state.active_player = PlayerId(2);
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let active_filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::ActivePlayer));
        assert_eq!(
            collect_player_targets(&state, &ability, &active_filter),
            vec![PlayerId(2)]
        );
        // Sibling: DefendingPlayer stays fail-closed (empty) — proves the new arm
        // is genuinely resolvable, not a fail-closed default.
        let defending =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::DefendingPlayer));
        assert!(collect_player_targets(&state, &ability, &defending).is_empty());
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

    /// CR 115.1 + CR 603.7c: delayed plural returns become `ChangeZoneAll`
    /// with a tracked-set filter, but their snapshotted referent is still not a
    /// chosen target. It must survive validation while it is in exile.
    #[test]
    fn validate_targets_in_chain_preserves_delayed_tracked_set_snapshot_off_battlefield() {
        let format = FormatConfig::duel_commander();
        let mut state = GameState::new(format, 2, 2);
        let victim = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Exile,
        );
        let set_id = TrackedSetId(1);
        state.tracked_object_sets.insert(set_id, vec![victim]);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSetFiltered {
                    id: set_id,
                    filter: Box::new(TargetFilter::ParentTarget),
                    caused_by: None,
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![TargetRef::Object(victim)],
            ObjectId(99),
            PlayerId(0),
        );

        let validated = validate_targets_in_chain(&state, &ability);
        assert_eq!(validated.targets, ability.targets);
        assert!(
            !crate::game::targeting::check_fizzle(
                &flatten_targets_in_chain(&ability),
                &flatten_targets_in_chain(&validated),
            ),
            "a delayed tracked-set return must not fizzle before its mass resolver runs"
        );
    }

    /// CR 608.2b (phase-rs/phase#5449 review): an `Effect::Attach` node whose
    /// `attachment`/`target` are both context-refs (SelfRef/ParentTarget —
    /// neither needs its own target slot) must not have its `.targets` wiped
    /// to `[]` when the node carries MORE entries than its own two operands
    /// consume — those extra entries are propagated through for a downstream
    /// sibling (e.g. a chained `CreateDelayedTrigger` reading the same
    /// `ParentTarget`), not this node's own operands, and must survive
    /// re-validation unchanged.
    #[test]
    fn validate_targets_in_chain_attach_preserves_unclaimed_propagated_targets() {
        let format = FormatConfig::duel_commander();
        let mut state = GameState::new(format, 2, 2);
        let creature = create_object(
            &mut state,
            CardId(0),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );

        // Attach{SelfRef, ParentTarget} — neither operand needs a slot — but
        // `.targets` carries the propagated creature id for a downstream
        // sibling, not for this node's own attachment/target resolution.
        let ability = ResolvedAbility::new(
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(99),
            PlayerId(0),
        );

        let validated = validate_targets_in_chain(&state, &ability);
        assert_eq!(
            validated.targets,
            vec![TargetRef::Object(creature)],
            "an Attach node's un-claimed propagated targets must pass through \
             re-validation unchanged, not be dropped just because neither of \
             this node's own operands needed a target slot"
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

        let result = crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![mazes_end],
            },
        )
        .expect("paying the self-bounce cost should finish activation");

        let moves: Vec<_> = result
            .events
            .iter()
            .filter_map(|event| match event {
                crate::types::events::GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } if *object_id == mazes_end => Some((*from, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(
            moves,
            vec![(Some(Zone::Battlefield), Zone::Hand)],
            "a self-return activation cost must emit exactly one battlefield-to-hand move"
        );

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
                end_cost: _,
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
        sub.distribute = Some(crate::types::game_state::DistributionUnit::Damage);

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
        assert_eq!(
            swapped.distribute,
            Some(crate::types::game_state::DistributionUnit::Damage),
            "swap must preserve the sub-ability's unassigned distribution unit"
        );
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

        // Layer-3 case (PR #6143): when the swapped-in effect has its own
        // declared target, it must use that node's CR 608.2b-validated list.
        // A parent may be empty after its narrower filter rejects every target.
        let empty_parent = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                destination: crate::types::zones::Zone::Graveyard,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        let sub_with_targets = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(10),
            PlayerId(0),
        );

        let swapped_empty_parent = apply_instead_swap(&empty_parent, &sub_with_targets);
        assert_eq!(
            swapped_empty_parent.targets,
            vec![TargetRef::Player(PlayerId(1))],
            "swap must take sub's targets when the parent's were emptied at resolution (CR 608.2b)"
        );

        // The partial case is the same rule: the base filter can retain one
        // target while rejecting another target that remains legal for the
        // broader override. Keeping a nonempty parent list would silently drop
        // the override-only target.
        let partially_validated_parent = ResolvedAbility::new(
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                destination: crate::types::zones::Zone::Graveyard,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(10),
            PlayerId(0),
        );
        let sub_with_broader_targets = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
            },
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            ObjectId(10),
            PlayerId(0),
        );

        let swapped_partial_parent =
            apply_instead_swap(&partially_validated_parent, &sub_with_broader_targets);
        assert_eq!(
            swapped_partial_parent.targets,
            vec![TargetRef::Player(PlayerId(0)), TargetRef::Player(PlayerId(1))],
            "swap must retain every target valid for the override, even when the parent retained a narrower subset"
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
    fn build_resolved_from_def_preserves_unassigned_distribution_unit() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Database,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        def.distribute = Some(crate::types::game_state::DistributionUnit::Damage);

        let resolved = build_resolved_from_def(&def, ObjectId(1), PlayerId(0));

        assert_eq!(resolved.distribute, def.distribute);
        assert!(resolved.distribution.is_none());
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

    /// CR 700.2d: the mode-root stamp is the OCCURRENCE ORDINAL, not the printed
    /// mode index. "If a particular mode is chosen multiple times, the spell is
    /// treated as if that mode appeared that many times in sequence" — so a
    /// repeated mode is two independent instructions and must carry two distinct
    /// ordinals even though both live at the same printed index.
    ///
    /// DISCRIMINATION: key the stamp on `idx` instead of `enumerate()`'s counter
    /// and the `[1, 1]` arm reads `Some(1), Some(1)` — the two occurrences
    /// collapse into one instruction, which is exactly what a mode-boundary
    /// consumer must not see. The `[0, 1, 2]` arm cannot distinguish the two
    /// keyings (index == ordinal there), which is why the repeat arm is here.
    #[test]
    fn build_chained_resolved_stamps_occurrence_ordinals_not_printed_indices() {
        let mode = |effect| AbilityDefinition::new(AbilityKind::Spell, effect);
        let abilities = vec![
            mode(Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            }),
            mode(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }),
            mode(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            }),
        ];

        let distinct =
            build_chained_resolved(&abilities, &[0, 1, 2], ObjectId(1), PlayerId(0)).unwrap();
        let second = distinct.sub_ability.as_deref().expect("mode 1 follows");
        let third = second.sub_ability.as_deref().expect("mode 2 follows");
        assert_eq!(
            (
                distinct.modal_instruction_ordinal,
                second.modal_instruction_ordinal,
                third.modal_instruction_ordinal,
            ),
            (Some(0), Some(1), Some(2)),
            "CR 700.2: every mode root is stamped, including the first"
        );

        // CR 700.2d: Eldrazi Confluence's `allow_repeat_modes` shape.
        let repeated =
            build_chained_resolved(&abilities, &[1, 1], ObjectId(1), PlayerId(0)).unwrap();
        let repeated_second = repeated
            .sub_ability
            .as_deref()
            .expect("the repeated mode occurs twice in sequence");
        assert!(
            matches!(repeated.effect, Effect::Draw { .. })
                && matches!(repeated_second.effect, Effect::Draw { .. }),
            "reach-guard: both occurrences must really be printed mode 1, or the \
             distinct-ordinal assertion below is about the wrong nodes"
        );
        assert_eq!(
            (
                repeated.modal_instruction_ordinal,
                repeated_second.modal_instruction_ordinal,
            ),
            (Some(0), Some(1)),
            "CR 700.2d: two occurrences of ONE printed mode are two instructions. \
             Keying on the printed index would give (Some(1), Some(1))"
        );

        // CR 700.2: the modes are the bulleted options, so "choose up to one"
        // with zero chosen has no instructions at all — it builds a bare
        // `GenericEffect` root, which is not a mode root.
        let none = build_chained_resolved(&abilities, &[], ObjectId(1), PlayerId(0)).unwrap();
        assert_eq!(none.modal_instruction_ordinal, None);
    }

    /// PROVENANCE PIN for `ResolvedAbility::modal_instruction_ordinal`: exactly
    /// ONE non-test writer in the whole engine crate.
    ///
    /// The field's meaning ("this node begins a new CR 700.2 instruction") is only
    /// sound while `build_chained_resolved` — the one function that linearizes
    /// selected modes into a chain — is its only author. A second writer would let
    /// a non-mode-root claim a mode boundary and reset the chain-local tracked-set
    /// identity mid-instruction.
    ///
    /// Classification is by WRITE, not by name occurrence: the identifier also
    /// appears at every exhaustive `ResolvedAbility` literal as `: None` (a
    /// default, not a write) and at each of the eight exhaustive destructures.
    ///
    /// Test regions are excluded by the `#[cfg(test)] mod` boundary, not by
    /// filename — a filename-keyed scan of this crate has produced a wrong census
    /// before (13 "src" sites that were all inside `#[cfg(test)] mod tests`).
    #[test]
    fn modal_instruction_ordinal_has_exactly_one_non_test_writer() {
        // Assembled so this test's own source cannot be counted.
        let needle = format!("modal_instruction_{}", "ordinal");
        let write_forms = [format!("{needle} = "), format!("{needle}: Some(")];
        // POSITIVE CONTROL: `build_chained_resolved`'s OWN other write, five lines
        // from the one under census, in the same non-test region of the same file.
        // If the walk or the `#[cfg(test)]` cut ever stops reaching that function,
        // this reads 0 and the "exactly 1 writer" assertion below would be
        // counterfeit. Counted per file rather than crate-wide: the needle is
        // written 17 times across the crate, a number that drifts with unrelated
        // work, and a crate-wide pin would be a maintenance tax that measures
        // nothing this row cares about.
        let control = format!("sub_link = SubAbilityLink::{}", "SequentialSibling");
        let control_file = "ability_utils.rs";

        let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![src_root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        assert!(files.len() > 100, "reach-guard: the walk found the crate");

        let mut writers: Vec<String> = Vec::new();
        let mut uncut_writers = 0usize;
        let mut control_hits = 0usize;
        for path in &files {
            let text = std::fs::read_to_string(path).expect("read source");
            // Comment halves removed by the shared authority, so a needle written
            // in prose is neither counted nor able to hide a deleted writer.
            let code = crate::source_census::code_lines(&text);
            let lines: Vec<&str> = code.lines().collect();
            // Cut at the FIRST `#[cfg(test)]` THAT IS FOLLOWED BY `mod`, not at
            // the first `#[cfg(test)]` full stop. This crate also `#[cfg(test)]`-
            // guards individual `use` and `fn` items (`ability_utils.rs` has four
            // before its test module), and an earlier draft of this scan located
            // the first marker and then merely CHECKED whether it introduced a
            // module — which made the cut silently degrade to "no cut at all" in
            // exactly the files that need it. Measured: it counted this PR's own
            // `effects/mod.rs` unit-test writers as production writers.
            let end = lines
                .iter()
                .enumerate()
                .position(|(i, line)| {
                    line.trim_start().starts_with("#[cfg(test)]")
                        && lines[i + 1..]
                            .iter()
                            .find(|l| !l.trim().is_empty())
                            .is_some_and(|l| l.trim_start().starts_with("mod "))
                })
                .unwrap_or(lines.len());
            let rel = path.display().to_string();
            for (i, line) in lines.iter().enumerate() {
                if write_forms.iter().any(|f| line.contains(f.as_str())) {
                    uncut_writers += 1;
                    if i < end {
                        writers.push(format!("{rel}: {}", line.trim()));
                    }
                }
                if i < end && rel.ends_with(control_file) {
                    control_hits += line.matches(control.as_str()).count();
                }
            }
        }

        // NEGATIVE CONTROL for the region cut itself: the same scan WITHOUT the
        // `#[cfg(test)] mod` cut must find strictly more writers. Without this
        // arm a broken cut is invisible whenever no test happens to write the
        // field — and then the day one does, this row reds for the wrong reason.
        assert!(
            uncut_writers > writers.len(),
            "NEGATIVE CONTROL: the `#[cfg(test)] mod` cut must actually be \
             excluding test-module writers. uncut={uncut_writers} cut={}",
            writers.len()
        );
        assert_eq!(
            control_hits, 1,
            "POSITIVE CONTROL: `build_chained_resolved`'s `SequentialSibling` write \
             must be visible to this scan, or a zero writer count is counterfeit. \
             control_hits={control_hits}"
        );
        assert_eq!(
            writers.len(),
            1,
            "CR 700.2: `modal_instruction_ordinal` must have exactly one non-test \
             writer (`build_chained_resolved`). writers: {writers:#?}"
        );
        assert!(
            writers[0].contains("ability_utils.rs"),
            "the one writer must be `build_chained_resolved`, got {:?}",
            writers[0]
        );
    }

    #[test]
    fn selected_mode_labels_follow_printed_order_and_preserve_repeats() {
        let labels = selected_mode_labels(
            &["First mode.".to_string(), "Second mode.".to_string()],
            &[1, 0, 1, 2],
        );

        assert_eq!(
            labels,
            ["First mode.", "Second mode.", "Second mode."],
            "labels use printed order, retain repeat selections, and omit missing legacy descriptions",
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
                        only_tag: None,
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

    /// CR 601.2c + CR 115.1: each announced slot carries its OWN link's effect,
    /// not the head link's.
    ///
    /// This is the discriminating assertion for per-frame stamping: verified by
    /// injecting a whole-chain stamp (sub-abilities inheriting the head link's
    /// kind), which makes slot 1 report `DealDamage` and fails here.
    ///
    /// What it does NOT cover, stated so it is not assumed: the
    /// `acc.current_effect_kind = previous_effect_kind` restore in
    /// `collect_target_slots`. Deleting that restore leaves this test green,
    /// because no reachable path in `collect_target_slots_inner` pushes a slot
    /// after recursing into a sub-ability — the one mid-frame recursion
    /// (`is_per_opponent_target_fanout`) returns immediately after. The restore
    /// is defensive symmetry with `current_chooser`, which has the identical
    /// set/restore structure, and would become load-bearing the moment a frame
    /// pushes after recursing. A whole-chain stamp is the bug that
    /// would silently re-label a pump prompt as a damage prompt.
    #[test]
    fn chained_ability_stamps_each_target_slot_with_its_own_links_effect() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chained Source".to_string(),
            Zone::Stack,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // "Deal 2 damage to target creature. Target creature gets +1/+1."
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            source,
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 2, "each link declares one creature target");
        assert_eq!(
            slots[0].effect_kind,
            EffectKind::DealDamage,
            "slot 0 belongs to the damage link"
        );
        assert_eq!(
            slots[1].effect_kind,
            EffectKind::Pump,
            "slot 1 belongs to the pump link — a whole-chain stamp would report DealDamage here"
        );
    }

    /// CR 115.1: the discriminating payload `EffectKind` cannot carry is read
    /// off the effect at construction, where it is symmetric across the spell
    /// and trigger paths.
    ///
    /// Both `build_target_slots` and `build_target_slots_labelled` route
    /// through `collect_target_slots`, so this holds for a triggered ability
    /// too — which is why the read is done here and NOT at projection time,
    /// where `WaitingFor::TriggerTargetSelection` carries no ability reference
    /// and the same effect would be labelled differently depending on whether
    /// it arrived as a spell or a trigger.
    #[test]
    fn slot_detail_carries_the_fact_the_effect_kind_cannot() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Payload Source".to_string(),
            Zone::Stack,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let pump = |power: i32, toughness: i32| {
            ResolvedAbility::new(
                Effect::Pump {
                    power: PtValue::Fixed(power),
                    toughness: PtValue::Fixed(toughness),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        // "+3/+3" and "-3/-3" are the SAME `EffectKind`; only the detail
        // separates them. This is the assertion that fails if the payload read
        // is dropped.
        let buff = build_target_slots(&state, &pump(3, 3)).expect("slots");
        assert_eq!(buff[0].effect_kind, EffectKind::Pump);
        assert_eq!(
            buff[0].effect_detail,
            TargetEffectDetail::Modification(PtDirection::Increase)
        );
        let debuff = build_target_slots(&state, &pump(-3, -3)).expect("slots");
        assert_eq!(debuff[0].effect_kind, EffectKind::Pump);
        assert_eq!(
            debuff[0].effect_detail,
            TargetEffectDetail::Modification(PtDirection::Decrease)
        );

        // A one-sided reduction ("-4/-0") IS directional and must resolve.
        let one_sided = build_target_slots(&state, &pump(-4, 0)).expect("slots");
        assert_eq!(
            one_sided[0].effect_detail,
            TargetEffectDetail::Modification(PtDirection::Decrease),
            "-4/-0 is a debuff, not an undirected modification"
        );

        // A genuinely opposing modification claims no direction rather than
        // guessing one.
        let opposing = build_target_slots(&state, &pump(2, -2)).expect("slots");
        assert_eq!(
            opposing[0].effect_detail,
            TargetEffectDetail::None,
            "+2/-2 is neither a buff nor a debuff"
        );

        // A dynamic magnitude is not knowable at announcement (CR 601.2c fixes
        // targets before X is locked), so it also declines.
        let dynamic = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Variable("X".to_string()),
                toughness: PtValue::Variable("X".to_string()),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            source,
            PlayerId(0),
        );
        assert_eq!(
            build_target_slots(&state, &dynamic).expect("slots")[0].effect_detail,
            TargetEffectDetail::None,
            "an X-sized pump has no statically known direction"
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

    #[test]
    fn deferred_effect_target_traversal_crosses_transparent_links_regardless_of_sub_link() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scrying Source".to_string(),
            Zone::Stack,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let chain = |tail_link| {
            let mut put_counter = ResolvedAbility::new(
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
                vec![],
                source,
                PlayerId(0),
            );
            put_counter.sub_link = tail_link;

            let shuffle = ResolvedAbility::new(
                Effect::Shuffle {
                    target: TargetFilter::Controller,
                },
                vec![],
                source,
                PlayerId(0),
            )
            .sub_ability(put_counter);
            let change_zone = ResolvedAbility::new(
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
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                vec![],
                source,
                PlayerId(0),
            )
            .sub_ability(shuffle);

            ResolvedAbility::new(
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                source,
                PlayerId(0),
            )
            .sub_ability(change_zone)
        };

        for link in [
            SubAbilityLink::ContinuationStep,
            SubAbilityLink::SequentialSibling,
        ] {
            let ability = chain(link);
            let slots = build_target_slots(&state, &ability)
                .expect("transparent deferred-effect links should surface the target");
            assert_eq!(slots.len(), 1);
            assert!(slots[0]
                .legal_targets
                .contains(&TargetRef::Object(creature)));
            assert_eq!(target_slot_specs(&state, &ability).len(), 1);
            assert!(chain_has_target_sink(&ability));
            assert_eq!(minimum_targets_in_chain(&state, &ability), 1);
            validate_selected_targets_for_ability(
                &state,
                &ability,
                &slots,
                &[TargetRef::Object(creature)],
                &[],
            )
            .expect("the deferred-effect tail creature target should validate");

            let mut compact_assigned = ability.clone();
            assign_targets_in_chain(
                &state,
                &mut compact_assigned,
                &[TargetRef::Object(creature)],
            )
            .expect("compact assignment must reach the deferred-effect tail");
            assert_eq!(
                compact_assigned
                    .sub_ability
                    .as_deref()
                    .and_then(|change_zone| change_zone.sub_ability.as_deref())
                    .and_then(|shuffle| shuffle.sub_ability.as_deref())
                    .unwrap()
                    .targets,
                vec![TargetRef::Object(creature)]
            );

            let mut selected_assigned = ability;
            assign_selected_slots_in_chain(
                &state,
                &mut selected_assigned,
                &[Some(TargetRef::Object(creature))],
            )
            .expect("selected-slot assignment must reach the deferred-effect tail");
            assert_eq!(
                selected_assigned
                    .sub_ability
                    .as_deref()
                    .and_then(|change_zone| change_zone.sub_ability.as_deref())
                    .and_then(|shuffle| shuffle.sub_ability.as_deref())
                    .unwrap()
                    .targets,
                vec![TargetRef::Object(creature)]
            );
        }

        let mut when_you_do = chain(SubAbilityLink::ContinuationStep);
        when_you_do
            .sub_ability
            .as_deref_mut()
            .and_then(|change_zone| change_zone.sub_ability.as_deref_mut())
            .and_then(|shuffle| shuffle.sub_ability.as_deref_mut())
            .unwrap()
            .condition = Some(AbilityCondition::WhenYouDo);
        let mut resolution_timing = chain(SubAbilityLink::SequentialSibling);
        resolution_timing
            .sub_ability
            .as_deref_mut()
            .and_then(|change_zone| change_zone.sub_ability.as_deref_mut())
            .and_then(|shuffle| shuffle.sub_ability.as_deref_mut())
            .unwrap()
            .target_choice_timing = TargetChoiceTiming::Resolution;

        for ability in [when_you_do, resolution_timing] {
            assert!(build_target_slots(&state, &ability)
                .expect("deferred conditional target traversal should build")
                .is_empty());
            assert!(target_slot_specs(&state, &ability).is_empty());
            assert!(!chain_has_target_sink(&ability));
            assert_eq!(minimum_targets_in_chain(&state, &ability), 0);

            let mut compact_assigned = ability.clone();
            assign_targets_in_chain(&state, &mut compact_assigned, &[])
                .expect("empty compact assignment should leave deferred targets unchosen");
            assert!(compact_assigned
                .sub_ability
                .as_deref()
                .and_then(|change_zone| change_zone.sub_ability.as_deref())
                .and_then(|shuffle| shuffle.sub_ability.as_deref())
                .unwrap()
                .targets
                .is_empty());

            let mut selected_assigned = ability;
            assign_selected_slots_in_chain(&state, &mut selected_assigned, &[])
                .expect("empty selected-slot assignment should leave deferred targets unchosen");
            assert!(selected_assigned
                .sub_ability
                .as_deref()
                .and_then(|change_zone| change_zone.sub_ability.as_deref())
                .and_then(|shuffle| shuffle.sub_ability.as_deref())
                .unwrap()
                .targets
                .is_empty());
        }
    }

    /// CR 608.2c + CR 115.1: Arcum Dagsson / #4678 — "Target artifact creature's
    /// controller sacrifices it. …". The ability must SURFACE a required target
    /// slot for the artifact creature (before the fix it compiled to a targetless
    /// `Sacrifice{ParentTarget}` and activated with no target). Only artifact
    /// creatures are legal; a plain creature is not.
    #[test]
    fn build_target_slots_target_controller_sacrifices_it_requires_object_target() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Arcum Dagsson".to_string(),
            Zone::Battlefield,
        );
        // Opponent-controlled artifact creature (a legal target).
        let art_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Ornithopter".to_string(),
            Zone::Battlefield,
        );
        {
            let types = &mut state.objects.get_mut(&art_creature).unwrap().card_types;
            types.core_types.push(CoreType::Artifact);
            types.core_types.push(CoreType::Creature);
        }
        // A plain (non-artifact) creature — must NOT be a legal target.
        let plain_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let parsed = crate::parser::oracle::parse_oracle_text(
            "{T}: Target artifact creature's controller sacrifices it. That player may search their library for a noncreature artifact card, put it onto the battlefield, then shuffle.",
            "Arcum Dagsson",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Artificer".to_string()],
        );
        let def = parsed.abilities.first().expect("activated ability parsed");
        let ability = build_resolved_from_def(def, source, PlayerId(0));

        let slots = build_target_slots(&state, &ability).unwrap();
        assert_eq!(
            slots.len(),
            1,
            "exactly one object target slot for the artifact creature, got {slots:?}",
        );
        assert!(
            !slots[0].optional,
            "the artifact-creature target is required"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(art_creature)),
            "the opponent's artifact creature must be a legal target",
        );
        assert!(
            !slots[0]
                .legal_targets
                .contains(&TargetRef::Object(plain_creature)),
            "a non-artifact creature must NOT be a legal target",
        );
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
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
    fn generate_modal_index_sequences_respects_pawprint_budget() {
        let modal = season_pawprint_modal();
        let sequences = generate_modal_index_sequences(&modal);

        assert!(
            sequences.contains(&Vec::<usize>::new()),
            "min_choices=0 permits choosing no modes"
        );
        assert!(
            sequences.contains(&vec![0, 0, 0, 0, 0]),
            "five 1-point picks must fit the 5-point budget"
        );
        assert!(
            !sequences.contains(&vec![2, 2, 2]),
            "three weight-3 picks (Σ=9) must not be generated for a budget of 5"
        );
        assert!(
            sequences
                .iter()
                .all(|indices| pawprint_budget_satisfied(&modal, indices)),
            "every generated sequence must satisfy the pawprint budget gate"
        );
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
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

    #[test]
    fn target_selection_filters_cards_outside_single_graveyard_owner() {
        let mut state = GameState::new_two_player(42);
        let p0_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 A".to_string(),
            Zone::Graveyard,
        );
        let p0_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "P0 B".to_string(),
            Zone::Graveyard,
        );
        let p1_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "P1 A".to_string(),
            Zone::Graveyard,
        );
        let p0_battlefield = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "P0 Battlefield".to_string(),
            Zone::Battlefield,
        );

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Card).properties(vec![
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 }));

        let target_slots = build_target_slots(&state, &ability).expect("target slots");
        let constraints = [TargetSelectionConstraint::SameZoneOwner {
            zone: Zone::Graveyard,
        }];
        let progress =
            begin_target_selection_for_ability(&state, &ability, &target_slots, &constraints)
                .expect("selection starts");

        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &target_slots,
            &constraints,
            &progress,
            Some(TargetRef::Object(p0_a)),
        )
        .expect("first target accepted") else {
            panic!("expected second target prompt");
        };

        assert!(progress
            .current_legal_targets
            .contains(&TargetRef::Object(p0_b)));
        assert!(!progress
            .current_legal_targets
            .contains(&TargetRef::Object(p1_a)));
        assert!(!progress
            .current_legal_targets
            .contains(&TargetRef::Object(p0_battlefield)));

        assert!(
            validate_target_constraints(
                Some(&state),
                &[TargetRef::Object(p0_a), TargetRef::Object(p0_b)],
                &constraints,
                Some(&ability),
            )
            .is_ok(),
            "same-owner graveyard pair must satisfy SameZoneOwner"
        );
        assert!(
            validate_target_constraints(
                Some(&state),
                &[TargetRef::Object(p0_a), TargetRef::Object(p1_a)],
                &constraints,
                Some(&ability),
            )
            .is_err(),
            "different graveyard owners must fail SameZoneOwner"
        );
        assert!(
            validate_target_constraints(
                Some(&state),
                &[TargetRef::Object(p0_a), TargetRef::Object(p0_battlefield)],
                &constraints,
                Some(&ability),
            )
            .is_err(),
            "objects outside the constrained zone must fail SameZoneOwner"
        );
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(0))],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
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
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(42))],
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
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
    fn choose_target_for_ability_skip_completes_optional_multi_target_tail() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let source = create_creature(&mut state, PlayerId(0), CardId(1), "Source");
        let first = create_creature(&mut state, PlayerId(0), CardId(2), "First");
        let second = create_creature(&mut state, PlayerId(0), CardId(3), "Second");

        let ability = up_to_n_target_creatures(source, PlayerId(0), 3);
        let target_slots = build_target_slots(&state, &ability).expect("target slots");
        let progress = begin_target_selection_for_ability(&state, &ability, &target_slots, &[])
            .expect("selection should start");
        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &target_slots,
            &[],
            &progress,
            Some(TargetRef::Object(first)),
        )
        .expect("first target should be accepted") else {
            panic!("expected target selection to continue");
        };

        let TargetSelectionAdvance::Complete(selected_slots) =
            choose_target_for_ability(&state, &ability, &target_slots, &[], &progress, None)
                .expect("skipping the optional tail should complete")
        else {
            panic!("expected skip to complete the optional target run");
        };

        assert_eq!(
            selected_slots,
            vec![Some(TargetRef::Object(first)), None, None,]
        );
        assert!(
            !selected_slots.contains(&Some(TargetRef::Object(second))),
            "skip must not auto-pick later legal targets"
        );
    }

    /// CR 115.1 + CR 115.6: After the "controlled by different players"
    /// constraint exhausts every controller, remaining optional multi-target
    /// slots must auto-skip instead of pausing with an empty
    /// `current_legal_targets` (issue #4242 / Lagrella).
    #[test]
    fn choose_target_auto_skips_optional_tail_when_constraint_exhausted() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let source = create_creature(&mut state, PlayerId(0), CardId(1), "Lagrella");
        let p0_creature = create_creature(&mut state, PlayerId(0), CardId(2), "Ally");
        let p1_creature = create_creature(&mut state, PlayerId(1), CardId(3), "Opp");

        let mut ability = up_to_n_target_creatures(source, PlayerId(0), 3);
        ability.target_constraints = vec![TargetSelectionConstraint::DifferentObjectControllers];
        let target_slots = build_target_slots(&state, &ability).expect("target slots");
        let constraints = ability.target_constraints.clone();

        let progress =
            begin_target_selection_for_ability(&state, &ability, &target_slots, &constraints)
                .expect("selection should start");

        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &target_slots,
            &constraints,
            &progress,
            Some(TargetRef::Object(p1_creature)),
        )
        .expect("first target should be accepted") else {
            panic!("expected target selection to continue after first pick");
        };

        let TargetSelectionAdvance::Complete(selected_slots) = choose_target_for_ability(
            &state,
            &ability,
            &target_slots,
            &constraints,
            &progress,
            Some(TargetRef::Object(p0_creature)),
        )
        .expect("second target should auto-complete the optional tail") else {
            panic!("expected auto-skip to complete after the last controller is used");
        };

        assert_eq!(
            selected_slots,
            vec![
                Some(TargetRef::Object(p1_creature)),
                Some(TargetRef::Object(p0_creature)),
                None,
            ]
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
                end_cost: None,
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
                end_cost: None,
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
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
                mana_spend_permission: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );

        let err = build_target_slots(&state, &ability).expect_err("target slot should be required");

        assert!(matches!(err, EngineError::ActionNotAllowed(_)));
    }

    // Nettling Imp / Norritt / Arcum's Whistle class: "target creature the active
    // player has controlled continuously since the beginning of the turn". Proves
    // BOTH predicates gate legality independently — the active-player controller
    // scope AND the continuity flag — via three fixtures, only one of which is a
    // legal target. Positive full-set assertion (not a bare negative) so neither
    // hostile fixture can be excluded vacuously by an upstream short-circuit.
    #[test]
    fn build_target_slots_active_player_controlled_continuously_since_turn_began() {
        use crate::types::ability::{ControllerRef, FilterProp};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        // Active player (CR 102.1) is PlayerId(0) at game start.
        assert_eq!(state.active_player, PlayerId(0));

        // Legal: active player's control, no summoning sickness (create_object
        // does not set the flag — a "pre-existing" battlefield creature).
        let continuous_active = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Continuous Active".to_string(),
            Zone::Battlefield,
        );
        // Illegal via continuity: active player's control, but summoning-sick.
        let fresh_active = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Fresh Active".to_string(),
            Zone::Battlefield,
        );
        // Illegal via controller: continuous control, but by the non-active player.
        let continuous_nonactive = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Continuous Nonactive".to_string(),
            Zone::Battlefield,
        );
        for creature in [continuous_active, fresh_active, continuous_nonactive] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        state.objects.get_mut(&fresh_active).unwrap().summoning_sick = true;

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::ActivePlayer)
                        .properties(vec![FilterProp::ControlledContinuouslySinceTurnBegan]),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 1);
        assert_eq!(
            slots[0].legal_targets,
            vec![TargetRef::Object(continuous_active)]
        );
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                excess: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

        // CR 115.10a: the pinned binder is announced by the engine, so the walk
        // opens on the first object slot with the binder already bound.
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(progress.current_slot, 1);
        assert_eq!(
            progress.selected_slots,
            vec![Some(TargetRef::Player(PlayerId(1)))]
        );
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
        assert_eq!(
            progress.selected_slots,
            vec![Some(TargetRef::Player(PlayerId(1)))],
            "the auto-announced binder still bypasses player targeting protection"
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_creature)]
        );
    }

    #[test]
    fn dismantling_wave_fanout_offer_excludes_regular_hexproof_permanent() {
        let mut state = GameState::new_two_player(42);
        let source = create_dismantling_wave_source(&mut state);
        let hexproof_artifact = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(2),
            "Hexproof Artifact Creature",
            &[CoreType::Artifact, CoreType::Creature],
        );
        state
            .objects
            .get_mut(&hexproof_artifact)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let unprotected_enchantment = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(3),
            "Unprotected Enchantment",
            &[CoreType::Enchantment],
        );
        let ability = dismantling_wave_fanout_ability(source);

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(unprotected_enchantment)]
        );
        assert!(!slots[1]
            .legal_targets
            .contains(&TargetRef::Object(hexproof_artifact)));

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(unprotected_enchantment)]
        );
    }

    #[test]
    fn per_opponent_fanout_revalidation_drops_regular_hexproof_from_spell_controller() {
        let mut state = GameState::new_two_player(42);
        let source = create_dismantling_wave_source(&mut state);
        let hexproof_artifact = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(2),
            "Hexproof Artifact",
            &[CoreType::Artifact],
        );
        state
            .objects
            .get_mut(&hexproof_artifact)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let mut ability = dismantling_wave_fanout_ability(source);

        assign_targets_in_chain(
            &state,
            &mut ability,
            &[
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(hexproof_artifact),
            ],
        )
        .expect("assignment should preserve pair structure");

        let validated = validate_targets_in_chain(&state, &ability);
        assert!(
            validated.targets.is_empty(),
            "hexproof object is illegal from the spell controller and must drop"
        );
    }

    #[test]
    fn per_opponent_fanout_excludes_matching_hexproof_from_source_quality() {
        let mut state = GameState::new_two_player(42);
        let source = create_dismantling_wave_source(&mut state);
        let hexproof_from_white = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(2),
            "Hexproof From White Artifact",
            &[CoreType::Artifact],
        );
        state
            .objects
            .get_mut(&hexproof_from_white)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(
                ManaColor::White,
            )));
        let unprotected_artifact = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(3),
            "Unprotected Artifact",
            &[CoreType::Artifact],
        );
        let ability = dismantling_wave_fanout_ability(source);

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(unprotected_artifact)]
        );
        assert!(!slots[1]
            .legal_targets
            .contains(&TargetRef::Object(hexproof_from_white)));
    }

    #[test]
    fn per_opponent_fanout_ignore_hexproof_bypasses_regular_hexproof() {
        let mut state = GameState::new_two_player(42);
        let source = create_dismantling_wave_source(&mut state);
        let hexproof_artifact = create_permanent_with_types(
            &mut state,
            PlayerId(1),
            CardId(2),
            "Hexproof Artifact",
            &[CoreType::Artifact],
        );
        state
            .objects
            .get_mut(&hexproof_artifact)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::IgnoreHexproof,
            }],
            None,
        );
        let ability = dismantling_wave_fanout_ability(source);

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(hexproof_artifact)]
        );
    }

    #[test]
    fn per_opponent_fanout_later_sub_ability_target_uses_normal_recompute() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let opponent_one_creature = create_creature(&mut state, PlayerId(1), CardId(1), "Opp One");
        let opponent_two_creature = create_creature(&mut state, PlayerId(2), CardId(2), "Opp Two");
        let ability = per_opponent_gain_control_ability().sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 5);

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Object(opponent_one_creature)),
        )
        .expect("first object slot should be accepted") else {
            panic!("expected second object slot");
        };
        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Object(opponent_two_creature)),
        )
        .expect("second object slot should advance to sub-ability target") else {
            panic!("expected trailing sub-ability target slot");
        };

        assert_eq!(progress.current_slot, 4);
        assert!(
            progress
                .current_legal_targets
                .contains(&TargetRef::Player(PlayerId(1))),
            "trailing non-fanout target slot should fall through to normal target recompute"
        );
    }

    /// Shared board for the two `chooser` rows: a MANDATORY per-opponent fanout
    /// with exactly one opponent, who controls exactly one legal permanent. The
    /// two rows are separate `#[test]` functions with their own verdicts and
    /// differ only in the binder slot's `chooser`.
    fn per_opponent_binder_chooser_fixture() -> (
        GameState,
        ResolvedAbility,
        Vec<TargetSelectionSlot>,
        ObjectId,
    ) {
        let mut state = GameState::new_two_player(42);
        let opponent_creature = create_creature(&mut state, PlayerId(1), CardId(1), "Opp One");
        let ability = per_opponent_gain_control_ability();
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        (state, ability, slots, opponent_creature)
    }

    /// CR 601.2c + CR 115.1: a slot another player announces is never the
    /// controller's to auto-resolve, even when it is a structurally pinned
    /// per-opponent binder. Paired negative for
    /// `binder_slot_without_a_chooser_is_autofilled`.
    #[test]
    fn binder_slot_with_a_foreign_chooser_is_not_autofilled() {
        let (state, ability, mut slots, _) = per_opponent_binder_chooser_fixture();
        assert!(is_per_opponent_target_fanout(&ability));
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert!(
            !slots[0].optional,
            "the fixture must satisfy every conjunct except `chooser`"
        );

        slots[0].chooser = Some(PlayerId(1));
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_slot, 0,
            "a chooser-stamped binder remains an announced step for that player"
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    /// CR 115.10a: the same fixture with no foreign chooser — the pinned
    /// opponent is announced by the engine and the walk opens on the object
    /// slot. Sibling positive for
    /// `binder_slot_with_a_foreign_chooser_is_not_autofilled`.
    #[test]
    fn binder_slot_without_a_chooser_is_autofilled() {
        let (state, ability, slots, opponent_creature) = per_opponent_binder_chooser_fixture();
        assert_eq!(slots.len(), 2);
        assert!(slots[0].chooser.is_none());
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_slot, 1,
            "the pinned binder is announced on the controller's behalf"
        );
        assert_eq!(
            progress.selected_slots,
            vec![Some(TargetRef::Player(PlayerId(1)))]
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_creature)]
        );
    }

    /// CR 603.3d: a REQUIRED binder with no legal player still removes the
    /// ability from the stack. The auto-fill block is deliberately ordered
    /// after the empty-legal-set block, so this path is byte-identical to its
    /// pre-auto-fill behavior.
    #[test]
    fn binder_with_no_legal_player_still_reports_no_legal_combinations() {
        let mut state = GameState::new_two_player(42);
        create_creature(&mut state, PlayerId(1), CardId(1), "Opp One");
        let ability = per_opponent_gain_control_ability();

        let mut slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert!(!slots[0].optional);

        // CR 800.4: the sole opponent leaves the game after the slots were
        // built. The fanout then yields no spec at all, and the stale required
        // binder slot has no legal player left.
        state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(1))
            .expect("opponent seat")
            .is_eliminated = true;
        slots[0].legal_targets.clear();

        let error = begin_target_selection_for_ability(&state, &ability, &slots, &[])
            .expect_err("a required binder with no legal player must not be auto-filled");
        assert!(
            matches!(&error, EngineError::ActionNotAllowed(message)
                if message == "No legal target combinations available"),
            "expected the CR 603.3d no-legal-combination error, got {error:?}"
        );
    }

    /// CR 115.10a does NOT apply to an ordinary singleton target: "destroy
    /// target creature" identifies the creature by the word "target", so the
    /// controller still announces it even when exactly one is legal. Negative
    /// control for the class boundary — the general "any mandatory singleton
    /// auto-fills" rule is explicitly not implemented.
    #[test]
    fn a_lone_legal_creature_for_destroy_target_creature_still_prompts() {
        let mut state = GameState::new_two_player(42);
        let lone_creature = create_creature(&mut state, PlayerId(1), CardId(1), "Lone Creature");
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );
        assert!(!is_per_opponent_target_fanout(&ability));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].optional);
        assert!(slots[0].chooser.is_none());
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_slot, 0,
            "the word `target` attaches to the creature, so the controller announces it"
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(lone_creature)]
        );

        // Reach guard: the same shape with a genuine choice also prompts at
        // slot 0, so the singleton assertion above is not the only branch.
        let second_creature =
            create_creature(&mut state, PlayerId(1), CardId(2), "Second Creature");
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(progress.current_slot, 0);
        assert!(progress
            .current_legal_targets
            .contains(&TargetRef::Object(second_creature)));
    }

    /// CR 115.10a is scoped to the per-opponent fanout BINDER, not to every
    /// pinned-player slot. A mandatory `SpecificPlayer` slot on a NON-fanout
    /// ability is a real announced target and still prompts — this is the row
    /// that separates the adopted `is_per_opponent_target_fanout` gate from a
    /// filter-shape-only predicate.
    #[test]
    fn mandatory_specific_player_slot_on_a_non_fanout_ability_still_prompts() {
        let state = GameState::new(FormatConfig::standard(), 3, 42);
        let pinned = PlayerId(1);
        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::SpecificPlayer { id: pinned },
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );
        assert!(!ability.optional_targeting);
        assert!(ability.multi_target.is_none());
        assert!(!is_per_opponent_target_fanout(&ability));

        // Reach guard: the fixture satisfies every conjunct of the auto-fill
        // guard EXCEPT the fanout gate — mandatory, unchoosered, singleton.
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 1);
        assert!(!slots[0].optional);
        assert!(slots[0].chooser.is_none());
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(pinned)]);

        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");
        assert_eq!(
            progress.current_slot, 0,
            "a pinned-player slot outside the fanout class is still announced by the controller"
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Player(pinned)]
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

    /// CR 115.1a + CR 108.3: The sole nonbattlefield per-opponent fanout class
    /// binds each graveyard card to its immediately preceding opponent target.
    /// The positive paired cards prove the path is reachable; a wrong-owner card
    /// and a battlefield lookalike prove neither owner nor zone is widened.
    #[test]
    fn per_opponent_graveyard_fanout_pairs_only_each_opponents_typed_card() {
        use crate::types::ability::{CardPlayMode, CastFromZoneDriver};

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let add_instant = |state: &mut GameState, owner, zone, card_id| {
            let id = create_object(
                state,
                CardId(card_id),
                owner,
                format!("Instant {card_id}"),
                zone,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Instant);
            id
        };
        let p1_graveyard = add_instant(&mut state, PlayerId(1), Zone::Graveyard, 1);
        let p2_graveyard = add_instant(&mut state, PlayerId(2), Zone::Graveyard, 2);
        let _wrong_owner = add_instant(&mut state, PlayerId(0), Zone::Graveyard, 3);
        let _battlefield_lookalike = add_instant(&mut state, PlayerId(1), Zone::Battlefield, 4);

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant)
                .controller(ControllerRef::TargetPlayer)
                .properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::TargetPlayer,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ]),
        );
        let mut ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: filter,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );
        ability.target_choice_timing = TargetChoiceTiming::Stack;
        ability.multi_target = Some(MultiTargetSpec::bounded(
            0,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));

        let slots = build_target_slots(&state, &ability).expect("paired graveyard slots");
        assert_eq!(slots.len(), 4, "one player/object pair per opponent");
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(p1_graveyard)],
            "P1's object slot excludes the wrong owner and battlefield lookalike"
        );
        assert_eq!(slots[2].legal_targets, vec![TargetRef::Player(PlayerId(2))]);
        assert_eq!(
            slots[3].legal_targets,
            vec![TargetRef::Object(p2_graveyard)]
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
                    end_cost: None,
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

    fn create_permanent_with_types(
        state: &mut GameState,
        controller: PlayerId,
        card_id: CardId,
        name: &str,
        core_types: &[CoreType],
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
            .core_types = core_types.to_vec();
        object
    }

    fn create_dismantling_wave_source(state: &mut GameState) -> ObjectId {
        let source = create_object(
            state,
            CardId(900),
            PlayerId(0),
            "Dismantling Wave".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .color
            .push(ManaColor::White);
        source
    }

    fn dismantling_wave_fanout_ability(source: ObjectId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::AnyOf(vec![
                        TypeFilter::Artifact,
                        TypeFilter::Enchantment,
                    ]))
                    .controller(ControllerRef::TargetPlayer),
                ),
                cant_regenerate: false,
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::bounded(
            0,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        ability
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

    /// CR 601.2d building-block (AST-shape) test: a division is announced only
    /// among the distributing effect's OWN targets, never sibling-effect targets
    /// elsewhere in the chain. `flatten_targets_in_chain` still returns the full
    /// chain (those siblings still "become targets" per CR 601.2c), proving the
    /// two helpers diverge.
    #[test]
    fn distribution_targets_excludes_sibling_chain_targets() {
        // Top-level divided damage carries two of its own object targets; a
        // chained "tap two target permanents" carries two unrelated targets.
        let mut ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
            vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2)),
            ],
            ObjectId(900),
            PlayerId(0),
        );
        ability = ability.sub_ability(ResolvedAbility::new(
            Effect::SetTapState {
                target: TargetFilter::Typed(TypedFilter::permanent()),
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
            vec![
                TargetRef::Object(ObjectId(3)),
                TargetRef::Object(ObjectId(4)),
            ],
            ObjectId(900),
            PlayerId(0),
        ));

        let dist = distribution_targets(&ability);
        assert_eq!(
            dist,
            vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2))
            ],
            "division scoped to the DealDamage node's own targets"
        );
        assert_eq!(
            flatten_targets_in_chain(&ability).len(),
            4,
            "flatten still spans the whole chain (siblings became targets)"
        );

        // Ordinary player-targeted divided damage (NOT per-opponent fanout):
        // the player target is part of the division and is kept.
        let with_player = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(ObjectId(5)),
            ],
            ObjectId(900),
            PlayerId(0),
        );
        assert_eq!(
            distribution_targets(&with_player),
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(ObjectId(5)),
            ],
            "non-fanout divided damage keeps its player target"
        );

        // Per-opponent fanout divided damage: player refs are structural
        // partitions, not division recipients, so they are stripped.
        let mut fanout = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
                damage_source: None,
                excess: None,
            },
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(ObjectId(6)),
                TargetRef::Player(PlayerId(2)),
                TargetRef::Object(ObjectId(7)),
            ],
            ObjectId(900),
            PlayerId(0),
        );
        fanout.multi_target = Some(MultiTargetSpec::bounded(
            1,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        assert!(is_per_opponent_target_fanout(&fanout));
        assert_eq!(
            distribution_targets(&fanout),
            vec![
                TargetRef::Object(ObjectId(6)),
                TargetRef::Object(ObjectId(7)),
            ],
            "per-opponent fanout strips player partition refs from the division"
        );
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
                excess: None,
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

    /// CR 115.1d + CR 701.3a: variable-count Equipment attachment slots must not
    /// consume a trailing explicit host target (issue #5339 review).
    #[test]
    fn assign_selected_slots_attach_multi_target_preserves_explicit_host() {
        let mut state = GameState::new_two_player(42);
        let source = ObjectId(1);
        let equipment_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        let equipment_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Skullclamp".to_string(),
            Zone::Battlefield,
        );
        let host = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        for id in [equipment_a, equipment_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
        }
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
        }

        let mut ability = ResolvedAbility::new(
            Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::unlimited(0));

        let slots = build_target_slots(&state, &ability).expect("slot build");
        assert_eq!(
            slots.len(),
            3,
            "two optional Equipment slots plus one required host slot"
        );
        assert!(slots[0].optional && slots[1].optional);
        assert!(!slots[2].optional, "explicit host must stay required");

        assign_selected_slots_in_chain(
            &state,
            &mut ability,
            &[
                Some(TargetRef::Object(equipment_a)),
                None,
                Some(TargetRef::Object(host)),
            ],
        )
        .expect("assign attachment window then host");

        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(equipment_a), TargetRef::Object(host),],
            "host must not be folded into the attachment multi-target window"
        );
    }

    /// CR 601.2c: compact declared targets (no per-slot `None` padding) must
    /// reserve the explicit host on the attachment operand window — mirrors the
    /// selected-slot path (issue #5339 review).
    #[test]
    fn assign_targets_in_chain_attach_compact_declared_preserves_explicit_host() {
        let mut state = GameState::new_two_player(42);
        let source = ObjectId(1);
        let equipment_a = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        let _equipment_b = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Skullclamp".to_string(),
            Zone::Battlefield,
        );
        let host = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        for id in [equipment_a, _equipment_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
        }
        {
            let obj = state.objects.get_mut(&host).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
        }

        let mut ability = ResolvedAbility::new(
            Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::unlimited(0));

        assign_targets_in_chain(
            &state,
            &mut ability,
            &[TargetRef::Object(equipment_a), TargetRef::Object(host)],
        )
        .expect("compact declared targets: one Equipment plus required host");

        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(equipment_a), TargetRef::Object(host),],
            "declared-target path must not consume the host as a second attachment"
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
                reveal: true,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

    #[test]
    fn build_target_slots_surfaces_player_slot_for_reveal_until_target_opponent() {
        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RevealUntil {
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                filter: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Land)),
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                matched_disposition: crate::types::ability::RevealUntilDisposition::KeepEach,
                kept_destination: Zone::Battlefield,
                rest_destination: Zone::Graveyard,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                kept_optional_to: None,
                enters_under: None,
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
            "target opponent reveal must not allow targeting yourself"
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

                channel: DamageChannel::Total,
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

    /// T14a (BB-FU10 Step 3a). `QuantityRef::BattlefieldEntriesThisTurn` is the
    /// CR 608.2i look-back sibling of `EnteredThisTurn` and carries the same kind
    /// of population filter, so it must reach the count-over-filter recursion
    /// instead of the `_ => None` fallback.
    ///
    /// DISCRIMINATING BY CONSTRUCTION: the nested count must itself be
    /// target-referencing. `filter_prop_target_slot_filter` routes
    /// `FilterProp::Counters { count, .. }` to `quantity_expr_target_slot_filter`,
    /// which returns `Some` only for a target-bearing ref — a `Fixed(1)` nested
    /// count would be `None` both before AND after the fix (red in both
    /// directions, i.e. broken rather than discriminating).
    ///
    /// REVERT-PROBE: remove the one-line or-group addition → `None`, FAIL.
    #[test]
    fn bbfu10_ledger_variant_surfaces_target_slot_filter() {
        use crate::types::ability::FilterProp;
        use crate::types::counter::{CounterMatch, CounterType};

        let props = vec![FilterProp::Counters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            comparator: Comparator::GE,
            count: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Target,
                },
            },
        }];
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: props,
        });

        let ledger = QuantityRef::BattlefieldEntriesThisTurn {
            player: PlayerScope::Controller,
            filter: filter.clone(),
        };
        let live = QuantityRef::EnteredThisTurn { filter };

        assert_eq!(
            quantity_ref_target_slot_spec(&ledger),
            Some(TargetFilter::Typed(TypedFilter::creature())),
            "CR 608.2i: the ledger variant must surface the nested target slot",
        );
        assert_eq!(
            quantity_ref_target_slot_spec(&ledger),
            quantity_ref_target_slot_spec(&live),
            "parity guard: the look-back and live siblings must agree",
        );
    }

    /// CR 115.1 + CR 208.1 + CR 202.3 + CR 701.9 + CR 120.9: the count-derived
    /// target-slot spec authority must return a filter DERIVED from the count
    /// ref, not a hardcoded creature, and must NOT surface a slot for
    /// non-targeted count refs. Reverting any spec arm flips one of these.
    #[test]
    fn quantity_ref_target_slot_spec_derives_filter_from_count_ref() {
        // CR 208.1: power/toughness of a target → creature slot.
        assert_eq!(
            quantity_ref_target_slot_spec(&QuantityRef::Power {
                scope: ObjectScope::Target,
            }),
            Some(TargetFilter::Typed(TypedFilter::creature())),
            "Power {{ Target }} must surface a creature slot",
        );

        // CR 202.3 + CR 115.1: TargetObjectManaValue carries its own slot filter.
        let artifact_or_creature = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::default()),
                TargetFilter::Typed(TypedFilter::creature()),
            ],
        };
        assert_eq!(
            quantity_ref_target_slot_spec(&QuantityRef::TargetObjectManaValue {
                filter: Box::new(artifact_or_creature.clone()),
            }),
            Some(artifact_or_creature),
            "TargetObjectManaValue must surface the filter it carries verbatim",
        );

        // CR 701.9 + CR 115.1: a single targeted opponent's discards → an
        // Opponent-scoped slot (NOT creature, NOT TargetPlayer).
        assert_eq!(
            quantity_ref_target_slot_spec(&QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Target,
            }),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            "CardsDiscardedThisTurn {{ Target }} must surface an Opponent-scoped slot",
        );
        // Controller-scoped discards declare no slot.
        assert_eq!(
            quantity_ref_target_slot_spec(&QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Controller,
            }),
            None,
            "CardsDiscardedThisTurn {{ Controller }} must surface NO slot",
        );

        // CR 115.1 + CR 109.4: TargetPlayer damage-history → an Opponent-rewritten
        // slot (enumerable); the "your opponents" non-targeted class → no slot.
        let targeted_damage = QuantityRef::DamageDealtThisTurn {
            source: Box::new(TargetFilter::Any),
            target: Box::new(TargetFilter::And {
                filters: vec![
                    TargetFilter::Player,
                    TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::TargetPlayer),
                    ),
                ],
            }),
            aggregate: AggregateFunction::Sum,
            group_by: None,
            damage_kind: DamageKindFilter::Any,

            channel: DamageChannel::Total,
        };
        let spec = quantity_ref_target_slot_spec(&targeted_damage)
            .expect("targeted DamageDealtThisTurn must surface a slot");
        // The rewritten slot filter must be Opponent-scoped (enumerable), never
        // TargetPlayer (which fails closed at enumeration → legal_actions=0 hang).
        assert_eq!(
            relative_controller_kind(&spec),
            None,
            "the surfaced slot filter must be Opponent-scoped, not TargetPlayer (CR 109.4)",
        );

        let opponents_damage = QuantityRef::DamageDealtThisTurn {
            source: Box::new(TargetFilter::Any),
            target: Box::new(TargetFilter::And {
                filters: vec![
                    TargetFilter::Player,
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                ],
            }),
            aggregate: AggregateFunction::Sum,
            group_by: None,
            damage_kind: DamageKindFilter::Any,

            channel: DamageChannel::Total,
        };
        assert_eq!(
            quantity_ref_target_slot_spec(&opponents_damage),
            None,
            "non-targeted 'your opponents' DamageDealtThisTurn must surface NO slot",
        );
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
                end_cost: None,
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
                end_cost: None,
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
                enters_attacking: false,
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

    /// CR 115.1 + CR 608.2b: a ChangeZoneAll player filter is a declared
    /// player target, unlike an internal delayed ParentTarget filter. It must
    /// be revalidated and cannot keep an eliminated player alive as a target.
    #[test]
    fn validate_targets_in_chain_drops_eliminated_change_zone_all_player_target() {
        let mut state = GameState::new_two_player(42);
        state.players[1].is_eliminated = true;
        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(900),
            PlayerId(0),
        );

        let validated = validate_targets_in_chain(&state, &ability);
        assert!(
            validated.targets.is_empty(),
            "an eliminated player must not survive ChangeZoneAll target revalidation"
        );
        assert!(
            crate::game::targeting::check_fizzle(
                &flatten_targets_in_chain(&ability),
                &flatten_targets_in_chain(&validated),
            ),
            "a ChangeZoneAll ability whose sole player target is gone must fizzle"
        );
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
                    excess: None,
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
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                },
                TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: true,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
        };
        let slot_b = TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Object(ObjectId(10)),
                TargetRef::Object(ObjectId(20)),
            ],
            optional: false,
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
        };
        let slot_optional = TargetSelectionSlot {
            legal_targets: vec![shared.clone()],
            optional: true,
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
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
                excess: None,
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
                provenance: None,
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

    /// CR 122.1f + CR 109.4 + CR 115.1: `QuantityRef::TargetControllerCounter`
    /// reads the poison counters on the controller of the ability's first object
    /// target — "if its controller is poisoned" (Corrupted Resolve) — never the
    /// ability's own controller. Discriminating: the caster (P0) is heavily
    /// poisoned while the countered spell's controller (P1) is not, so a
    /// controller-scoped misread would return a nonzero count.
    #[test]
    fn target_controller_poison_reads_object_target_controller_not_caster() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        use crate::types::player::PlayerCounterKind;

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
                source_name: "Stacked Spell".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });

        // Corrupted Resolve cast by P0 (controller), targeting P1's stacked spell.
        let corrupted_resolve = make_simple_ability(vec![TargetRef::Object(stack_id)], ObjectId(0));
        let poisoned_check = QuantityExpr::Ref {
            qty: QuantityRef::TargetControllerCounter {
                kind: PlayerCounterKind::Poison,
            },
        };

        // Caster P0 heavily poisoned; target controller P1 not — must read P1 → 0.
        state.players[0].poison_counters = 9;
        assert_eq!(
            crate::game::quantity::resolve_quantity_with_targets(
                &state,
                &poisoned_check,
                &corrupted_resolve,
            ),
            0,
            "reads the target spell's controller (P1=0), not the caster (P0=9)"
        );

        // Poison P1: "its controller is poisoned" now reads >= 1.
        state.players[1].poison_counters = 1;
        assert_eq!(
            crate::game::quantity::resolve_quantity_with_targets(
                &state,
                &poisoned_check,
                &corrupted_resolve,
            ),
            1,
            "poisoning the target's controller (P1) flips the read to 1"
        );
    }

    /// CR 810.10a + CR 810.10d + CR 810.5: In Two-Headed Giant, "its controller
    /// is poisoned" reads the target controller's TEAM poison total, not their
    /// individual counters. Discriminating: the target's controller (P0) has 0
    /// individual poison, but their teammate (P1) carries the team's 1 poison —
    /// the team is poisoned, so the read must be >= 1. A per-player read (the
    /// pre-fix behavior) would return P0's individual 0 and mis-resolve the
    /// counter's condition to false.
    #[test]
    fn target_controller_poison_reads_team_total_in_two_headed_giant() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        use crate::types::format::FormatConfig;
        use crate::types::player::PlayerCounterKind;

        // 2HG teams: {P0, P1} and {P2, P3}.
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let stack_id = ObjectId(77);
        let source_id = ObjectId(12);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: stack_id,
            source_id,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(make_simple_ability(vec![], source_id)),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Stacked Spell".to_string(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        });

        // The target's controller (P0) has 0 individual poison; the team's
        // poison lives entirely on teammate P1.
        state.players[0].poison_counters = 0;
        state.players[1].poison_counters = 1;

        let ability = make_simple_ability(vec![TargetRef::Object(stack_id)], ObjectId(0));
        let poisoned_check = QuantityExpr::Ref {
            qty: QuantityRef::TargetControllerCounter {
                kind: PlayerCounterKind::Poison,
            },
        };
        assert_eq!(
            crate::game::quantity::resolve_quantity_with_targets(
                &state,
                &poisoned_check,
                &ability,
            ),
            1,
            "2HG: reads the target controller's TEAM poison (P0=0 + teammate P1=1), not P0's individual 0"
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
                token_image_ref: None,
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
                tapped: false,
                is_suspected: false,
                attachments: Vec::new(),
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
                redirect_lifetime: crate::types::ability::RedirectionLifetime::OneOpportunity,
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
                redirect_lifetime: crate::types::ability::RedirectionLifetime::OneOpportunity,
                source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
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
                redirect_lifetime: crate::types::ability::RedirectionLifetime::OneOpportunity,
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
                excess: None,
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
                excess: None,
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
                excess: None,
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
                excess: None,
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

    #[test]
    fn ents_fury_spell_collects_ally_and_opponent_target_slots() {
        use crate::game::zones::create_object;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::card_type::CoreType;
        use crate::types::zones::Zone;

        let def = parse_effect_chain(
            "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.",
            AbilityKind::Spell,
        );
        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let wolf = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        for id in [bear, wolf] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let ability = build_resolved_from_def(&def, ObjectId(1), PlayerId(0));
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots.len(),
            2,
            "Ent's Fury must surface ally + opponent target slots, got {}",
            slots.len()
        );
    }

    #[test]
    fn ents_fury_oracle_text_path_collects_two_target_slots() {
        use crate::database::synthesis::parse_oracle_with_cleave_brackets;
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::zones::Zone;

        let oracle = "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.";
        let (parsed, _) = parse_oracle_with_cleave_brackets(
            oracle,
            "Ent's Fury",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        assert!(
            !parsed.abilities.is_empty(),
            "oracle parse must produce a spell ability"
        );
        let mut combined = parsed.abilities[0].clone();
        for spell_ability in parsed.abilities.iter().skip(1) {
            if spell_ability.kind == AbilityKind::Spell {
                let mut node = &mut combined;
                while node.sub_ability.is_some() {
                    node = node.sub_ability.as_mut().unwrap();
                }
                node.sub_ability = Some(Box::new(spell_ability.clone()));
            }
        }
        let mut state = GameState::new_two_player(42);
        let bear = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let wolf = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        for id in [bear, wolf] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let ability = build_resolved_from_def(&combined, ObjectId(1), PlayerId(0));
        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(
            slots.len(),
            2,
            "production oracle path must surface ally + opponent slots (abilities={}), got {}",
            parsed.abilities.len(),
            slots.len()
        );
    }
}
