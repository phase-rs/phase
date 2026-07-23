use crate::game::functioning_abilities::static_kind_present;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ChoiceValue, ChosenAttribute,
    ContinuousModification, CostPaidObjectSnapshot, Effect, ManaProduction, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetFilter, REMOVE_COUNTER_COST_ALL,
    REMOVE_COUNTER_COST_ANY_NUMBER,
};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    CostResume, GameState, ManaAbilityCostCursor, ManaAbilityCostParent,
    ManaAbilityCostParentLifecycle, ManaAbilityCostResolutionMode, ManaAbilityResume, ManaChoice,
    ManaChoiceContext, ManaChoicePrompt, PayCostKind, PayableResource, PendingCostMoveResume,
    PendingManaAbility, ProductionOverride, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost, ManaPool, ManaType, PaymentContext};
#[cfg(test)]
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticModeKind;
use crate::types::zones::Zone;
use std::collections::HashSet;

use super::cost_payability::{eligible_exile_cost_objects, exile_cost_effective_zone};
use super::effects::mana::resolve_restrictions;
use super::engine::EngineError;
use super::filter::{matches_target_filter, FilterContext};
use super::life_costs::{self, PayLifeCostResult};
use super::mana_payment;
use super::mana_sources;
use super::mana_sources::{mana_color_to_type, mana_type_to_color};
use super::sacrifice;
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};

/// Check if a typed ability definition represents a mana ability (CR 605).
/// CR 605.3: Mana abilities produce mana and resolve immediately without using the stack.
/// CR 605.1a: A mana ability cannot have targets. If the effect produces mana but the
/// ability has targeting (e.g., via `multi_target`), it must use the stack instead.
/// Currently `Effect::Mana` has no embedded target field and no `AbilityCost` variant
/// implies targeting, so this check is defensive — if future variants introduce
/// targeting on mana-producing abilities, this guard ensures correctness.
pub fn is_mana_ability(ability_def: &AbilityDefinition) -> bool {
    let target_attached = match &*ability_def.effect {
        Effect::Mana { target, .. } => target.as_ref(),
        _ => return false,
    };
    // CR 605.1a: A targeted mana-producing ability is not a mana ability.
    // Reject both the explicit `multi_target` mechanism and the embedded
    // `Effect::Mana::target` field (Jeska's Will mode 1: "Add {R} for each
    // card in target opponent's hand" — the spell targets, so it must use the
    // stack and is not a mana ability under CR 605).
    if ability_def.multi_target.is_some() || target_attached.is_some() {
        return false;
    }
    // CR 605.1a: "...and it's not a loyalty ability." A loyalty ability (CR 606)
    // that happens to add mana — e.g. Chandra, Bold Pyromancer's `[+1]: Add
    // {R}{R}` — is NOT a mana ability: it uses the stack and obeys loyalty-ability
    // timing (CR 606.3, sorcery speed, once per turn). Excluding it here keeps it
    // off the instant-speed mana-ability path.
    // CR 606: a loyalty ability adjusts loyalty as its cost — exclude it here.
    if mana_sources::cost_has_component(&ability_def.cost, |c| {
        matches!(c, AbilityCost::Loyalty { .. })
    }) {
        return false;
    }
    true
}

/// CR 605.1b: A triggered ability is a mana ability iff all three hold:
///   (a) it doesn't require a target (CR 115.6),
///   (b) it triggers from the activation/resolution of an activated mana ability
///       OR from mana being added to a player's mana pool,
///   (c) it could add mana to a player's mana pool when it resolves.
///
/// Triggered mana abilities don't use the stack (CR 605.3b applies analogously);
/// they resolve immediately at the moment the trigger event occurs. This is the
/// single authority for classifying triggered mana abilities — all trigger-enqueue
/// call sites must route through this classifier.
///
/// `trigger_event` is the event that caused the trigger to fire (CR 603.7c).
///
/// Criterion (c) requires that **every** reachable link in the resolution graph
/// (the `sub_ability` chain and the `else_ability` branch at each link, per
/// CR 608.2c) is `Effect::Mana`. Inline resolution runs the full chain without
/// giving any player priority — so a mixed chain like "add {G}, then draw a
/// card" must use the stack, not route inline. "Any link adds mana" is too
/// permissive: it would skip priority on the draw.
///
/// Criterion (b) accepts `TappedForMana` (CR 106.12a) — the per-resolution
/// event emitted whenever a `{T}`-cost mana ability resolves and produces mana,
/// which is exactly the event a `TapsForMana` triggered mana ability fires
/// from. It also accepts `ManaAdded`, because CR 605.1b explicitly includes
/// abilities that trigger from mana being added. CR 605.1b also admits
/// "triggered from the activation/resolution of an activated mana ability" in
/// general, but mana abilities bypass the stack and do not emit a
/// distinguishable `AbilityActivated` event; widening (b) to that axis requires
/// first emitting such an event. No real card exercises the gap today.
pub fn is_triggered_mana_ability(
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
) -> bool {
    // (c) Every reachable link must produce mana. A mixed chain (Mana + Draw,
    // Mana + Damage, …) cannot route inline because non-mana effects in the
    // chain require stack resolution to give players priority.
    if !chain_is_all_mana(ability) {
        return false;
    }
    // (a) No target anywhere in the reachable resolution graph — mirrors the
    // activated-mana-ability guard in `is_mana_ability`. A downstream link
    // with targets (CR 115.6) disqualifies inline resolution, since the full
    // chain must resolve without interrupting for target selection.
    if chain_has_any_targets(ability) {
        return false;
    }
    // (b) CR 106.12a / CR 605.1b: triggered by a `{T}`-cost mana ability
    // resolving and producing mana, or by mana being added. See the doc comment
    // above for the deliberately-not-yet-widened `AbilityActivated` axis.
    matches!(
        trigger_event,
        Some(GameEvent::TappedForMana { .. } | GameEvent::ManaAdded { .. })
    )
}

/// True iff every reachable link (via `sub_ability` and `else_ability` per
/// CR 608.2c) has `Effect::Mana`. The "every link is mana" rule is the
/// conservative reading of CR 605.1b(c) — inline resolution skips priority,
/// so any non-mana effect reachable during resolution forces stack use.
fn chain_is_all_mana(ability: &ResolvedAbility) -> bool {
    visit_links_all(ability, &|link| matches!(link.effect, Effect::Mana { .. }))
}

/// True iff **any** reachable link (via `sub_ability` and `else_ability`)
/// carries targets or a `multi_target` spec (CR 115.6 + CR 608.2c).
fn chain_has_any_targets(ability: &ResolvedAbility) -> bool {
    visit_links_any(ability, &|link| {
        !link.targets.is_empty() || link.multi_target.is_some()
    })
}

/// CR 105.3 + CR 106.1a: True iff any reachable link of this mana ability sets a
/// permanent's color to the mana produced earlier in the same activation — i.e.
/// carries a `ContinuousModification::AddChosenColor { .. }` ("… becomes that color",
/// Foraging Wickermaw). Gates the `ChosenAttribute::Color` record in
/// `produce_mana_from_ability` so ordinary producers (basics, City of Brass,
/// painlands, filter lands) never touch `chosen_attributes` — zero blast radius.
/// Built fresh per activation and walks only the activated ability's own chain.
fn chain_references_chosen_color(ability: &ResolvedAbility) -> bool {
    visit_links_any(ability, &|link| match &link.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. }))
        }),
        _ => false,
    })
}

/// CR 106.1a: The single `ManaColor` this activation produced, or `None` if the
/// produced mana is empty, colorless, or spans more than one color. Reuses the
/// honest `mana_type_to_color` converter (Colorless → `None`). Used to bind
/// "that color" to the color of the mana the ability just made.
fn sole_produced_color(produced: &[ManaType]) -> Option<ManaColor> {
    let mut iter = produced.iter();
    let first = mana_type_to_color(*iter.next()?)?;
    for &mana_type in iter {
        if mana_type_to_color(mana_type)? != first {
            return None;
        }
    }
    Some(first)
}

/// Visit every reachable link of `ability` — head + `sub_ability` chain +
/// `else_ability` branches at each link — and return `true` iff `pred` holds
/// for all of them. Mirrors `chain_is_all_mana` / `chain_has_any_targets`'s
/// single traversal shape so the two walkers stay structurally identical.
fn visit_links_all(ability: &ResolvedAbility, pred: &dyn Fn(&ResolvedAbility) -> bool) -> bool {
    if !pred(ability) {
        return false;
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        if !visit_links_all(sub, pred) {
            return false;
        }
    }
    if let Some(else_branch) = ability.else_ability.as_deref() {
        if !visit_links_all(else_branch, pred) {
            return false;
        }
    }
    true
}

/// Dual of [`visit_links_all`]: returns `true` iff `pred` holds for any
/// reachable link.
fn visit_links_any(ability: &ResolvedAbility, pred: &dyn Fn(&ResolvedAbility) -> bool) -> bool {
    if pred(ability) {
        return true;
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        if visit_links_any(sub, pred) {
            return true;
        }
    }
    if let Some(else_branch) = ability.else_ability.as_deref() {
        if visit_links_any(else_branch, pred) {
            return true;
        }
    }
    false
}

/// CR 605.4a: Resolve a triggered mana ability inline (stack-skipped).
/// The ability's effect chain is executed immediately; mana additions land in the
/// controller's pool before any player could respond.
pub fn resolve_triggered_mana_ability_inline(
    state: &mut GameState,
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
) {
    // CR 603.3d: a triggered mana ability still resolves after its source has
    // left its zone. Production paths carry exact identity via the Plan-04
    // `trigger_source` context (captured at trigger time, LKI-safe); the
    // object lookup covers pre-P04 callers whose source is still present.
    // When neither is available (source gone AND no trigger context — only
    // synthetic/legacy callers), no dedicated journal node is begun: produced
    // mana falls back to the automatic Proposal attribution in
    // `add_mana_to_pool`, preserving pip conservation without fabricating an
    // exact incarnation identity.
    let source = ability
        .trigger_source
        .as_ref()
        .map(|context| context.identity.reference)
        .or_else(|| {
            state
                .objects
                .get(&ability.source_id)
                .map(crate::types::ObjectIncarnationRef::from_object)
        });
    let node = source.map(|source| {
        let caused_by = match trigger_event {
            Some(
                GameEvent::ManaAdded { source_id, .. } | GameEvent::TappedForMana { source_id, .. },
            ) => state
                .resolved_rules_journal
                .latest_mana_producer_for_source(*source_id),
            _ => None,
        };
        state.begin_triggered_mana_journal_node(
            source,
            ability.trigger_definition_ref.clone(),
            caused_by,
        )
    });
    state.with_optional_rules_execution_node(node, |state| {
        let previous_trigger_event = state.current_trigger_event.clone();
        let previous_mana_override = state.current_triggered_mana_override.take();
        state.current_trigger_event = trigger_event.cloned();
        // Forward the planned color override so `effects::mana::resolve` can produce
        // the correct color for `AnyOneColor` triggered mana abilities (Fertile Ground)
        // rather than defaulting to `color_options.first()`.
        state.current_triggered_mana_override = color_override;
        // Use the standard resolution entry so sub_ability chains resolve uniformly.
        let _ = super::effects::resolve_ability_chain(state, ability, events, 0);
        state.current_triggered_mana_override = previous_mana_override;
        state.current_trigger_event = previous_trigger_event;
    });
}

/// CR 605.2: Mana abilities don't use the stack — they can't be targeted, countered, or responded to.
/// CR 605.3b: Mana abilities resolve immediately when activated.
///
/// Pays the full ability cost (tap, sacrifice, etc.) via `pay_mana_ability_cost`,
/// then produces mana. When `color_override` is `Some`, the choice dimension is
/// already resolved (auto-tap during cost payment): `SingleColor` replays a
/// single-color pick for `AnyOneColor`/`ChoiceAmongExiledColors`, while
/// `Combination` carries a full pre-chosen multi-mana sequence for
/// `ChoiceAmongCombinations` (filter lands).
pub fn resolve_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
) -> Result<(), EngineError> {
    // CR 605.3c: A top-level mana-ability activation has no suspended ancestor
    // on the call stack, so the in-flight exclusion chain starts empty. The
    // source itself is added downstream in `pay_mana_sub_cost`.
    resolve_mana_ability_excluding(
        state,
        source_id,
        player,
        ability_def,
        events,
        color_override,
        &HashSet::new(),
        None,
        None,
        None,
    )
}

/// Resolve a mana ability while excluding an in-flight chain of ancestor
/// mana-ability sources from the cost-payment auto-tap (CR 605.3c). Called by
/// the casting auto-tap (`auto_tap_mana_sources_inner`) when paying one mana
/// ability's mana sub-cost forces activation of further mana abilities: each
/// ancestor activation is synchronously suspended mid-payment on the Rust call
/// stack and must not be re-activated, or the auto-tap recurses infinitely
/// (two cross-paying Signets, an N-source chain, or a self-loop).
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_mana_ability_excluding(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    excluded_sources: &HashSet<ObjectId>,
    // CR 107.4b + CR 118.10: When this ability is being activated to fund an
    // outer cost (nested Phase-3 auto-tap), the outer cost's colored shard demand
    // is threaded here so this ability's own mana sub-cost is funded from
    // non-demanded mana, never a floated color the outer cost still needs. `None`
    // at the top-level entry — there is no outer cost on the stack.
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    resume: Option<&ManaAbilityResume>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<(), EngineError> {
    let waiting_before = state.waiting_for.clone();
    let ability_index = state
        .objects
        .get(&source_id)
        .and_then(|object| {
            object
                .abilities
                .iter()
                .position(|ability| ability == ability_def)
        })
        .unwrap_or(usize::MAX);
    let rules_execution_node = Some(state.begin_activated_mana_journal_node(source_id));
    let pending = PendingManaAbility {
        player,
        source_id,
        ability_index,
        rules_execution_node,
        ability_snapshot: Some(ability_def.clone()),
        color_override,
        // The direct resolver normally leaves its caller's waiting root
        // untouched.  Its ordinary completion must therefore return to
        // priority; the outer typed payment root lives exclusively in
        // `cost_move_resume` and is promoted only when a replaceable cost
        // move actually pauses.  Reusing that root here would make a
        // synchronous auto-tap recursively retry its still-live caller.
        resume: ManaAbilityResume::Priority,
        cost_move_resume: resume.cloned(),
        chosen_tappers: Vec::new(),
        chosen_discards: Vec::new(),
        chosen_mana_payment: None,
        chosen_counter_count: None,
        chosen_x: None,
        collected_evidence: Vec::new(),
        chosen_exiled: Vec::new(),
        chosen_sacrificed_battlefield: Vec::new(),
        cost_paid_object: None,
        batch_siblings: Vec::new(),
    };
    // CR 605.3b + CR 616.1: This direct path is used by auto-tap and retains
    // its historical default-output semantics even when a cost move pauses.
    // The cursor serializes that resolution mode with the exact outer resume.
    let cost_event_start = events.len();
    let waiting_for = continue_mana_ability_cost_payment(
        state,
        pending,
        mana_ability_cost_cursor(
            &ability_def.cost,
            excluded_sources,
            sub_cost_demand,
            ManaAbilityCostResolutionMode::AutoResolved,
            parent,
        ),
        events,
        cost_event_start,
    )?;
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        state.waiting_for = waiting_for;
    } else {
        state.waiting_for = waiting_before;
    }
    Ok(())
}

/// Produce mana from a resolved mana ability without paying costs.
/// Shared by `resolve_mana_ability` (cost paid inline) and `handle_choose_mana_color`
/// (cost already paid during the `TapCreaturesForManaAbility` phase).
///
/// `cost_paid_object` carries the captured public characteristics of any
/// object exiled or sacrificed as part of cost payment so production counts can
/// resolve cost-paid-object refs (Food Chain / Burnt Offering class).
#[allow(clippy::too_many_arguments)]
fn produce_mana_from_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    chosen_x: Option<u32>,
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) {
    // CR 117.1 + CR 202.3: Build a transient `ResolvedAbility` carrying the
    // cost-paid object snapshot so quantity resolution sees it. Reused for
    // both production-count and sub-chain resolution paths so the same
    // snapshot is visible end-to-end.
    let resolved_for_quantity = resolved_mana_ability_for_current_state(
        state,
        source_id,
        player,
        ability_def,
        chosen_x,
        cost_paid_object,
    );

    // CR 106.6: Resolve spend-restriction templates, grants, and expiry so they
    // attach to each produced `ManaUnit`. Dropping these here is the bug that
    // made Flamebraider's Elemental-only mana behave as unrestricted mana.
    let (produced_mana, restrictions, grants, expiry, source_could_produce_two_or_more_colors) =
        match &resolved_for_quantity.effect {
            Effect::Mana {
                produced,
                restrictions,
                grants,
                expiry,
                target: None,
            } => {
                let mana = match color_override {
                    // `Combination` is pre-chosen — skip `resolve_mana_types` entirely
                    // so the exact sequence lands in the pool (CR 605.3b).
                    Some(ProductionOverride::Combination(types)) => types,
                    Some(ProductionOverride::SingleColor(color)) => resolve_single_color_override(
                        state,
                        produced,
                        &resolved_for_quantity,
                        color,
                    ),
                    None => super::effects::mana::resolve_mana_types_for_ability(
                        produced,
                        state,
                        &resolved_for_quantity,
                    ),
                };
                let concrete = resolve_restrictions(restrictions, state, source_id);
                let source_could_produce_two_or_more_colors =
                    mana_sources::source_could_produce_two_or_more_colors(state, source_id, player);
                (
                    mana,
                    concrete,
                    grants.clone(),
                    *expiry,
                    source_could_produce_two_or_more_colors,
                )
            }
            _ => (Vec::new(), Vec::new(), Vec::new(), None, false),
        };

    // CR 106.12: a permanent is "tapped for mana" when the activated mana
    // ability's cost includes the `{T}` symbol.
    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for &mana_type in &produced_mana {
        mana_payment::produce_mana_with_attributes_from_source_quality(
            state,
            source_id,
            mana_type,
            player,
            tapped,
            source_could_produce_two_or_more_colors,
            &restrictions,
            &grants,
            expiry,
            events,
        );
    }

    // CR 105.3 + CR 106.1a + CR 605.3b: If a later clause in THIS mana ability
    // sets a permanent's color to the mana just produced ("… becomes that
    // color", Foraging Wickermaw), record the produced color on the source so
    // the downstream `AddChosenColor` (Layer 5, CR 613.1e) reads it live. Gated
    // on the chain actually carrying an `AddChosenColor`, so ordinary producers
    // are untouched. Placed above the `TappedForMana` push below, which MOVES
    // `produced_mana`.
    if chain_references_chosen_color(&resolved_for_quantity) {
        if let Some(color) = sole_produced_color(&produced_mana) {
            if let Some(obj) = state.objects.get_mut(&source_id) {
                // CR 400.7: `chosen_attributes` persist on the permanent until it
                // changes zones, and `chosen_color()` returns the FIRST match, so
                // a re-activation on a later turn must OVERWRITE — retain-drop any
                // prior `Color`, then push the current one (not accumulate).
                obj.chosen_attributes
                    .retain(|a| !matches!(a, ChosenAttribute::Color(_)));
                obj.chosen_attributes.push(ChosenAttribute::Color(color));
            }
        }
    }

    // CR 106.12a: an "is tapped for mana" trigger fires once per resolution of
    // a `{T}`-cost mana ability that produces mana — not once per mana unit.
    // Emit a single `TappedForMana` here so the `TapsForMana` matcher fires
    // exactly once (the per-unit `ManaAdded` events above remain pool
    // accounting only).
    if tapped && !produced_mana.is_empty() {
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id,
            produced: produced_mana,
            tap_state: ManaTapState::from_tap(tapped),
        });
    }

    // CR 605.3b + CR 605.1a: A mana ability with a non-mana clause in its
    // effect chain (e.g. painlands' "This land deals 1 damage to you.")
    // resolves that chain inline — mana abilities don't use the stack, so
    // the sub-ability runs as part of the same atomic resolution.
    resolve_mana_ability_sub_chain(state, &resolved_for_quantity, events);
}

fn resolved_mana_ability_for_current_state(
    state: &GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    chosen_x: Option<u32>,
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) -> ResolvedAbility {
    let mut resolved =
        super::ability_utils::build_resolved_from_def(ability_def, source_id, player);
    if let Some(snapshot) = cost_paid_object {
        resolved.set_cost_paid_object_recursive(snapshot);
    }
    // CR 107.3a/.3c: bind the announced X into the resolved ability so produced
    // mana counts (`AnyCombination { count: Ref(Variable "X") }`) and any X-bearing
    // sub-effects resolve to the chosen value (Chicago Loop's `Add X mana`).
    if let Some(x) = chosen_x {
        resolved.set_chosen_x_recursive(x);
    }
    apply_condition_instead_mana_swap(state, &resolved)
}

fn apply_condition_instead_mana_swap(
    state: &GameState,
    ability: &ResolvedAbility,
) -> ResolvedAbility {
    let Some(sub) = ability.sub_ability.as_deref() else {
        return ability.clone();
    };
    let Some(AbilityCondition::ConditionInstead { inner }) = sub.condition.as_ref() else {
        return ability.clone();
    };
    if super::effects::evaluate_condition(inner, state, ability) {
        if matches!(sub.effect, Effect::Mana { target: None, .. }) {
            return super::ability_utils::apply_instead_swap(ability, sub);
        }
        return ability.clone();
    }

    let mut base = ability.clone();
    base.sub_ability = sub.else_ability.clone();
    base
}

fn resolve_single_color_override(
    state: &mut GameState,
    produced: &ManaProduction,
    ability: &ResolvedAbility,
    color: ManaType,
) -> Vec<ManaType> {
    let previous_choice = if matches!(produced, ManaProduction::ChosenColor { .. }) {
        let Some(chosen_color) = mana_type_to_color(color) else {
            return Vec::new();
        };
        let previous = state.last_named_choice.take();
        state.last_named_choice = Some(ChoiceValue::Color(chosen_color));
        Some(previous)
    } else {
        None
    };

    let resolved = super::effects::mana::resolve_mana_types_for_ability(produced, state, ability);

    if let Some(previous) = previous_choice {
        state.last_named_choice = previous;
    }

    vec![color; resolved.len()]
}

/// CR 605.3b: Mana abilities resolve immediately unless paying the cost requires a choice.
#[allow(clippy::too_many_arguments)]
pub fn activate_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    resume: ManaAbilityResume,
    color_override: Option<ProductionOverride>,
) -> Result<WaitingFor, EngineError> {
    let source = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Mana ability source not found".to_string()))?;
    // CR 702.26b: Phased-out permanents are treated as though they do not
    // exist, so they cannot activate abilities.
    if source.is_phased_out_permanent() {
        return Err(EngineError::ActionNotAllowed(
            "Phased-out permanents cannot activate abilities (CR 702.26b)".to_string(),
        ));
    }
    if source.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if source.zone != required_zone {
        return Err(EngineError::InvalidAction(format!(
            "Object is not in the correct zone (expected {:?})",
            required_zone
        )));
    }
    // CR 602.5: enforce activation prohibitions at the executor, not just at
    // legal-action filtering — a buggy or hostile client may submit
    // `GameAction::ActivateAbility` directly. The mana-ability fast path must
    // honor the same static-ability gates that `casting::handle_activate_ability`
    // applies on the non-mana path, so City of Solitude (CantActivateDuring with
    // exemption: None) and any future CantBeActivated with exemption: None block
    // mana activations as the rules require.
    if super::casting::is_blocked_by_cant_be_activated(state, player, source_id, ability_def) {
        return Err(EngineError::ActionNotAllowed(
            "Activated abilities of this permanent can't be activated (CR 602.5)".to_string(),
        ));
    }
    if super::casting::is_blocked_by_cant_activate_during(state, player, ability_def) {
        return Err(EngineError::ActionNotAllowed(
            "Activated abilities can't be activated during this turn (CR 602.5 + CR 117.1b)"
                .to_string(),
        ));
    }
    super::restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    let rules_execution_node = Some(state.begin_activated_mana_journal_node(source_id));
    advance_mana_ability_activation(
        state,
        PendingManaAbility {
            player,
            source_id,
            ability_index,
            rules_execution_node,
            ability_snapshot: Some(ability_def.clone()),
            color_override,
            resume,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        },
        events,
    )
}

fn complete_mana_ability_activation(
    state: &mut GameState,
    source_id: ObjectId,
    ability_index: usize,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    super::restrictions::record_ability_activation(state, source_id, ability_index);
    super::casting_targets::emit_keyword_ability_event_if_tagged(
        state,
        source_id,
        ability_index,
        player,
        events,
    );
}

/// Extract the prompt shape for a mana ability that requires a player choice.
///
/// Returns `Some(ManaChoicePrompt::SingleColor)` when the player must pick one
/// color from a set (AnyOneColor, ChoiceAmongExiledColors) and
/// `Some(ManaChoicePrompt::Combination)` when the player must pick one of
/// several fixed multi-mana sequences (filter lands). Returns
/// `Some(ManaChoicePrompt::AnyCombination)` when each produced mana unit has
/// an independent color choice. Returns `None` when production is fully
/// determined (Fixed, Colorless, single-option AnyOneColor).
pub(crate) fn mana_choice_prompt(
    effect: &Effect,
    state: &GameState,
    source_id: ObjectId,
    ability: Option<&ResolvedAbility>,
) -> Option<ManaChoicePrompt> {
    let Effect::Mana { produced, .. } = effect else {
        return None;
    };
    match produced {
        ManaProduction::AnyOneColor { color_options, .. } if color_options.len() > 1 => {
            Some(ManaChoicePrompt::SingleColor {
                options: color_options.iter().map(mana_color_to_type).collect(),
            })
        }
        ManaProduction::AnyCombination { color_options, .. } if color_options.len() > 1 => {
            let ability = ability?;
            let count =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                    .len();
            if count > 0 {
                Some(ManaChoicePrompt::AnyCombination {
                    count,
                    options: color_options.iter().map(mana_color_to_type).collect(),
                })
            } else {
                None
            }
        }
        ManaProduction::ChoiceAmongExiledColors { source } => {
            let options = super::effects::mana::exiled_color_options(state, *source, source_id);
            if options.len() > 1 {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        ManaProduction::AnyOneColorAmongPermanents { filter, .. } => {
            // CR 106.1: Player chooses one of the colors among matching permanents they
            // control.
            let options = super::effects::mana::distinct_colors_among_permanents(
                state, ability, source_id, filter,
            )
            .into_iter()
            .map(|color| mana_color_to_type(&color))
            .collect::<Vec<_>>();
            if options.len() > 1 {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 605.3b: Filter lands — pick one of N fixed multi-mana combinations.
        ManaProduction::ChoiceAmongCombinations { options } if options.len() > 1 => {
            Some(ManaChoicePrompt::Combination {
                options: options
                    .iter()
                    .map(|combo| combo.iter().map(mana_color_to_type).collect())
                    .collect(),
            })
        }
        ManaProduction::ChosenColor {
            fixed_alternative, ..
        } => {
            let chosen = super::effects::mana::chosen_color_for_mana(state, source_id);
            match (fixed_alternative, chosen) {
                // CR 106.1: "Add {fixed} or one mana of the chosen color" — once
                // a color is chosen, the player still picks between the fixed
                // color and the chosen color. Dedupe defensively: identical
                // options collapse to a 1-element set (no prompt).
                (Some(fixed), Some(chosen)) => {
                    let mut options = vec![mana_color_to_type(fixed)];
                    let chosen_type = mana_color_to_type(&chosen);
                    if !options.contains(&chosen_type) {
                        options.push(chosen_type);
                    }
                    if options.len() > 1 {
                        Some(ManaChoicePrompt::SingleColor { options })
                    } else {
                        None
                    }
                }
                // CR 106.1: no color chosen yet (cannot occur for Gate lands —
                // the as-enters Choose always fires first — but the field makes
                // it representable). The fixed color is a subset of ALL, so a
                // full five-color prompt loses nothing.
                (Some(_), None) | (None, None) => Some(ManaChoicePrompt::SingleColor {
                    options: ManaColor::ALL.iter().map(mana_color_to_type).collect(),
                }),
                // CR 106.1: pure chosen-color production with a color already
                // chosen — no prompt (Utopia Sprawl class).
                (None, Some(_)) => None,
            }
        }
        // CR 106.7 + CR 106.1b: Reflecting Pool class — surface the union of
        // mana types that filter-matching lands could produce, including
        // `Colorless`. With 0 or 1 options the resolver handles it without a
        // prompt (CR 106.5: empty union → no mana; single option auto-picks).
        ManaProduction::AnyTypeProduceableBy { land_filter, .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let options = super::mana_sources::produceable_mana_types_by_filter(
                state,
                land_filter,
                owner,
                source_id,
            );
            if options.len() > 1 {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 903.4 + CR 903.4f + CR 106.5: Dynamically resolve the activator's
        // commander color identity. If the identity contains 0 or 1 colors,
        // the resolver handles it without a prompt (CR 106.5: undefined color
        // produces no mana; single-color identity auto-picks).
        ManaProduction::AnyInCommandersColorIdentity { .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let identity = super::commander::commander_color_identity(state, owner);
            if identity.len() > 1 {
                Some(ManaChoicePrompt::SingleColor {
                    options: identity.iter().map(mana_color_to_type).collect(),
                })
            } else {
                None
            }
        }
        // CR 106.7 (issue #1556): Exotic Orchard class — "add one mana of any
        // color that a land an opponent controls could produce." Surface the
        // union of producible colors as a choice; with 0 or 1 option the
        // resolver handles it without a prompt (CR 106.5: empty union → no mana;
        // single option auto-picks). Mirrors `AnyTypeProduceableBy`.
        ManaProduction::OpponentLandColors { .. } => {
            let owner = state.objects.get(&source_id).map(|obj| obj.controller)?;
            let options = super::mana_sources::opponent_land_color_options(state, owner);
            if options.len() > 1 {
                Some(ManaChoicePrompt::SingleColor { options })
            } else {
                None
            }
        }
        // CR 106.1 + CR 202.2c: Omnath, Locus of All — each of the produced mana
        // is freely chosen among the scoped object's colors (dynamic, mirrors
        // AnyCombination but with a runtime-resolved option set). Surface the
        // AnyCombination prompt only when the object has more than one color; 0 or
        // 1 color needs no prompt (CR 106.5 empty → no mana; single auto-picks).
        ManaProduction::AnyCombinationOfObjectColors { scope, .. } => {
            let options = super::effects::mana::object_colors_for_scope(state, ability, *scope)
                .iter()
                .map(mana_color_to_type)
                .collect::<Vec<_>>();
            if options.len() <= 1 {
                return None;
            }
            let ability = ability?;
            let count =
                super::effects::mana::resolve_mana_types_for_ability(produced, state, ability)
                    .len();
            if count > 0 {
                Some(ManaChoicePrompt::AnyCombination { count, options })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// CR 605.3b: Complete the mana color/combination choice. Cost was already
/// paid before the prompt (either in `activate_mana_ability` or
/// `handle_tap_creatures_for_mana_ability`), so this only produces mana.
/// The `choice` shape must match the `prompt` shape — the engine rejects
/// mismatches (e.g., answering `Combination` to a `SingleColor` prompt).
pub fn handle_choose_mana_color(
    state: &mut GameState,
    pending: &PendingManaAbility,
    prompt: &ManaChoicePrompt,
    chosen: ManaChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let override_value = match (prompt, chosen) {
        (ManaChoicePrompt::SingleColor { options }, ManaChoice::SingleColor(color)) => {
            if !options.contains(&color) {
                return Err(EngineError::InvalidAction(
                    "Chosen color is not among the legal options".to_string(),
                ));
            }
            ProductionOverride::SingleColor(color)
        }
        (ManaChoicePrompt::Combination { options }, ManaChoice::Combination(combo)) => {
            if !options.iter().any(|opt| opt == &combo) {
                return Err(EngineError::InvalidAction(
                    "Chosen combination is not among the legal options".to_string(),
                ));
            }
            ProductionOverride::Combination(combo)
        }
        (ManaChoicePrompt::AnyCombination { count, options }, ManaChoice::Combination(combo)) => {
            if combo.len() != *count || combo.iter().any(|color| !options.contains(color)) {
                return Err(EngineError::InvalidAction(
                    "Chosen mana combination is not legal for this prompt".to_string(),
                ));
            }
            ProductionOverride::Combination(combo)
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Mana choice shape does not match the active prompt".to_string(),
            ));
        }
    };

    let ability_def = state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| obj.abilities.get(pending.ability_index))
        .cloned()
        .or_else(|| pending.ability_snapshot.clone())
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))?;

    let node = pending
        .rules_execution_node
        .unwrap_or_else(|| state.begin_activated_mana_journal_node(pending.source_id));
    state.with_rules_execution_node(node, |state| {
        produce_mana_from_ability(
            state,
            pending.source_id,
            pending.player,
            &ability_def,
            events,
            Some(override_value),
            pending.chosen_x,
            pending.cost_paid_object.clone(),
        );
        complete_mana_ability_activation(
            state,
            pending.source_id,
            pending.ability_index,
            pending.player,
            events,
        );
    });

    Ok(resume_waiting_for(pending.player, pending.resume.clone()))
}

/// CR 605.3a: Bulk-activate the controller's other identical, choice-free mana
/// sources (their remaining Treasures, etc.) with the color just chosen for a
/// `SingleColor` prompt. Runs immediately after `handle_choose_mana_color` has
/// resolved the originally-tapped source; together they activate `count` sources
/// in one `ChooseManaColor` round-trip.
///
/// Each sibling is an independent activated mana ability that resolves
/// immediately and before the next is begun (CR 605.3c), without using the stack
/// (CR 605.3b) — so no player gains priority between them. Cost-payment and mana
/// events append to `events`; the caller's single post-handler trigger scan then
/// fires each sacrifice's observers (Mayhem Devil, Korvold, Cruel Celebrant, …)
/// exactly once. `pending.batch_siblings` was pre-filtered to choice-free,
/// currently-activatable twins (see `cost_resolves_without_choice` /
/// `batch_eligible_siblings`), so no sibling can surface a further interactive
/// prompt — that invariant is asserted below rather than handled.
pub(crate) fn batch_activate_mana_siblings(
    state: &mut GameState,
    pending: &PendingManaAbility,
    chosen: &ManaChoice,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let ManaChoice::SingleColor(color) = chosen else {
        return Err(EngineError::InvalidAction(
            "Bulk mana activation is only valid for a single-color choice".to_string(),
        ));
    };
    // `count` is validated against `batch_siblings.len() + 1` by the dispatcher
    // before any mana is produced, so `extra` never exceeds the sibling list and
    // `take` is exact.
    let extra = (count as usize).saturating_sub(1);

    // The originally-activated source's mana ability is the shape every sibling
    // was selected to match. Re-resolve each sibling's matching ability index
    // (a sibling may carry unrelated abilities too).
    let reference_def = state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| obj.abilities.get(pending.ability_index))
        .cloned()
        .ok_or_else(|| {
            EngineError::InvalidAction("Mana ability source no longer exists".to_string())
        })?;

    for &sibling_id in pending.batch_siblings.iter().take(extra) {
        let Some((index, def)) = state.objects.get(&sibling_id).and_then(|obj| {
            obj.abilities
                .iter()
                .position(|ability| *ability == reference_def)
                .map(|index| (index, obj.abilities[index].clone()))
        }) else {
            return Err(EngineError::InvalidAction(
                "Bulk mana source is no longer available".to_string(),
            ));
        };
        // CR 605.3a + CR 605.3b: independent mana ability, no stack, color fixed.
        let resume = activate_mana_ability(
            state,
            sibling_id,
            pending.player,
            index,
            &def,
            events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(*color)),
        )?;
        debug_assert!(
            matches!(resume, WaitingFor::Priority { .. }),
            "batched choice-free mana sibling returned an interactive state: {resume:?}"
        );
    }
    Ok(())
}

/// CR 118.3 / CR 605.3b: Complete the tapped-creature choice, then resolve the mana ability.
pub fn handle_tap_creatures_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_creatures: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must tap exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for mana ability cost".to_string(),
            ));
        }
    }

    let mut updated = pending.clone();
    updated.chosen_tappers = chosen.to_vec();
    advance_mana_ability_activation(state, updated, events)
}

/// CR 117.1 + CR 118.3 + CR 605.3b + CR 400.7j: Complete a non-self exile
/// mana-ability cost selection. Captures the cost-paid object's public
/// characteristics before the cost is paid, then resumes the activation flow.
pub fn handle_exile_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must exile exactly {} card(s), got {}",
            count,
            chosen.len()
        )));
    }
    if contains_duplicate_object_id(chosen) {
        return Err(EngineError::InvalidAction(
            "Cannot exile the same card more than once for a mana ability cost".to_string(),
        ));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible for mana ability exile cost".to_string(),
            ));
        }
        // CR 118.10: One payment cannot apply to both this mana ability cost
        // and a pending spell sacrifice cost.
        if deferred_spell_sacrifice_reserved(state, *id) {
            return Err(EngineError::InvalidAction(
                "Selected card is already committed to a spell sacrifice cost".to_string(),
            ));
        }
    }

    // CR 117.1 + CR 400.7j + CR 608.2k: Capture the cost-paid object's public
    // characteristics before it leaves its zone.
    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });

    let mut updated = pending.clone();
    updated.chosen_exiled = chosen.to_vec();
    updated.cost_paid_object = captured;
    advance_mana_ability_activation(state, updated, events)
}

/// CR 117.1 + CR 118.3 + CR 605.3b + CR 202.3: Complete the
/// sacrifice-from-battlefield mana-ability cost selection (Phyrexian Altar class).
/// Captures the cost-paid object's public characteristics before sacrifice so
/// mana production can reference the sacrificed object's mana value when needed.
pub fn handle_sacrifice_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_permanents: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must sacrifice exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for mana ability sacrifice cost".to_string(),
            ));
        }
        // CR 118.10: One payment cannot apply to both this mana ability cost
        // and a pending spell sacrifice cost.
        if deferred_spell_sacrifice_reserved(state, *id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent is already committed to a spell sacrifice cost".to_string(),
            ));
        }
    }

    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });

    let mut updated = pending.clone();
    updated.chosen_sacrificed_battlefield = chosen.to_vec();
    updated.cost_paid_object = captured;
    advance_mana_ability_activation(state, updated, events)
}

fn deferred_spell_sacrifice_reserved(state: &GameState, object_id: ObjectId) -> bool {
    state.pending_cast.as_ref().is_some_and(|pending| {
        pending
            .deferred_sacrificed_permanents
            .iter()
            .any(|selection| selection.object_id == object_id)
    })
}

pub fn handle_discard_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_cards: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must discard exactly {} card(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible for mana ability cost".to_string(),
            ));
        }
    }

    let mut updated = pending.clone();
    updated.chosen_discards = chosen.to_vec();
    advance_mana_ability_activation(state, updated, events)
}

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static MANA_READINESS_CALLS: AtomicUsize = AtomicUsize::new(0);

/// CR 602.5 + CR 605.3a: True iff this mana ability is activatable right now
/// using only non-simulating, recursion-free gates — the simulation-free prefix
/// of `activate_mana_ability`. Single authority shared by the
/// `can_activate_mana_ability_now` pre-clone gate and the `batch_eligible_siblings`
/// sibling filter, so both agree on readiness without each cloning + recursing
/// the whole game state (the O(N!) cause when N batchable sources are present).
/// CR 602.5 + CR 604.1: hoistable existence gates for the two whole-battlefield
/// activation-prohibition scans inside [`mana_ability_ready_without_simulation`].
///
/// `is_blocked_by_cant_be_activated` (CR 602.5, City of Solitude class) and
/// `is_blocked_by_cant_activate_during` (CR 117.1b) each iterate every
/// battlefield static. Calling them per mana source turns the board-global mana
/// availability sweep into O(N^2) (~700 Cryptolith-Rite tokens × ~700 statics).
/// Computing presence ONCE and gating each scan collapses the sweep to O(N) when
/// no such static exists (the overwhelming common case). Mirrors
/// `combat::CombatStaticGates`. Uses `game_functioning_statics` (a superset of
/// the precise `battlefield_active_statics` the scans use) so a `false` gate is a
/// sound skip; a `true` gate falls through to the exact per-source scan.
#[derive(Debug, Clone, Copy)]
pub struct ManaActivationGates {
    has_cant_be_activated: bool,
    has_cant_activate_during: bool,
}

impl ManaActivationGates {
    /// Reads both presence flags from the O(1) `StaticModePresence` index (Unit 1)
    /// instead of sweeping `game_functioning_statics`. A post-flush-precise superset:
    /// a spurious `true` falls through to the exact per-source scan.
    pub fn compute(state: &GameState) -> Self {
        ManaActivationGates {
            has_cant_be_activated: static_kind_present(state, StaticModeKind::CantBeActivated),
            has_cant_activate_during: static_kind_present(
                state,
                StaticModeKind::CantActivateDuring,
            ),
        }
    }
}

fn mana_ability_ready_without_simulation(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    // Single-call entry: compute the gates once (one battlefield scan) and
    // delegate. The board-sweep caller (`derive_display_state`) hoists the gates
    // across all sources via `..._gated` instead.
    let gates = ManaActivationGates::compute(state);
    mana_ability_ready_without_simulation_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        &gates,
    )
}

fn mana_ability_ready_without_simulation_gated(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    gates: &ManaActivationGates,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    // CR 702.26b: Phased-out permanents are treated as though they do not
    // exist, so they cannot activate abilities.
    if obj.is_phased_out_permanent() {
        return false;
    }
    // CR 701.35a: Detained permanents' activated abilities can't be activated.
    if !obj.detained_by.is_empty() {
        return false;
    }
    // CR 602.2a: Only the controller may activate the ability.
    if obj.controller != player {
        return false;
    }
    // CR 113.6 + CR 113.6b: A permanent's abilities function only on the battlefield by
    // default; an ability that states which zones it functions in (activation_zone, e.g.
    // Hand/Graveyard mana abilities) functions only from those zones.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return false;
    }
    // CR 106.12 + CR 602.5a: A tap-cost mana ability requires an untapped source.
    // Gated on has_tap_component so no-tap sacrifice altars stay activatable while tapped.
    if mana_sources::has_tap_component(&ability_def.cost) && obj.tapped {
        return false;
    }
    // CR 701.26a + CR 508.1f: a "can't become tapped" source (e.g. a goaded mana
    // dork) can't activate a tap-cost mana ability. A {Q} untap-cost ability is
    // unaffected — untapping is governed by `StaticMode::CantUntap`.
    if mana_sources::has_tap_component(&ability_def.cost)
        && crate::game::restrictions::object_cant_tap(state, source_id)
    {
        return false;
    }
    // CR 107.6: A {Q}-cost mana ability requires a currently-tapped source — an
    // already-untapped permanent can't be untapped to pay the cost (Pili-Pala).
    if mana_sources::has_untap_component(&ability_def.cost) && !obj.tapped {
        return false;
    }
    // CR 302.6 + CR 602.5a: a {T}- or {Q}-cost mana ability on a creature that
    // hasn't been controlled since the start of its controller's most recent turn
    // can't be activated (CR 302.6 names both symbols). Haste /
    // CanActivateAbilitiesAsThoughHaste lift it via the shared predicate.
    if (mana_sources::has_tap_component(&ability_def.cost)
        || mana_sources::has_untap_component(&ability_def.cost))
        && super::restrictions::summoning_sick_for_tap_ability(state, obj)
    {
        return false;
    }
    // CR 602.5: CantBeActivated (City of Solitude class) blocks activation.
    // CR 604.1: gated existence check hoisted across the board sweep — the
    // per-source full-battlefield scan only runs when such a static exists.
    if gates.has_cant_be_activated
        && super::casting::is_blocked_by_cant_be_activated(state, player, source_id, ability_def)
    {
        return false;
    }
    // CR 602.5 + CR 117.1b: CantActivateDuring blocks activation this turn.
    if gates.has_cant_activate_during
        && super::casting::is_blocked_by_cant_activate_during(state, player, ability_def)
    {
        return false;
    }
    // CR 604 + CR 605.3b: Static activation restrictions must currently hold.
    if super::restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )
    .is_err()
    {
        return false;
    }
    // CR 605.3a + CR 601.2h: The mana sub-cost (pool + choice-of-object) must be
    // currently payable. is_payable_for_mana_ability's Mana arm uses auto_tap with
    // require_current_payability=false, so it does not recurse here.
    if let Some(cost) = &ability_def.cost {
        if !cost.is_payable_for_mana_ability(state, player, source_id) {
            return false;
        }
    }
    true
}

pub fn can_activate_mana_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    // Single-call entry: compute the activation-prohibition gates once and
    // delegate. Board-wide sweeps use `..._gated` to hoist them across sources.
    let gates = ManaActivationGates::compute(state);
    can_activate_mana_ability_now_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        &gates,
    )
}

/// Gated variant of [`can_activate_mana_ability_now`]: the caller supplies
/// once-computed [`ManaActivationGates`] so a board-global mana sweep does not
/// re-scan the battlefield for activation-prohibition statics per source.
pub fn can_activate_mana_ability_now_gated(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
    gates: &ManaActivationGates,
) -> bool {
    #[cfg(test)]
    MANA_READINESS_CALLS.fetch_add(1, Ordering::Relaxed);

    if !mana_ability_ready_without_simulation_gated(
        state,
        player,
        source_id,
        ability_index,
        ability_def,
        gates,
    ) {
        return false;
    }
    // CR 605.3a + CR 106.12 + CR 107.6: When the cheap gate already conclusively
    // decides payability (no cost, or a {T}/{Q}-only cost whose production +
    // payment path is infallible), skip the full-state-clone legality
    // simulation. Eliminates the mana-display board-sweep clone-storm (Cryptolith
    // Rite granting bare `{T}: Add` to ~700 tokens => ~700 clones/sweep). Mana/
    // resource/composite costs still simulate — the auto-tap affordability
    // witness (CR 601.2g) must not flip UNAVAILABLE->AVAILABLE.
    if mana_sources::cost_conclusively_payable_by_cheap_gate(&ability_def.cost) {
        return true;
    }
    can_activate_mana_ability_by_simulation(state, player, source_id, ability_index, ability_def)
}

fn can_activate_mana_ability_by_simulation(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    crate::game::perf_counters::record_state_clone_for_legality();
    let mut simulated = state.clone();
    activate_mana_ability(
        &mut simulated,
        source_id,
        player,
        ability_index,
        ability_def,
        &mut Vec::new(),
        ManaAbilityResume::Priority,
        None,
    )
    .is_ok()
}

// CR 701.59: collect-evidence amount inside a (possibly composite) mana-ability cost.
pub(crate) fn collect_evidence_cost_amount(cost: &AbilityCost) -> Option<u32> {
    match cost {
        AbilityCost::CollectEvidence { amount } => Some(*amount),
        AbilityCost::Composite { costs } => costs.iter().find_map(collect_evidence_cost_amount),
        _ => None,
    }
}

// CR 107.3a + CR 702.179f: a mana-ability cost of "Pay X speed".
fn pay_speed_x_cost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PaySpeed {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
        } if name == "X" => true,
        AbilityCost::Composite { costs } => costs.iter().any(pay_speed_x_cost),
        _ => false,
    }
}

pub(super) fn advance_mana_ability_activation(
    state: &mut GameState,
    pending: PendingManaAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ability_def = mana_ability_definition(state, &pending)?;

    // CR 107.3a + CR 601.2b + CR 702.179f: A `Pay X speed` mana-ability cost
    // (Chicago Loop's `Pay X speed: Add X mana in any combination of colors`)
    // requires the player to announce X before any cost is paid or mana is
    // produced. X is bound to BOTH the speed cost and the produced-mana count.
    if pending.chosen_x.is_none() {
        if let Some(cost) = &ability_def.cost {
            if pay_speed_x_cost(cost) {
                // CR 118.3: a player can't pay a cost without the resources to
                // pay it fully, so X is bounded by the player's current speed.
                // CR 702.179f: a player with no speed has speed 0.
                let max = super::speed::effective_speed(state, pending.player) as u32;
                let source_id = pending.source_id;
                let player = pending.player;
                return Ok(WaitingFor::PayAmountChoice {
                    player,
                    resource: PayableResource::Speed,
                    min: 0,
                    max,
                    accumulated: 0,
                    source_id,
                    pending_mana_ability: Some(Box::new(pending)),
                });
            }
        }
    }

    if pending.chosen_discards.is_empty() {
        if let Some((count, cards)) =
            discard_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if cards.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard for mana ability".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::Discard,
                choices: cards,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    if pending.chosen_tappers.is_empty() {
        if let Some((count, creatures)) =
            tap_creature_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if creatures.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough untapped creatures to pay mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::TapCreatures { aggregate: None },
                choices: creatures,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 117.1 + CR 118.3 + CR 400.7j: Non-self exile as a mana ability cost.
    // Library costs are deterministic top-card payment, so prepare their
    // selected objects and cost-paid snapshot before any mana output prompt.
    if pending.chosen_exiled.is_empty() {
        if let Some(updated) =
            prepare_deterministic_exile_cost_selection(state, &pending, &ability_def.cost)?
        {
            return advance_mana_ability_activation(state, updated, events);
        }
    }

    // CR 117.1 + CR 118.3: Interactive non-self exile costs (Food Chain,
    // Titans' Nest) choose objects before producing mana so the cost-paid
    // object's public characteristics can be captured at payment time.
    if pending.chosen_exiled.is_empty() {
        if let Some((count, zone, cards)) =
            exile_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if cards.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible cards to exile for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::ExileFromManaZone { zone },
                choices: cards,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 117.1 + CR 118.3: Non-self sacrifice-from-battlefield as a mana
    // ability cost (Phyrexian Altar class). Surface the player choice before
    // producing mana so the selected permanent is sacrificed as the cost.
    if pending.chosen_sacrificed_battlefield.is_empty() {
        if let Some((count, permanents)) =
            sacrifice_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            let permanents: Vec<ObjectId> = permanents
                .into_iter()
                .filter(|id| !deferred_spell_sacrifice_reserved(state, *id))
                .collect();
            if permanents.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to sacrifice for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player: pending.player,
                kind: PayCostKind::Sacrifice,
                choices: permanents,
                count,
                min_count: 0,
                resume: CostResume::ManaAbility {
                    mana_ability: Box::new(pending),
                },
            });
        }
    }

    // CR 605.2 + CR 701.59: "Collect evidence N" in a (possibly composite)
    // mana-ability cost (Cryptex) requires interactively exiling graveyard
    // cards before any mana is produced. Surface the choice via the shared
    // CollectEvidenceChoice prompt, resuming this activation once cards are
    // chosen. Keyed on not-yet-collected (empty selection).
    if pending.collected_evidence.is_empty() {
        if let Some(cost) = &ability_def.cost {
            if let Some(amount) = collect_evidence_cost_amount(cost) {
                // CR 605.2 + CR 605.3b: pay the ability's cost before producing mana.
                if !super::effects::collect_evidence::can_collect_evidence(
                    state,
                    pending.player,
                    amount,
                ) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay collect-evidence cost for mana ability".to_string(),
                    ));
                }
                return Ok(
                    super::effects::collect_evidence::begin_cost_payment_for_mana_ability(
                        state,
                        pending.player,
                        amount,
                        pending,
                    ),
                );
            }
        }
    }

    // CR 107.1c + CR 605.3a: "Remove any number of <type> counters" in a
    // mana-ability cost requires choosing the count before costs are paid and
    // mana is produced.
    if pending.chosen_counter_count.is_none() {
        if let Some(counter_type) = any_number_self_remove_counter_cost(&ability_def.cost) {
            let max = removable_counter_count_for_mana_cost(state, pending.source_id, counter_type);
            return Ok(WaitingFor::PayAmountChoice {
                player: pending.player,
                resource: PayableResource::Counters,
                min: 0,
                max,
                accumulated: 0,
                source_id: pending.source_id,
                pending_mana_ability: Some(Box::new(pending)),
            });
        }
    }

    // CR 605.3a + CR 602.2b + CR 601.2g-h + CR 107.4e: Resolve the mana
    // sub-cost payment before producing any mana or prompting for output
    // choices. If the current pool already offers multiple hybrid assignments,
    // surface `PayManaAbilityMana` so the player picks. If the pool cannot
    // cover the sub-cost yet, fall through to the real payment site, which may
    // activate other mana abilities while paying this activation cost (CR
    // 117.1d / CR 118.2).
    if pending.chosen_mana_payment.is_none() {
        if let Some(sub_cost) = mana_sub_cost_of(&ability_def.cost) {
            let (source_types, source_subtypes) =
                super::casting::activation_source_types(state, pending.source_id);
            let activation_ctx = PaymentContext::Activation {
                source_types: &source_types,
                source_subtypes: &source_subtypes,
                ability_tag: None,
            };
            let pool = &state.players[pending.player.0 as usize].mana_pool;
            let plans = enumerate_hybrid_payment_plans(pool, sub_cost, &activation_ctx);
            match plans.len() {
                0 if {
                    let excluded_sources = std::collections::HashSet::from([pending.source_id]);
                    !super::casting::can_pay_ability_mana_cost_after_auto_tap_excluding(
                        state,
                        pending.player,
                        pending.source_id,
                        sub_cost,
                        &excluded_sources,
                    )
                } =>
                {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay mana cost for mana ability".to_string(),
                    ));
                }
                0 => {}
                1 => {
                    let mut updated = pending;
                    updated.chosen_mana_payment = Some(plans.into_iter().next().unwrap());
                    return advance_mana_ability_activation(state, updated, events);
                }
                _ => {
                    return Ok(WaitingFor::PayManaAbilityMana {
                        player: pending.player,
                        options: plans,
                        pending_mana_ability: Box::new(pending),
                    });
                }
            }
        }
    }

    // CR 601.2h + CR 602.2b + CR 616.1: The activation owns a serialized
    // component cursor while any replaceable cost move is paused. It never
    // re-enters this choice-discovery prefix after the player answers a
    // replacement choice, so paid components and selected objects stay paid.
    let cost_event_start = events.len();
    continue_mana_ability_cost_payment(
        state,
        pending,
        mana_ability_cost_cursor(
            &ability_def.cost,
            &HashSet::new(),
            None,
            ManaAbilityCostResolutionMode::Interactive,
            None,
        ),
        events,
        cost_event_start,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManaAbilityPaymentProgress {
    Complete,
    Paused,
}

fn mana_ability_definition(
    state: &GameState,
    pending: &PendingManaAbility,
) -> Result<AbilityDefinition, EngineError> {
    state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| obj.abilities.get(pending.ability_index))
        .cloned()
        .or_else(|| pending.ability_snapshot.clone())
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))
}

fn mana_ability_cost_cursor(
    cost: &Option<AbilityCost>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    resolution_mode: ManaAbilityCostResolutionMode,
    parent: Option<&ManaAbilityCostParent>,
) -> ManaAbilityCostCursor {
    let mut remaining = Vec::new();
    if let Some(cost) = cost {
        append_mana_ability_cost_components(cost, &mut remaining);
    }
    let mut excluded_sources: Vec<_> = excluded_sources.iter().copied().collect();
    excluded_sources.sort_unstable_by_key(|source_id| source_id.0);
    ManaAbilityCostCursor {
        remaining,
        resolution_mode,
        excluded_sources,
        sub_cost_demand: sub_cost_demand.copied(),
        next_tapper: 0,
        next_discard: 0,
        next_exiled: 0,
        next_sacrificed: 0,
        selected_exile_remaining: None,
        selected_sacrifice_remaining: None,
        // CR 603.2 + CR 603.3b: A nested child takes over every unscanned
        // event already owned by its suspended parent.  It appends only newly
        // emitted events if it pauses, then hands the one batch back on
        // completion; no ancestor batch is copied twice.
        deferred_cost_events: parent.map_or_else(Vec::new, |parent| {
            parent.cursor.deferred_cost_events.clone()
        }),
        current_action_deferred_start: 0,
        parent: parent.cloned().map(Box::new),
    }
}

fn append_mana_ability_cost_components(cost: &AbilityCost, remaining: &mut Vec<AbilityCost>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for cost in costs {
                append_mana_ability_cost_components(cost, remaining);
            }
        }
        cost => remaining.push(cost.clone()),
    }
}

fn promote_cost_move_resume(pending: &mut PendingManaAbility) {
    if let Some(resume) = pending.cost_move_resume.take() {
        pending.resume = resume;
    }
}

fn promote_cost_move_resume_chain(
    pending: &mut PendingManaAbility,
    parent: Option<&mut ManaAbilityCostParent>,
) {
    promote_cost_move_resume(pending);
    if let Some(parent) = parent {
        promote_cost_move_resume_chain(&mut parent.pending, parent.cursor.parent.as_deref_mut());
    }
}

fn suspend_mana_ability_parent_chain(parent: Option<&mut ManaAbilityCostParent>) {
    if let Some(parent) = parent {
        parent.lifecycle = ManaAbilityCostParentLifecycle::Suspended;
        suspend_mana_ability_parent_chain(parent.cursor.parent.as_deref_mut());
    }
}

fn pause_mana_ability_cost_payment(
    state: &mut GameState,
    pre_delivery_choice_player: Option<PlayerId>,
    mut pending: PendingManaAbility,
    mut cursor: ManaAbilityCostCursor,
    events: &[GameEvent],
    cost_event_start: usize,
) {
    // CR 605.3b + CR 616.1: Promote the typed root only on the paused path.
    // In particular, a synchronously completed auto-tap must return normally
    // so its caller can spend the mana rather than recursively retrying.
    promote_cost_move_resume_chain(&mut pending, cursor.parent.as_deref_mut());
    // CR 605.3b + CR 605.3c: Only a pause unwinds the parent's synchronous
    // call frame. Mark the complete parent chain so the resumed child, and no
    // synchronously completed child, takes ownership of continuing it.
    suspend_mana_ability_parent_chain(cursor.parent.as_deref_mut());
    cursor.current_action_deferred_start = cursor.deferred_cost_events.len();
    cursor
        .deferred_cost_events
        .extend_from_slice(&events[cost_event_start..]);
    state.pending_cost_move_resume = Some(PendingCostMoveResume::ManaAbilityPayment {
        pending: Box::new(pending),
        cursor,
    });
    // `NeedsChoice` also reports a post-delivery replacement continuation.
    // That continuation has already consumed `pending_replacement` and installed
    // its own live prompt, which must remain visible while the typed cursor is
    // parked. Pre-delivery CR 616.1 ordering is the only case that needs a
    // synthesized ReplacementChoice.
    if state.pending_replacement.is_some() {
        super::costs::pause_cost_payment_for_replacement_choice(
            state,
            pre_delivery_choice_player.expect(
                "a pending replacement must identify the player ordering the interrupted mana cost",
            ),
        );
    }
}

fn mana_ability_cursor_after_current_component(
    cursor: &ManaAbilityCostCursor,
) -> ManaAbilityCostCursor {
    let mut resumed = cursor.clone();
    resumed.remaining.remove(0);
    resumed
}

fn ensure_mana_ability_selection_cursor_consumed(
    pending: &PendingManaAbility,
    cursor: &ManaAbilityCostCursor,
) -> Result<(), EngineError> {
    if cursor.next_tapper != pending.chosen_tappers.len() {
        return Err(EngineError::InvalidAction(
            "Too many creatures selected for mana ability cost".to_string(),
        ));
    }
    if cursor.next_exiled != pending.chosen_exiled.len() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability exile cost".to_string(),
        ));
    }
    if cursor.next_discard != pending.chosen_discards.len() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability cost".to_string(),
        ));
    }
    if cursor.next_sacrificed != pending.chosen_sacrificed_battlefield.len() {
        return Err(EngineError::InvalidAction(
            "Too many permanents selected for mana ability sacrifice cost".to_string(),
        ));
    }
    Ok(())
}

fn advance_mana_ability_selection_cursor(
    cursor: &mut ManaAbilityCostCursor,
    cost: &AbilityCost,
    paid_discard_count: Option<usize>,
) -> Result<(), EngineError> {
    match cost {
        AbilityCost::TapCreatures { requirement, .. } => {
            cursor.next_tapper += requirement.fixed_count().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Aggregate-power tap cost is not valid for a mana ability".to_string(),
                )
            })? as usize;
        }
        AbilityCost::Discard { self_scope, .. } if !self_scope.is_source_card() => {
            cursor.next_discard += paid_discard_count.expect(
                "mana ability discard count must be captured before moving the selected cards",
            );
        }
        AbilityCost::Sacrifice(cost) if !matches!(cost.target, TargetFilter::SelfRef) => {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                return Err(EngineError::InvalidAction(
                    "Unsupported sacrifice cost requirement for mana ability".to_string(),
                ));
            };
            cursor.next_sacrificed += count as usize;
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pay_selected_mana_ability_exile_cost(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    count: u32,
    zone: Option<Zone>,
    filter: Option<&TargetFilter>,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    let effective_zone = exile_cost_effective_zone(zone, filter);
    if effective_zone == Zone::Library && filter.is_some() {
        return Err(EngineError::InvalidAction(
            "Unsupported filtered library exile cost for mana ability".to_string(),
        ));
    }
    if cursor.selected_exile_remaining.is_none() {
        let end = cursor.next_exiled + count as usize;
        let selected = pending
            .chosen_exiled
            .get(cursor.next_exiled..end)
            .ok_or_else(|| {
                EngineError::InvalidAction(
                    "Missing exiled card selection for mana ability".to_string(),
                )
            })?;
        if contains_duplicate_object_id(selected) {
            return Err(EngineError::InvalidAction(
                "Cannot exile the same card more than once for a mana ability cost".to_string(),
            ));
        }
        let legal = eligible_exile_cost_objects(
            state,
            pending.player,
            pending.source_id,
            effective_zone,
            filter,
            count,
        );
        if effective_zone == Zone::Library {
            if selected != legal {
                return Err(EngineError::ActionNotAllowed(
                    "Selected cards are no longer on top of your library".to_string(),
                ));
            }
        } else if selected.iter().any(|object_id| {
            deferred_spell_sacrifice_reserved(state, *object_id) || !legal.contains(object_id)
        }) {
            return Err(EngineError::ActionNotAllowed(
                "Selected card does not match the exile cost".to_string(),
            ));
        }
        cursor.next_exiled = end;
        cursor.selected_exile_remaining = Some(selected.to_vec());
    }

    while let Some(object_id) = cursor
        .selected_exile_remaining
        .as_ref()
        .and_then(|remaining| remaining.first())
        .copied()
    {
        if object_id == pending.source_id {
            return Err(EngineError::ActionNotAllowed(
                "Source cannot satisfy its own exile cost".to_string(),
            ));
        }
        cursor
            .selected_exile_remaining
            .as_mut()
            .expect("selected exile cursor was checked above")
            .remove(0);
        match zone_pipeline::move_object(
            state,
            ZoneMoveRequest::cost(object_id, Zone::Exile, pending.source_id),
            events,
        ) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsChoice(choice_player) => {
                pause_mana_ability_cost_payment(
                    state,
                    Some(choice_player),
                    pending,
                    cursor.clone(),
                    events,
                    cost_event_start,
                );
                return Ok(ManaAbilityPaymentProgress::Paused);
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                unreachable!("a cost move to Exile cannot require Aura attachment")
            }
        }
    }
    cursor.selected_exile_remaining = None;
    Ok(ManaAbilityPaymentProgress::Complete)
}

/// CR 601.2h + CR 605.3b + CR 616.1: Pay a selected sacrifice component from
/// the mana-ability cursor. The selected list is consumed before proposing each
/// sacrifice, so a replacement-choice resume cannot re-sacrifice the object
/// whose move the replacement action just settled.
#[allow(clippy::too_many_arguments)]
fn pay_selected_mana_ability_sacrifice_cost(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    count: u32,
    filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    if cursor.selected_sacrifice_remaining.is_none() {
        let end = cursor.next_sacrificed + count as usize;
        let selected = pending
            .chosen_sacrificed_battlefield
            .get(cursor.next_sacrificed..end)
            .ok_or_else(|| {
                EngineError::InvalidAction(
                    "Missing sacrificed permanent selection for mana ability".to_string(),
                )
            })?;
        if contains_duplicate_object_id(selected) {
            return Err(EngineError::InvalidAction(
                "Cannot sacrifice the same permanent more than once for a mana ability cost"
                    .to_string(),
            ));
        }
        cursor.next_sacrificed = end;
        cursor.selected_sacrifice_remaining = Some(selected.to_vec());
    }

    while let Some(object_id) = cursor
        .selected_sacrifice_remaining
        .as_ref()
        .and_then(|remaining| remaining.first())
        .copied()
    {
        cursor
            .selected_sacrifice_remaining
            .as_mut()
            .expect("selected sacrifice cursor was checked above")
            .remove(0);
        match sacrifice_selected_permanent_for_mana_cost(
            state,
            pending.source_id,
            pending.player,
            object_id,
            filter,
            events,
        )? {
            sacrifice::SacrificeOutcome::Complete => {}
            sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                pause_mana_ability_cost_payment(
                    state,
                    Some(choice_player),
                    pending,
                    cursor.clone(),
                    events,
                    cost_event_start,
                );
                return Ok(ManaAbilityPaymentProgress::Paused);
            }
        }
    }
    cursor.selected_sacrifice_remaining = None;
    Ok(ManaAbilityPaymentProgress::Complete)
}

fn pay_mana_ability_cost_component(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: &mut ManaAbilityCostCursor,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<ManaAbilityPaymentProgress, EngineError> {
    match cost {
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone,
            count: 1,
        } => {
            let required_zone = zone.unwrap_or(Zone::Battlefield);
            let source = state.objects.get(&pending.source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exile cost".to_string())
            })?;
            if source.zone != required_zone {
                return Err(EngineError::ActionNotAllowed(format!(
                    "Cannot exile from {:?}: source is not in that zone",
                    required_zone
                )));
            }
            match zone_pipeline::move_object(
                state,
                ZoneMoveRequest::cost(pending.source_id, Zone::Exile, pending.source_id),
                events,
            ) {
                ZoneMoveResult::Done => Ok(ManaAbilityPaymentProgress::Complete),
                ZoneMoveResult::NeedsChoice(choice_player) => {
                    pause_mana_ability_cost_payment(
                        state,
                        Some(choice_player),
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => {
                    unreachable!("a cost move to Exile cannot require Aura attachment")
                }
            }
        }
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if !matches!(filter, Some(TargetFilter::SelfRef)) => {
            pay_selected_mana_ability_exile_cost(
                state,
                pending,
                cursor,
                *count,
                *zone,
                filter.as_ref(),
                events,
                cost_event_start,
            )
        }
        AbilityCost::Sacrifice(cost)
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            if deferred_spell_sacrifice_reserved(state, pending.source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "This permanent is already committed to a spell sacrifice cost".to_string(),
                ));
            }
            if super::static_abilities::player_cant_sacrifice_as_cost(
                state,
                pending.player,
                pending.source_id,
            ) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot sacrifice this permanent as a cost".to_string(),
                ));
            }
            match sacrifice::sacrifice_permanent(state, pending.source_id, pending.player, events)?
            {
                sacrifice::SacrificeOutcome::Complete => Ok(ManaAbilityPaymentProgress::Complete),
                sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                    pause_mana_ability_cost_payment(
                        state,
                        Some(choice_player),
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
            }
        }
        AbilityCost::Sacrifice(cost)
            if !matches!(cost.target, TargetFilter::SelfRef)
                && matches!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { .. }
                ) =>
        {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                unreachable!("guarded above");
            };
            pay_selected_mana_ability_sacrifice_cost(
                state,
                pending,
                cursor,
                count,
                &cost.target,
                events,
                cost_event_start,
            )
        }
        cost if is_self_contained_mana_subcost(cost) => {
            match super::costs::pay_ability_cost_for_activation(
                state,
                pending.player,
                pending.source_id,
                cost,
                None,
                events,
            )? {
                super::costs::PaymentOutcome::Paid => Ok(ManaAbilityPaymentProgress::Complete),
                super::costs::PaymentOutcome::Paused { .. } => {
                    let Some(PendingCostMoveResume::Cast { .. }) =
                        state.pending_cost_move_resume.take()
                    else {
                        unreachable!(
                            "a paused delegated mana-ability cost must retain the activation cost move"
                        );
                    };
                    let pre_delivery_choice_player = state
                        .pending_replacement
                        .is_some()
                        .then(|| state.waiting_for.acting_player())
                        .flatten();
                    pause_mana_ability_cost_payment(
                        state,
                        pre_delivery_choice_player,
                        pending,
                        mana_ability_cursor_after_current_component(cursor),
                        events,
                        cost_event_start,
                    );
                    Ok(ManaAbilityPaymentProgress::Paused)
                }
                super::costs::PaymentOutcome::Failed { reason } => {
                    Err(EngineError::ActionNotAllowed(reason.reason))
                }
            }
        }
        cost => {
            // CR 601.2h: A dynamic discard cost is measured before its cards
            // leave the hand, so retain that paid count for the cursor.
            let paid_discard_count = match cost {
                AbilityCost::Discard {
                    count, self_scope, ..
                } if !self_scope.is_source_card() => Some(
                    super::quantity::resolve_quantity(
                        state,
                        count,
                        pending.player,
                        pending.source_id,
                    )
                    .max(0) as usize,
                ),
                _ => None,
            };
            let excluded_sources = cursor.excluded_sources.iter().copied().collect();
            let mut tappers = pending
                .chosen_tappers
                .iter()
                .copied()
                .skip(cursor.next_tapper);
            let mut discards = pending
                .chosen_discards
                .iter()
                .copied()
                .skip(cursor.next_discard);
            let mut sacrificed = pending
                .chosen_sacrificed_battlefield
                .iter()
                .copied()
                .skip(cursor.next_sacrificed);
            // CR 605.3b + CR 605.3c: A nested source that pauses while
            // funding this Mana component must retain this exact parent cursor.
            // The child cursor inherits the parent's existing deferred batch
            // when it is constructed.  Do not also copy this action's events
            // into the parent: if the child pauses, its own cursor appends them
            // exactly once and becomes the temporary settlement authority.
            let parent_cursor = cursor.clone();
            let parent = ManaAbilityCostParent {
                pending: Box::new(pending.clone()),
                cursor: Box::new(parent_cursor),
                lifecycle: ManaAbilityCostParentLifecycle::Synchronous,
            };
            if let Err(error) = pay_mana_ability_cost_with_choices(
                state,
                pending.source_id,
                pending.player,
                &Some(cost.clone()),
                events,
                &mut tappers,
                &mut discards,
                &mut sacrificed,
                pending.chosen_mana_payment.as_deref(),
                pending.chosen_counter_count,
                pending.chosen_x,
                &excluded_sources,
                cursor.sub_cost_demand.as_ref(),
                Some(&parent),
            ) {
                // CR 605.3b + CR 605.3c + CR 616.1: A nested mana source
                // paused on a replacement-aware cost. Its serialized cursor
                // owns this exact parent; do not turn that valid suspension
                // into a failed parent mana payment.
                if super::casting::mana_ability_cost_payment_is_paused(state) {
                    return Ok(ManaAbilityPaymentProgress::Paused);
                }
                return Err(error);
            }
            advance_mana_ability_selection_cursor(cursor, cost, paid_discard_count)?;
            Ok(ManaAbilityPaymentProgress::Complete)
        }
    }
}

fn finish_mana_ability_cost_payment(
    state: &mut GameState,
    mut pending: PendingManaAbility,
    cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let resolves_automatically = matches!(
        cursor.resolution_mode,
        ManaAbilityCostResolutionMode::AutoResolved
    );
    let has_deferred_cost_events = !cursor.deferred_cost_events.is_empty();
    let parent = cursor
        .parent
        .clone()
        .filter(|parent| matches!(parent.lifecycle, ManaAbilityCostParentLifecycle::Suspended));
    let ability_def = mana_ability_definition(state, &pending)?;
    if !resolves_automatically && pending.color_override.is_none() {
        let resolved_for_prompt = resolved_mana_ability_for_current_state(
            state,
            pending.source_id,
            pending.player,
            &ability_def,
            pending.chosen_x,
            pending.cost_paid_object.clone(),
        );
        if let Some(choice) = mana_choice_prompt(
            &resolved_for_prompt.effect,
            state,
            pending.source_id,
            Some(&resolved_for_prompt),
        ) {
            if matches!(choice, ManaChoicePrompt::SingleColor { .. })
                && cost_resolves_without_choice(&ability_def.cost)
            {
                pending.batch_siblings =
                    batch_eligible_siblings(state, pending.player, pending.source_id, &ability_def);
            }
            let resume = WaitingFor::ChooseManaColor {
                player: pending.player,
                choice,
                context: ManaChoiceContext::ManaAbility(Box::new(pending)),
            };
            if has_deferred_cost_events {
                settle_mana_ability_cost_events(
                    state,
                    cursor.deferred_cost_events,
                    events,
                    cost_event_start,
                );
                if let Some(order_wf) =
                    super::triggers::preserve_order_triggers_resume(state, resume.clone())
                {
                    return Ok(order_wf);
                }
            } else if events.len() > cost_event_start {
                // CR 603.2 + CR 603.3b + CR 605.3b: A mana-color choice
                // returns before the ordinary post-action pipeline runs.
                // Its already-paid cost events therefore need their one
                // normal trigger collection here. Replacement-paused events
                // are deliberately excluded: their typed cursor owns them
                // until `settle_mana_ability_cost_events` above.
                let cost_events = events[cost_event_start..].to_vec();
                super::triggers::process_triggers(state, &cost_events);
            }
            return Ok(resume);
        }
    }

    produce_mana_from_ability(
        state,
        pending.source_id,
        pending.player,
        &ability_def,
        events,
        pending.color_override.clone(),
        pending.chosen_x,
        pending.cost_paid_object,
    );
    if !resolves_automatically {
        complete_mana_ability_activation(
            state,
            pending.source_id,
            pending.ability_index,
            pending.player,
            events,
        );
    }
    if let Some(parent) = parent {
        let mut parent_cursor = *parent.cursor;
        // CR 603.2 + CR 603.3b: The child now owns the complete old batch;
        // append its current action exactly once, then transfer that batch to
        // the parent before it retries its still-unpaid Mana component.
        parent_cursor.deferred_cost_events = cursor.deferred_cost_events;
        parent_cursor
            .deferred_cost_events
            .extend_from_slice(&events[cost_event_start..]);
        let parent_event_start = events.len();
        return continue_mana_ability_cost_payment(
            state,
            *parent.pending,
            parent_cursor,
            events,
            parent_event_start,
        );
    }
    let resume = resume_mana_ability_root(state, pending.player, pending.resume, events)?;
    if super::casting::mana_ability_cost_payment_is_paused(state) {
        defer_cost_events_into_active_mana_root(
            state,
            cursor.deferred_cost_events,
            &events[cost_event_start..],
        );
        return Ok(resume);
    }

    if has_deferred_cost_events {
        settle_mana_ability_cost_events(
            state,
            cursor.deferred_cost_events,
            events,
            cost_event_start,
        );
        return Ok(
            super::triggers::preserve_order_triggers_resume(state, resume.clone())
                .unwrap_or(resume),
        );
    }

    // CR 603.2 + CR 603.3b + CR 605.3b: A direct (non-paused) mana-ability
    // action normally reaches the ordinary post-action trigger pipeline, which
    // remains its settlement authority. But that pipeline only runs for a
    // `Priority` resume (it is guarded by `waiting_for == Priority`). A mana
    // ability activated during mana payment or an unless-cost payment resumes
    // to `ManaPayment`/`UnlessPayment`, which the pipeline skips — so its
    // already-paid cost events (sacrifice, discard, exile) would never be
    // scanned and every observer would be dropped (Scavenger's Talent, Korvold,
    // Mayhem Devil, ...; #5963). Scan them here, the single settlement
    // authority, exactly as the `ChooseManaColor` branch above already does for
    // an interrupted mana-color choice. A `Priority` resume stays EXCLUDED so
    // the pipeline remains the sole scan site and nothing double-fires (e.g.
    // Kilo's becomes-tapped proliferate under a standalone Relic activation).
    if !matches!(resume, WaitingFor::Priority { .. }) && events.len() > cost_event_start {
        let cost_events = events[cost_event_start..].to_vec();
        super::triggers::process_triggers(state, &cost_events);
        return Ok(
            super::triggers::preserve_order_triggers_resume(state, resume.clone())
                .unwrap_or(resume),
        );
    }

    Ok(resume)
}

/// CR 603.2 + CR 603.3b + CR 605.4a: The typed mana-cost root is the single
/// settlement authority for every event emitted before and after a replacement
/// pause. Deferred events were never scanned by their initiating action; current
/// events are scanned here even when the nominal resume is Priority.
fn settle_mana_ability_cost_events(
    state: &mut GameState,
    mut deferred: Vec<GameEvent>,
    events: &mut Vec<GameEvent>,
    current_start: usize,
) {
    let deferred_original_len = deferred.len();
    super::triggers::resolve_tap_mana_triggers_inline(state, &mut deferred, 0);
    events.extend(deferred[deferred_original_len..].iter().cloned());
    super::triggers::resolve_tap_mana_triggers_inline(state, events, current_start);
    deferred.extend_from_slice(&events[current_start..]);
    if !deferred.is_empty() {
        super::triggers::process_triggers(state, &deferred);
    }
    // The typed cursor has already collected the events emitted by this action.
    // Claim their exact occurrences through the normal post-action authority so
    // a Priority resume cannot scan the same events a second time.
    state.consumed_before_priority_trigger_events.extend(
        events
            .iter()
            .enumerate()
            .skip(current_start)
            .map(
                |(index, event)| crate::game::triggers::ConsumedTriggerEventOccurrence {
                    event: event.clone(),
                    occurrence: events[..index]
                        .iter()
                        .filter(|prior| *prior == event)
                        .count(),
                },
            ),
    );
}

/// CR 603.2 + CR 603.3b: A parent payment can immediately pause again after
/// its child completes. Transfer the child's unscanned batch into that next
/// typed root so it remains exactly-once owned rather than being dropped.
fn defer_cost_events_into_active_mana_root(
    state: &mut GameState,
    deferred: Vec<GameEvent>,
    current: &[GameEvent],
) {
    let Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) =
        state.pending_cost_move_resume.as_mut()
    else {
        return;
    };
    let local_start = cursor
        .current_action_deferred_start
        .min(cursor.deferred_cost_events.len());
    let local_events = cursor.deferred_cost_events.split_off(local_start);
    let inherited_current_len = current.len().saturating_sub(local_events.len());
    cursor.deferred_cost_events.extend(deferred);
    cursor
        .deferred_cost_events
        .extend_from_slice(&current[..inherited_current_len]);
    cursor.deferred_cost_events.extend(local_events);
    cursor.current_action_deferred_start = 0;
}

fn resume_mana_ability_root(
    state: &mut GameState,
    mana_source_controller: PlayerId,
    resume: ManaAbilityResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match resume {
        ManaAbilityResume::EffectPayCost {
            payer,
            return_to,
            ability,
            cost,
        } => match super::costs::pay_ability_cost_for_resolution(
            state,
            payer,
            cost.as_ref(),
            ability.as_ref(),
            events,
        )? {
            super::costs::PaymentOutcome::Paid => {
                super::effects::resolve_effect_pay_cost_rider(state, ability.as_ref(), events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    Ok(WaitingFor::Priority { player: return_to })
                } else {
                    Ok(state.waiting_for.clone())
                }
            }
            super::costs::PaymentOutcome::Paused { .. } => Ok(state.waiting_for.clone()),
            super::costs::PaymentOutcome::Failed { .. } => {
                state.cost_payment_failed_flag = true;
                super::effects::resolve_effect_pay_cost_rider(state, ability.as_ref(), events)
                    .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
                if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                    Ok(WaitingFor::Priority { player: return_to })
                } else {
                    Ok(state.waiting_for.clone())
                }
            }
        },
        ManaAbilityResume::PhyrexianCastPayment { caster, choices } => {
            super::casting_costs::finalize_mana_payment_with_phyrexian_choices(
                state, caster, &choices, events,
            )
        }
        ManaAbilityResume::FinalizePendingManaPayment { player } => {
            super::casting_costs::finalize_automatic_mana_payment(state, player, events)
        }
        ManaAbilityResume::CompanionToHand { player, cost } => {
            super::companion::resume_companion_to_hand_payment(state, player, cost, events)
        }
        resume => Ok(resume_waiting_for(mana_source_controller, resume)),
    }
}

fn continue_mana_ability_cost_payment(
    state: &mut GameState,
    pending: PendingManaAbility,
    cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let node = pending
        .rules_execution_node
        .unwrap_or_else(|| state.begin_activated_mana_journal_node(pending.source_id));
    state.with_rules_execution_node(node, |state| {
        continue_mana_ability_cost_payment_in_node(state, pending, cursor, events, cost_event_start)
    })
}

fn continue_mana_ability_cost_payment_in_node(
    state: &mut GameState,
    pending: PendingManaAbility,
    mut cursor: ManaAbilityCostCursor,
    events: &mut Vec<GameEvent>,
    cost_event_start: usize,
) -> Result<WaitingFor, EngineError> {
    let ability_def = mana_ability_definition(state, &pending)?;
    if cost_sacrifices_reserved_source(state, pending.source_id, &ability_def.cost) {
        return Err(EngineError::ActionNotAllowed(
            "This permanent is already committed to a spell sacrifice cost".to_string(),
        ));
    }
    while let Some(cost) = cursor.remaining.first().cloned() {
        match pay_mana_ability_cost_component(
            state,
            pending.clone(),
            &mut cursor,
            &cost,
            events,
            cost_event_start,
        )? {
            ManaAbilityPaymentProgress::Complete => {
                cursor.remaining.remove(0);
            }
            ManaAbilityPaymentProgress::Paused => return Ok(state.waiting_for.clone()),
        }
    }
    ensure_mana_ability_selection_cursor_consumed(&pending, &cursor)?;
    finish_mana_ability_cost_payment(state, pending, cursor, events, cost_event_start)
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: Resume the precise unpaid
/// suffix of a mana ability's activation cost after the interrupted cost move
/// was delivered or fully replaced. Mana production and its inline subchain
/// run only when the cursor has no remaining components.
pub(crate) fn resume_mana_ability_cost_move(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) =
        state.pending_cost_move_resume.take()
    else {
        unreachable!("mana ability cost-move resume requires its typed continuation")
    };
    continue_mana_ability_cost_payment(state, *pending, cursor, events, 0)
}

/// CR 605.3b + CR 605.1a: Run a mana ability's `sub_ability` chain inline.
/// Mana abilities don't use the stack, so non-mana clauses ("This land deals
/// 1 damage to you.") resolve atomically with the mana production. Walks the
/// full chain via `resolve_ability_chain` so nested effects (DealDamage on
/// controller, GainLife, etc.) route through the standard effect handlers.
fn resolve_mana_ability_sub_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    let Some(sub) = ability.sub_ability.as_deref() else {
        return;
    };
    // Errors during the sub-chain are non-fatal — mana has already been
    // added to the pool and the cost has been paid. The damage/life clause
    // of a painland cannot legitimately fail in a well-formed game state.
    let _ = super::effects::resolve_ability_chain(state, sub, events, 0);
}

fn contains_duplicate_object_id(ids: &[ObjectId]) -> bool {
    ids.iter()
        .enumerate()
        .any(|(index, id)| ids[index + 1..].contains(id))
}

#[allow(clippy::too_many_arguments)]
fn pay_mana_ability_cost_with_choices<I, J, L>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
    chosen_discards: &mut J,
    chosen_sacrificed_battlefield: &mut L,
    chosen_hybrid_payment: Option<&[ManaType]>,
    chosen_counter_count: Option<u32>,
    chosen_x: Option<u32>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<(), EngineError>
where
    I: Iterator<Item = ObjectId>,
    J: Iterator<Item = ObjectId>,
    L: Iterator<Item = ObjectId>,
{
    if cost_sacrifices_reserved_source(state, source_id, cost) {
        return Err(EngineError::ActionNotAllowed(
            "This permanent is already committed to a spell sacrifice cost".to_string(),
        ));
    }

    match cost {
        Some(AbilityCost::Tap) => tap_source(state, source_id, events)?,
        // CR 605.3a + CR 601.2h: Top-level mana sub-cost (e.g. hypothetical
        // `{R}: Add {G}{G}`). Composite costs route through the Composite arm.
        Some(AbilityCost::Mana { cost }) => {
            pay_mana_sub_cost(
                state,
                source_id,
                player,
                cost,
                chosen_hybrid_payment,
                events,
                excluded_sources,
                sub_cost_demand,
                parent,
            )?;
        }
        // CR 605.1a + CR 701.17a: Bare `Mill` mana-ability cost. The Millikin
        // `{T}, Mill a card: Add {C}` shape routes through the Composite arm; this
        // arm covers a hypothetical mill-only mana ability for completeness.
        Some(AbilityCost::Mill { count }) => mill_for_mana_cost(state, player, *count, events)?,
        Some(AbilityCost::PayLife { amount }) => {
            // CR 119.4 + CR 903.4: QuantityExpr resolves against the activator's
            // current state (e.g. commander color identity count).
            let resolved =
                super::quantity::resolve_quantity(state, amount, player, source_id).max(0) as u32;
            pay_life_cost(state, player, resolved, events)?
        }
        Some(AbilityCost::TapCreatures {
            requirement,
            filter,
        }) => {
            // CR 605.1a: Mana-ability tap costs (Convoke-style) are fixed-count
            // only; the aggregate "total power N" form is reserved for
            // Crew/Saddle/Teamwork, which are never mana abilities.
            let count = requirement.fixed_count().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Aggregate-power tap cost is not valid for a mana ability".to_string(),
                )
            })?;
            for _ in 0..count {
                let chosen_id = chosen_tappers.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing tapped creature selection for mana ability".to_string(),
                    )
                })?;
                tap_selected_creature_for_mana_cost(
                    state,
                    source_id,
                    player,
                    chosen_id,
                    filter,
                    cost_has_source_tap_component(cost),
                    events,
                )?;
            }
        }
        Some(AbilityCost::Discard {
            count,
            filter,
            selection,
            self_scope,
        }) => {
            if selection.is_random() {
                return Err(EngineError::InvalidAction(
                    "Unsupported random discard cost for mana ability".to_string(),
                ));
            }
            if self_scope.is_source_card() {
                match crate::game::effects::discard::discard_as_cost(
                    state, source_id, player, events,
                ) {
                    crate::game::effects::discard::DiscardOutcome::Complete => {}
                    crate::game::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => {}
                }
            } else {
                let resolved = super::quantity::resolve_quantity(state, count, player, source_id)
                    .max(0) as usize;
                for _ in 0..resolved {
                    let chosen_id = chosen_discards.next().ok_or_else(|| {
                        EngineError::InvalidAction(
                            "Missing discarded card selection for mana ability".to_string(),
                        )
                    })?;
                    discard_selected_card_for_mana_cost(
                        state,
                        source_id,
                        player,
                        chosen_id,
                        filter.as_ref(),
                        events,
                    )?;
                }
            }
        }
        // CR 118.3 + CR 605.3b: Self-sacrifice mana ability costs are paid
        // atomically before mana production. This is the Treasure / Eldrazi
        // Spawn / Lotus Petal shape.
        Some(AbilityCost::Sacrifice(cost))
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            if deferred_spell_sacrifice_reserved(state, source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "This permanent is already committed to a spell sacrifice cost".to_string(),
                ));
            }
            if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot sacrifice this permanent as a cost".to_string(),
                ));
            }
            if matches!(
                sacrifice::sacrifice_permanent(state, source_id, player, events)?,
                sacrifice::SacrificeOutcome::NeedsReplacementChoice(_)
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability sacrifice replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // CR 117.1 + CR 118.3 + CR 605.3b: Non-self sacrifice-from-battlefield
        // as a mana ability cost (Phyrexian Altar class). The interactive flow
        // has already captured the chosen permanents; verify each is still
        // legal and route through the sacrifice replacement pipeline.
        Some(AbilityCost::Sacrifice(cost))
            if !matches!(cost.target, TargetFilter::SelfRef)
                && matches!(
                    cost.requirement,
                    crate::types::ability::SacrificeRequirement::Count { .. }
                ) =>
        {
            let crate::types::ability::SacrificeRequirement::Count { count } = cost.requirement
            else {
                unreachable!("guarded above");
            };
            let target = &cost.target;
            for _ in 0..count {
                let chosen_id = chosen_sacrificed_battlefield.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing sacrificed permanent selection for mana ability".to_string(),
                    )
                })?;
                if matches!(
                    sacrifice_selected_permanent_for_mana_cost(
                        state, source_id, player, chosen_id, target, events,
                    )?,
                    sacrifice::SacrificeOutcome::NeedsReplacementChoice(_)
                ) {
                    return Err(EngineError::InvalidAction(
                        "Mana ability sacrifice replacement pause must be owned by the activation cursor"
                            .to_string(),
                    ));
                }
            }
        }
        Some(AbilityCost::Exile { .. }) => {
            return Err(EngineError::InvalidAction(
                "Mana ability exile costs must be paid by the activation cursor".to_string(),
            ));
        }
        // CR 605.2 + CR 701.59: Bare collect-evidence mana-ability cost. The
        // exile already happened interactively via the `CollectEvidenceChoice`
        // resume; no-op so the cost is neither re-paid nor errored.
        Some(AbilityCost::CollectEvidence { .. }) => {}
        // CR 107.3a/.3c + CR 702.179f: Bare `Pay X speed` mana-ability cost
        // (Chicago Loop). Concretize the announced X into a Fixed cost, then
        // delegate to the single-authority cost payer.
        Some(AbilityCost::PaySpeed { amount }) => {
            let cost = match chosen_x {
                Some(x) => AbilityCost::PaySpeed {
                    amount: QuantityExpr::Fixed { value: x as i32 },
                },
                None => AbilityCost::PaySpeed {
                    amount: amount.clone(),
                },
            };
            if matches!(
                super::costs::pay_ability_cost_for_activation(
                    state, player, source_id, &cost, None, events,
                )?,
                super::costs::PaymentOutcome::Paused { .. }
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // Self-contained components (Untap, Exert, PayEnergy, self-ReturnToHand,
        // EffectCost) delegate to the single-authority cost payer.
        Some(c) if is_self_contained_mana_subcost(c) => {
            if matches!(
                super::costs::pay_ability_cost_for_activation(
                    state, player, source_id, c, None, events,
                )?,
                super::costs::PaymentOutcome::Paused { .. }
            ) {
                return Err(EngineError::InvalidAction(
                    "Mana ability replacement pause must be owned by the activation cursor"
                        .to_string(),
                ));
            }
        }
        // CR 122.1 + CR 601.2b: Standalone RemoveCounter-on-self mana-ability
        // cost (Pentad Prism, Crystalline Crawler, Druids' Repository class).
        Some(AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: None,
            ..
        }) => {
            let count = match *count {
                REMOVE_COUNTER_COST_ANY_NUMBER => chosen_counter_count.ok_or_else(|| {
                    EngineError::InvalidAction("Missing counter count for mana ability".to_string())
                })?,
                REMOVE_COUNTER_COST_ALL => {
                    removable_counter_count_for_mana_cost(state, source_id, counter_type)
                }
                count => count,
            };
            remove_counters_for_mana_cost(state, source_id, counter_type, count, events);
        }
        Some(other) => {
            return Err(EngineError::InvalidAction(format!(
                "Unsupported mana ability cost: {other:?}"
            )));
        }
        None => {}
    }

    Ok(())
}

fn cost_sacrifices_reserved_source(
    state: &GameState,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> bool {
    deferred_spell_sacrifice_reserved(state, source_id)
        && cost.as_ref().is_some_and(ability_cost_sacrifices_source)
}

fn ability_cost_sacrifices_source(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(cost) => {
            matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1)
        }
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_sacrifices_source),
        _ => false,
    }
}

/// CR 605.1a + CR 701.17a-b: Pay a `Mill` cost component of a mana ability by
/// milling `count` cards from the activating player's library into their
/// graveyard. Routes through the replacement pipeline (mirroring `mill::resolve`
/// and the rad-counter handler) so graveyard-redirect replacements (Rest in
/// Peace / Leyline of the Void) apply and "a card was put into a graveyard"
/// triggers see the milled cards. Millikin (`{T}, Mill a card: Add {C}`) is the
/// canonical case — mill is a non-mana cost component and the {C} is produced
/// unconditionally.
fn mill_for_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let proposed = crate::types::proposed_event::ProposedEvent::Mill {
        player_id: player,
        count,
        destination: Zone::Graveyard,
        applied: Default::default(),
    };
    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            // CR 616.1: a per-card `Moved` ordering choice parks the prompt
            // (`state.waiting_for` left set, tail in the active BatchDelivery frame);
            // bail like `mill::resolve` does so the surfaced prompt is not
            // clobbered. The parked activation resumes the remaining cost
            // components and mana production.
            if !super::effects::mill::apply_mill_after_replacement(state, event, events).map_err(
                |e| EngineError::InvalidAction(format!("Mill cost could not be paid: {e:?}")),
            )? {
                return Ok(());
            }
        }
        // CR 701.17b: "mill as many as you can" — a fully replaced-away or empty
        // library still pays the cost (milling zero cards is legal).
        super::replacement::ReplacementResult::Prevented => {}
        super::replacement::ReplacementResult::NeedsChoice(choosing_player) => {
            state.waiting_for =
                super::replacement::replacement_choice_waiting_for(choosing_player, state);
        }
    }
    Ok(())
}

/// Single-authority delegation gate: self-contained mana-ability cost components
/// (non-interactive, non-mana, non-tap, no `chosen_*` selection) that the
/// activated-ability cost payer (`super::costs::pay_ability_cost`) already
/// resolves correctly. Routing these through that one authority — rather than
/// duplicating each CR-annotated body here — keeps replacement routing and rule
/// annotations in a single place (CLAUDE.md: "single authority for ability
/// costs"). Covers Untap ({Q}, Pili-Pala), Exert (Arena of Glory / Oasis
/// Ritualist), PayEnergy (Aether Hub class), self-`ReturnToHand` (Grinning
/// Ignus), and `EffectCost` put-counter-on-self (Wall of Roots).
///
/// `PaySpeed` is deliberately excluded: Chicago Loop's `Pay X speed: Add X mana`
/// couples a player-announced X to both cost and effect (CR 601.2b), which the
/// non-announcing delegation path cannot express — it is handled on its own.
///
/// `CollectEvidence` is also deliberately excluded: Cryptex's `Collect evidence 3`
/// is paid interactively via the `CollectEvidenceChoice` prompt that
/// `advance_mana_ability_activation` surfaces before mana production (CR 701.59),
/// so it is a no-op in the cost-payment match rather than a delegated payment.
fn is_self_contained_mana_subcost(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Untap
        | AbilityCost::Exert
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::ReturnToHand {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => true,
        // CR 122.1 + CR 601.2b: Pentad Prism / Everflowing Chalice — bare
        // self-RemoveCounter mana-ability costs (no tap) delegate to the
        // activated-ability cost payer. "Remove any number" stays on the
        // interactive mana-ability path in `advance_mana_ability_activation`.
        AbilityCost::RemoveCounter {
            target: None,
            count,
            ..
        } => !crate::types::ability::is_chosen_remove_counter_cost_count(*count),
        _ => false,
    }
}

fn pay_life_cost(
    state: &mut GameState,
    player: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // CR 118.3 + CR 119.4 + CR 119.8: Delegate to the single-authority helper
    // so mana-ability life costs honor the replacement pipeline and the
    // CantLoseLife lock identically to every other pay-life path.
    match life_costs::pay_life_as_cast_or_activation_cost(state, player, amount, events) {
        PayLifeCostResult::Paid { .. } => Ok(()),
        PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => Err(
            EngineError::ActionNotAllowed("Cannot pay life cost for mana ability".to_string()),
        ),
    }
}

/// CR 605.3a + CR 605.1a: Extract the nested `ManaCost` from an ability cost
/// that contains a mana sub-cost (either at top level or inside a Composite).
/// Returns `None` for costs with no mana payment component.
pub(crate) fn mana_sub_cost_of(cost: &Option<AbilityCost>) -> Option<&ManaCost> {
    match cost {
        Some(AbilityCost::Mana { cost }) => Some(cost),
        Some(AbilityCost::Composite { costs }) => costs.iter().find_map(|c| match c {
            AbilityCost::Mana { cost } => Some(cost),
            _ => None,
        }),
        _ => None,
    }
}

/// CR 605.3a + CR 605.3b: True iff this cost resolves with NO player prompt once
/// the produced color is pre-chosen — i.e. it hits none of the five interactive
/// cost gates that `advance_mana_ability_activation` checks before producing
/// mana (discard, tap-creatures, non-self exile, non-self sacrifice, and the
/// mana sub-cost handled by `find_*`/`mana_sub_cost_of` directly above/below).
/// This is the eligibility gate for bulk activation: only such sources can be
/// batched behind a single shared color decision (CR 605.3b — no stack, resolves
/// immediately).
///
/// Deny-by-default whitelist — only `Tap`, self-sacrifice (`SelfRef`, the
/// Treasure/Gold cost shape), and `Composite`s built solely from those qualify.
/// Every other cost variant — including any added later — is treated as
/// choice-bearing and excluded, so a new interactive cost can never silently
/// become batchable. Kept beside the gate matchers so the whitelist stays in
/// lockstep if a sixth gate is introduced.
fn cost_resolves_without_choice(cost: &Option<AbilityCost>) -> bool {
    cost.as_ref().is_none_or(cost_component_choice_free)
}

fn cost_component_choice_free(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Sacrifice(cost)
            if matches!(cost.target, TargetFilter::SelfRef)
                && cost.requirement == crate::types::ability::SacrificeRequirement::count(1) =>
        {
            true
        }
        AbilityCost::Composite { costs } => costs.iter().all(cost_component_choice_free),
        _ => false,
    }
}

/// CR 605.3a: The controller's *other* permanents that could be activated for
/// the same `SingleColor` mana choice — identical ability definition, choice-
/// free cost, and currently activatable (untapped, on the battlefield, not
/// summoning-sick, via the shared `mana_ability_ready_without_simulation` gate).
/// These are the sources `GameAction::ChooseManaColor` may bulk-activate with the
/// chosen color. `exclude` is the just-activated source (already cost-paid, so
/// omitted). Sorted by id for deterministic ordering across the WASM/multiplayer
/// boundary.
fn batch_eligible_siblings(
    state: &GameState,
    player: PlayerId,
    exclude: ObjectId,
    ability_def: &AbilityDefinition,
) -> Vec<ObjectId> {
    // A permanent may carry the same ability definition more than once (granted by
    // multiple Auras/effects). Checking only the first matching index would wrongly
    // reject the source when that copy is unavailable (e.g. a once-each-turn
    // restriction) while a later identical copy is ready, so test whether *any*
    // matching ability index is ready.
    let mut siblings: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter_map(|id| {
            let obj = state.objects.get(&id)?;
            (id != exclude
                && obj.controller == player
                && obj.abilities.iter().enumerate().any(|(index, ability)| {
                    ability == ability_def
                        && mana_ability_ready_without_simulation(
                            state,
                            player,
                            id,
                            index,
                            ability_def,
                        )
                }))
            .then_some(id)
        })
        .collect();
    siblings.sort_unstable_by_key(|id| id.0);
    siblings
}

/// CR 107.4e + CR 601.2h: Enumerate legal per-hybrid-shard color assignments
/// for a mana-ability mana sub-cost. Each returned vector aligns 1:1 with
/// hybrid shards in `cost` in printed order. A plan is included iff a clone
/// of `pool` can be fully debited when each hybrid shard is pinned to the
/// chosen color.
///
/// For a cost with zero hybrid shards the result is `[vec![]]` when the pool
/// covers the cost (representing the trivial empty-choice plan), or empty
/// when the pool cannot cover. Callers short-circuit the single-plan case
/// into auto-pay.
fn enumerate_hybrid_payment_plans(
    pool: &ManaPool,
    cost: &ManaCost,
    ctx: &PaymentContext<'_>,
) -> Vec<Vec<ManaType>> {
    let hybrid_pairs = hybrid_shard_pairs(cost);
    let mut plans = Vec::new();
    enumerate_plans_rec(pool, cost, ctx, &hybrid_pairs, &mut Vec::new(), &mut plans);
    plans
}

/// List the (a, b) color pairs for each hybrid shard in printed order.
/// Only pure hybrid shards (`{W/U}` style) contribute — Phyrexian hybrid
/// shards resolve via the mana-payment life-fallback path and
/// colorless-hybrid (`{C/W}`) defers to the auto-pay preference, which
/// matches how casting treats them.
fn hybrid_shard_pairs(cost: &ManaCost) -> Vec<(ManaType, ManaType)> {
    let ManaCost::Cost { shards, .. } = cost else {
        return Vec::new();
    };
    shards
        .iter()
        .filter_map(|&shard| match mana_payment::shard_to_mana_type(shard) {
            mana_payment::ShardRequirement::Hybrid(a, b) => Some((a, b)),
            _ => None,
        })
        .collect()
}

fn enumerate_plans_rec(
    pool: &ManaPool,
    cost: &ManaCost,
    ctx: &PaymentContext<'_>,
    hybrid_pairs: &[(ManaType, ManaType)],
    chosen: &mut Vec<ManaType>,
    out: &mut Vec<Vec<ManaType>>,
) {
    if chosen.len() == hybrid_pairs.len() {
        if try_pay_with_hybrid_plan(pool, cost, chosen, ctx).is_some() {
            out.push(chosen.clone());
        }
        return;
    }
    let (a, b) = hybrid_pairs[chosen.len()];
    chosen.push(a);
    enumerate_plans_rec(pool, cost, ctx, hybrid_pairs, chosen, out);
    chosen.pop();
    if a != b {
        chosen.push(b);
        enumerate_plans_rec(pool, cost, ctx, hybrid_pairs, chosen, out);
        chosen.pop();
    }
}

/// CR 107.4e: Simulate paying `cost` from a clone of `pool` with hybrid
/// shards pinned to the colors in `plan`. Returns `Some(())` when the pool
/// covers the cost, `None` otherwise. Deterministic — uses the same
/// auto-pay rules as `pay_cost` except hybrid shards defer to `plan`.
fn try_pay_with_hybrid_plan(
    pool: &ManaPool,
    cost: &ManaCost,
    plan: &[ManaType],
    ctx: &PaymentContext<'_>,
) -> Option<()> {
    // CR 106.6: Plan publication and auto-selection must use the same
    // activation context as the authoritative real debit. Otherwise the
    // engine can offer a restricted mana unit that execution then rejects.
    // The simulated spent units are discarded; provenance is recorded only
    // at the real payment site.
    select_cost_with_plan(pool, cost, plan, Some(ctx))
        .ok()
        .map(|_| ())
}

/// CR 107.4e + CR 601.2h: Select the exact units that pay `cost` using `plan` for hybrid
/// shards. Non-hybrid shards (single, Phyrexian, snow, colorless-hybrid,
/// hybrid-Phyrexian, two-generic-hybrid, X) are routed through the same
/// auto-pay rules the casting flow uses via `mana_payment::pay_from_pool`, but
/// with the hybrid shards already resolved, the plan is unambiguous.
///
/// Implementation: build a scratch cost with hybrid shards rewritten to
/// single-color shards per `plan`, then delegate to the shared selector. This keeps
/// every shard-kind's payment rules in one place.
fn select_cost_with_plan(
    pool: &ManaPool,
    cost: &ManaCost,
    plan: &[ManaType],
    ctx: Option<&PaymentContext<'_>>,
) -> Result<Vec<crate::types::mana::ManaUnit>, mana_payment::PaymentError> {
    use crate::types::mana::ManaCostShard;
    let ManaCost::Cost { shards, generic } = cost else {
        return Ok(Vec::new());
    };
    let mut plan_cursor = 0usize;
    let rewritten_shards: Vec<ManaCostShard> = shards
        .iter()
        .map(|&shard| match mana_payment::shard_to_mana_type(shard) {
            mana_payment::ShardRequirement::Hybrid(..) => {
                let color = plan[plan_cursor];
                plan_cursor += 1;
                mana_type_to_single_shard(color)
            }
            _ => shard,
        })
        .collect();
    let scratch_cost = ManaCost::Cost {
        shards: rewritten_shards,
        generic: *generic,
    };
    // CR 106.6: Route through the restriction-aware payment path so the
    // player's context (activation or spell) gates eligible mana units.
    // CR 107.4f: Mana-ability sub-cost payment doesn't surface a player-side
    // ShardChoice and is paid implicitly during ability resolution; pass an
    // empty `LifePaymentColors` since K'rrik substitution does not apply to
    // mana abilities' own activation costs in any printed exemplar today.
    mana_payment::select_mana_payment(
        pool,
        &scratch_cost,
        None,
        ctx,
        false,
        None,
        crate::types::mana::LifePaymentColors::EMPTY,
        // CR 118.3a: mana-ability activation sub-costs are not pinnable.
        &[],
    )
    .map(|(spent, _life)| spent)
}

/// Map a `ManaType` to the printed-shard variant that requires exactly that
/// color (used to pin hybrid shards after the player's color choice).
fn mana_type_to_single_shard(color: ManaType) -> crate::types::mana::ManaCostShard {
    use crate::types::mana::ManaCostShard;
    match color {
        ManaType::White => ManaCostShard::White,
        ManaType::Blue => ManaCostShard::Blue,
        ManaType::Black => ManaCostShard::Black,
        ManaType::Red => ManaCostShard::Red,
        ManaType::Green => ManaCostShard::Green,
        ManaType::Colorless => ManaCostShard::Colorless,
    }
}

/// CR 605.3a + CR 602.2b + CR 601.2g-h: Pay a mana sub-cost for an activated
/// mana ability. If `hybrid_plan` is provided, hybrid shards are pinned to the
/// colors chosen by `PayManaAbilityMana` and debited from the current pool.
/// Otherwise, use the shared activation mana-payment building block so the
/// player may activate other mana abilities while paying this activation cost.
#[allow(clippy::too_many_arguments)]
fn pay_mana_sub_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &ManaCost,
    hybrid_plan: Option<&[ManaType]>,
    events: &mut Vec<GameEvent>,
    excluded_sources: &HashSet<ObjectId>,
    sub_cost_demand: Option<&mana_payment::ColorDemand>,
    parent: Option<&ManaAbilityCostParent>,
) -> Result<(), EngineError> {
    let Some(hybrid_plan) = hybrid_plan else {
        // CR 605.3c: Every source already in `excluded_sources` is an ancestor
        // mana-ability activation that is synchronously suspended on the call
        // stack mid-payment (its cost is still being paid; it has not yet
        // resolved). CR 605.3c ("Once a player begins to activate a mana
        // ability, that ability can't be activated again until it has
        // resolved") applies to each ancestor link individually, so the entire
        // in-flight chain must be excluded from auto-tap — not just `source_id`.
        // Extending the chain here (rather than rebuilding it from
        // `{source_id}`) is what makes a 2-source cross-payment, an N-source
        // chain, or a self-loop terminate instead of recursing infinitely.
        let mut excluded_sources = excluded_sources.clone();
        excluded_sources.insert(source_id);
        // CR 605.1a: A mana ability never carries a power-up tag (power-up
        // abilities can't produce mana), so the tag-scoped activation context is
        // `None` here — Quinjet's {R}{R} must not pay another mana ability's cost.
        return super::casting::pay_ability_mana_cost_excluding_with_parent(
            state,
            player,
            source_id,
            cost,
            None,
            events,
            &excluded_sources,
            sub_cost_demand,
            parent,
        );
    };

    // CR 106.6: The mana sub-cost of a mana ability is paid as part of an
    // ability activation — spend-restrictions must be evaluated through
    // `allows_activation` (via `PaymentContext::Activation`), not through the
    // pool's restriction-blind `pay_cost`. Without this, activation-only
    // mana (e.g. Heart of Ramos) would silently pay through for the {R} half
    // of a hypothetical "{R}: Add {G}{G}" mana ability.
    let (source_types, source_subtypes) = super::casting::activation_source_types(state, source_id);
    // CR 605.1a: Mana abilities never carry a power-up tag, so `ability_tag` is
    // `None` for the mana-ability sub-cost activation context.
    let ctx = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
        ability_tag: None,
    };
    state.restamp_pool_pip_ids(player);
    let spent = select_cost_with_plan(
        &state.players[player.0 as usize].mana_pool,
        cost,
        hybrid_plan,
        Some(&ctx),
    )
    .map_err(|_| {
        EngineError::ActionNotAllowed("Mana pool cannot cover mana ability cost".to_string())
    })?;
    let recipient = state.mana_payment_recipient(source_id, player);
    state
        .resolve_and_apply_mana_spend(player, recipient, &spent)
        .map_err(|_| {
            EngineError::ActionNotAllowed("Mana pool changed before payment applied".to_string())
        })?;
    state.layers_dirty.mark_full();
    // CR 605.3b: The player's mana pool mutation is the public signal; no
    // dedicated event exists for ability mana payments. The pool-diff is
    // surfaced via the standard state-update machinery.
    let _ = events;
    Ok(())
}

/// CR 605.3b: Complete a `PayManaAbilityMana` prompt by validating the
/// submitted payment against the enumerated options and resuming activation.
pub fn handle_pay_mana_ability_mana(
    state: &mut GameState,
    options: &[Vec<ManaType>],
    pending: &PendingManaAbility,
    payment: &[ManaType],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !options.iter().any(|opt| opt.as_slice() == payment) {
        return Err(EngineError::InvalidAction(
            "Chosen mana payment is not among the legal options".to_string(),
        ));
    }
    let mut updated = pending.clone();
    updated.chosen_mana_payment = Some(payment.to_vec());
    advance_mana_ability_activation(state, updated, events)
}

fn any_number_self_remove_counter_cost(cost: &Option<AbilityCost>) -> Option<&CounterMatch> {
    match cost.as_ref()? {
        AbilityCost::RemoveCounter {
            count: REMOVE_COUNTER_COST_ANY_NUMBER,
            counter_type,
            target: None,
            ..
        } => Some(counter_type),
        AbilityCost::Composite { costs } => costs.iter().find_map(|cost| match cost {
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_ANY_NUMBER,
                counter_type,
                target: None,
                ..
            } => Some(counter_type),
            _ => None,
        }),
        _ => None,
    }
}

fn removable_counter_count_for_mana_cost(
    state: &GameState,
    source_id: ObjectId,
    counter_type: &CounterMatch,
) -> u32 {
    let Some(obj) = state.objects.get(&source_id) else {
        return 0;
    };
    match counter_type {
        CounterMatch::Any => obj.counters.values().copied().sum(),
        CounterMatch::OfType(counter_type) => obj.counters.get(counter_type).copied().unwrap_or(0),
    }
}

fn remove_counters_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    counter_type: &CounterMatch,
    mut count: u32,
    events: &mut Vec<GameEvent>,
) {
    match counter_type {
        CounterMatch::OfType(counter_type) => {
            super::effects::counters::remove_counter_with_replacement(
                state,
                source_id,
                counter_type.clone(),
                count,
                events,
            );
        }
        CounterMatch::Any => {
            let counters: Vec<(CounterType, u32)> = state
                .objects
                .get(&source_id)
                .map(|obj| {
                    obj.counters
                        .iter()
                        .map(|(counter_type, count)| (counter_type.clone(), *count))
                        .collect()
                })
                .unwrap_or_default();
            for (counter_type, available) in counters {
                if count == 0 {
                    break;
                }
                let to_remove = available.min(count);
                if to_remove > 0 {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        counter_type,
                        to_remove,
                        events,
                    );
                    count -= to_remove;
                }
            }
        }
    }
}

/// Tap a permanent as part of paying a mana ability cost.
fn tap_source(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Cannot activate tap ability: permanent is tapped".to_string(),
        ));
    }
    // CR 701.26a + CR 508.1f: route the {T} mana-ability tap through the single
    // authority so a "can't become tapped" source is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, source_id, events)?;
    Ok(())
}

fn tap_creature_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (requirement, filter) = super::casting::find_tap_creatures_cost(cost.as_ref()?)?;
    // CR 605.1a: mana-ability tap costs (Convoke-style) are fixed-count only.
    let count = requirement.fixed_count()?;
    let creatures = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if cost_has_source_tap_component(cost) && id == source_id {
                return false;
            }
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
                return false;
            }
            matches_target_filter(
                state,
                id,
                filter,
                &FilterContext::from_source(state, source_id),
            )
        })
        .collect();
    Some((count as usize, creatures))
}

fn discard_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = find_non_self_discard_cost(cost.as_ref()?)?;
    let resolved = super::quantity::resolve_quantity(state, count, player, source_id).max(0);
    let cards = super::casting::find_eligible_discard_targets(state, player, source_id, filter);
    Some((resolved as usize, cards))
}

/// CR 117.1 + CR 118.3: Match non-self `AbilityCost::Exile` shapes. Returns
/// `(count, effective_zone, filter)` if found, else `None`.
fn find_exile_cost(cost: &AbilityCost) -> Option<(u32, Zone, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if !matches!(filter, Some(TargetFilter::SelfRef)) => Some((
            *count,
            exile_cost_effective_zone(*zone, filter.as_ref()),
            filter.as_ref(),
        )),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_exile_cost),
        _ => None,
    }
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible objects for a non-self
/// exile mana ability cost. Library costs are deterministic top-card payment,
/// not a player choice, so they are prepared separately.
fn exile_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Zone, Vec<ObjectId>)> {
    let (count, zone, filter) = find_exile_cost(cost.as_ref()?)?;
    if zone == Zone::Library {
        return None;
    }
    let cards = eligible_exile_cost_objects(state, player, source_id, zone, filter, count)
        .into_iter()
        .filter(|id| !deferred_spell_sacrifice_reserved(state, *id))
        .collect();
    Some((count as usize, zone, cards))
}

fn prepare_deterministic_exile_cost_selection(
    state: &GameState,
    pending: &PendingManaAbility,
    cost: &Option<AbilityCost>,
) -> Result<Option<PendingManaAbility>, EngineError> {
    let Some((count, Zone::Library, filter)) = cost.as_ref().and_then(find_exile_cost) else {
        return Ok(None);
    };
    if count == 0 {
        return Ok(None);
    }
    if filter.is_some() {
        return Err(EngineError::InvalidAction(
            "Unsupported filtered library exile cost for mana ability".to_string(),
        ));
    }
    let chosen = eligible_exile_cost_objects(
        state,
        pending.player,
        pending.source_id,
        Zone::Library,
        None,
        count,
    );
    if chosen.len() < count as usize {
        return Err(EngineError::ActionNotAllowed(
            "Not enough cards in library to exile for mana ability cost".to_string(),
        ));
    }
    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });
    let mut updated = pending.clone();
    updated.chosen_exiled = chosen;
    updated.cost_paid_object = captured;
    Ok(Some(updated))
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible battlefield permanents
/// for an `AbilityCost::Sacrifice(SacrificeCost::count(!SelfRef, 1))` mana ability cost.
/// Delegates eligibility to the casting cost helper so mana and non-mana
/// activation costs share the same battlefield/controller/filter semantics.
fn sacrifice_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = super::casting::find_non_self_sacrifice_cost(cost.as_ref()?)?;
    let permanents =
        super::casting::find_eligible_sacrifice_targets(state, player, source_id, filter);
    Some((count as usize, permanents))
}

fn find_non_self_discard_cost(
    cost: &AbilityCost,
) -> Option<(&crate::types::ability::QuantityExpr, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Discard {
            count,
            filter,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            selection: crate::types::ability::CardSelectionMode::Chosen,
        } => Some((count, filter.as_ref())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_discard_cost),
        _ => None,
    }
}

fn tap_selected_creature_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: &TargetFilter,
    exclude_source: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if exclude_source && chosen_id == source_id {
        return Err(EngineError::ActionNotAllowed(
            "Source cannot satisfy both tap costs".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&chosen_id)
        .ok_or_else(|| EngineError::InvalidAction("Selected creature not found".to_string()))?;
    if obj.zone != Zone::Battlefield || obj.controller != player || obj.tapped {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature is not an untapped creature you control".to_string(),
        ));
    }
    if !matches_target_filter(
        state,
        chosen_id,
        filter,
        &FilterContext::from_source(state, source_id),
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Selected creature does not satisfy mana ability cost".to_string(),
        ));
    }

    // CR 701.26a + CR 508.1f: route the tap-another-creature mana cost through the
    // single authority so a "can't become tapped" creature is refused.
    crate::game::restrictions::tap_permanent_for_cost(state, chosen_id, events)?;
    Ok(())
}

fn discard_selected_card_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: Option<&TargetFilter>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let player_state = state
        .players
        .get(player.0 as usize)
        .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
    if !player_state.hand.contains(&chosen_id) || chosen_id == source_id {
        return Err(EngineError::ActionNotAllowed(
            "Selected card is not eligible to discard for mana ability".to_string(),
        ));
    }
    if let Some(target_filter) = filter {
        if !matches_target_filter(
            state,
            chosen_id,
            target_filter,
            &FilterContext::from_source(state, source_id),
        ) {
            return Err(EngineError::ActionNotAllowed(
                "Selected card does not satisfy mana ability discard cost".to_string(),
            ));
        }
    }
    match crate::game::effects::discard::discard_as_cost(state, chosen_id, player, events) {
        crate::game::effects::discard::DiscardOutcome::Complete => Ok(()),
        crate::game::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => Ok(()),
    }
}

fn sacrifice_selected_permanent_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    chosen_id: ObjectId,
    filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<sacrifice::SacrificeOutcome, EngineError> {
    let obj = state.objects.get(&chosen_id).ok_or_else(|| {
        EngineError::InvalidAction("Selected permanent for sacrifice cost not found".to_string())
    })?;
    if obj.zone != Zone::Battlefield || obj.controller != player {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent is not on the battlefield under your control".to_string(),
        ));
    }
    if !matches_target_filter(
        state,
        chosen_id,
        filter,
        &FilterContext::from_source(state, source_id),
    ) {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent does not match the sacrifice cost filter".to_string(),
        ));
    }
    if super::static_abilities::player_cant_sacrifice_as_cost(state, player, chosen_id) {
        return Err(EngineError::ActionNotAllowed(
            "Selected permanent cannot be sacrificed as a cost".to_string(),
        ));
    }
    sacrifice::sacrifice_permanent(state, chosen_id, player, events)
}

fn cost_has_source_tap_component(cost: &Option<AbilityCost>) -> bool {
    match cost {
        Some(AbilityCost::Tap) => true,
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|cost| matches!(cost, AbilityCost::Tap))
        }
        _ => false,
    }
}

pub(crate) fn resume_waiting_for(
    mana_source_controller: PlayerId,
    resume: ManaAbilityResume,
) -> WaitingFor {
    match resume {
        ManaAbilityResume::Priority => WaitingFor::Priority {
            player: mana_source_controller,
        },
        ManaAbilityResume::ManaPayment {
            outer_player,
            convoke_mode,
        } => WaitingFor::ManaPayment {
            player: outer_player.unwrap_or(mana_source_controller),
            convoke_mode,
        },
        ManaAbilityResume::UnlessPayment {
            outer_player,
            cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        } => WaitingFor::UnlessPayment {
            player: outer_player.unwrap_or(mana_source_controller),
            cost: *cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        },
        ManaAbilityResume::EffectPayCost { .. }
        | ManaAbilityResume::PhyrexianCastPayment { .. }
        | ManaAbilityResume::FinalizePendingManaPayment { .. }
        | ManaAbilityResume::CompanionToHand { .. } => {
            unreachable!("effect-cost resume is handled by resume_mana_ability_root")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityKind, AbilityTag, ActivationRestriction, Comparator,
        ContinuousModification, ControllerRef, CopyRetargetPermission, DelayedTriggerCondition,
        DevotionColors, Duration, Effect, FilterProp, LinkedExileScope, ManaContribution,
        ManaProduction, MultiTargetSpec, ObjectScope, PlayerScope, QuantityExpr, QuantityRef,
        SacrificeCost, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
        REMOVE_COUNTER_COST_ANY_NUMBER,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{
        ManaColor, ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit,
    };
    use crate::types::statics::{CostPaymentProhibition, ProhibitionScope, StaticMode};
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    fn gemstone_caverns_mana_ability() -> AbilityDefinition {
        let replacement = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .condition(AbilityCondition::ConditionInstead {
            inner: Box::new(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Generic("luck".to_string())),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            }),
        });

        let mut ability = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 1 },
        });
        ability.sub_ability = Some(Box::new(replacement));
        ability
    }

    use crate::game::test_fixtures::brushland_colored_ability;

    fn seed_pool_with(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        for _ in 0..count {
            state.players[player.0 as usize].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn seed_pool_with_restriction(
        state: &mut GameState,
        player: PlayerId,
        color: ManaType,
        restriction: ManaRestriction,
    ) {
        let _ = state.add_mana_to_pool(
            player,
            ManaUnit::new(color, ObjectId(0), false, vec![restriction]),
        );
    }

    fn expect_mana_ability_context(context: ManaChoiceContext) -> Box<PendingManaAbility> {
        match context {
            ManaChoiceContext::ManaAbility(pending) => pending,
            other => panic!("expected mana ability context, got {other:?}"),
        }
    }

    #[test]
    fn mana_api_type_detected_as_mana_ability() {
        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn is_mana_ability_serialized_only_when_true() {
        // The AbilityDefinition Serialize impl emits the derived `is_mana_ability`
        // UI key (skip_serializing_if = is_false), so the client routes mana-tap
        // affordances off this engine flag instead of introspecting the effect AST.
        let mana = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mana_json = serde_json::to_value(&mana).unwrap();
        assert_eq!(mana_json["is_mana_ability"], serde_json::json!(true));

        let non_mana = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )
        .cost(AbilityCost::Tap);
        let non_mana_json = serde_json::to_value(&non_mana).unwrap();
        assert!(non_mana_json.get("is_mana_ability").is_none());
    }

    #[test]
    fn non_mana_api_type_not_detected() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn targeted_mana_producing_ability_is_not_mana_ability() {
        // CR 605.1a: If a mana-producing ability has targets, it must use the stack.
        let mut def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        def.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn draw_ability_is_not_mana_ability() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Tap);
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn mana_with_delayed_trigger_sub_remains_mana_ability() {
        let mut head = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 2 },
        });
        head.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::ThisTurn,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CopySpell {
                        target: TargetFilter::TriggeringSource,
                        retarget: CopyRetargetPermission::KeepOriginalTargets,
                        copier: None,
                        additional_modifications: Vec::new(),
                        starting_loyalty_from_casualty_sacrifice: false,
                    },
                )),
                uses_tracked_set: false,
            },
        )));
        assert!(
            is_mana_ability(&head),
            "CR 605.1: chained delayed triggers do not disqualify activated mana abilities"
        );
    }

    #[test]
    fn resolve_mana_ability_sub_chain_registers_delayed_trigger() {
        use crate::types::ability::{
            CopyRetargetPermission, DelayedTriggerCondition, FilterProp, TriggerDefinition,
            TypedFilter,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Magus Lucea Kane".to_string(),
            Zone::Battlefield,
        );

        let copy_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CopySpell {
                target: TargetFilter::TriggeringSource,
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: Vec::new(),
                starting_loyalty_from_casualty_sacrifice: false,
            },
        );
        let delayed = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new({
                        let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast);
                        trigger.valid_card = Some(TargetFilter::Typed(
                            TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
                        ));
                        trigger.valid_target = Some(TargetFilter::Controller);
                        trigger
                    }),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::ThisTurn,
                },
                effect: Box::new(copy_effect),
                uses_tracked_set: false,
            },
        );
        let mut def = make_mana_ability(ManaProduction::Colorless {
            count: QuantityExpr::Fixed { value: 2 },
        });
        def.sub_ability = Some(Box::new(delayed));

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.delayed_triggers.len(), 1);
        assert!(state.objects.get(&obj_id).unwrap().tapped);
    }

    #[test]
    fn mana_with_roll_die_sub_remains_mana_ability() {
        // CR 605.1: mana abilities remain mana abilities regardless of other
        // generated effects (Vexing Puzzlebox rolls a d20 inline).
        let mut def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
        )));
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn resolve_mana_ability_sub_chain_emits_die_rolled() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1011),
            PlayerId(0),
            "Vexing Puzzlebox".to_string(),
            Zone::Battlefield,
        );
        let mut def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: None,
            },
        )));
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, source, PlayerId(0), &def, &mut events, None).unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::DieRolled {
                    sides: 20,
                    result: Some(_),
                    ..
                }
            )),
            "RollDie sub_ability must resolve inline after mana production"
        );
    }

    #[test]
    fn mana_with_mana_sub_remains_mana_ability() {
        let mut def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Blue],
            contribution: ManaContribution::Base,
        });
        def.sub_ability = Some(Box::new(make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Red],
            contribution: ManaContribution::Base,
        })));
        assert!(is_mana_ability(&def));
    }

    #[test]
    fn resolve_mana_ability_produces_mana_and_taps() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn condition_instead_mana_ability_without_counter_produces_base_mana() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gemstone Caverns".to_string(),
            Zone::Battlefield,
        );
        let ability = gemstone_caverns_mana_ability();
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert_eq!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);
    }

    #[test]
    fn condition_instead_mana_ability_with_luck_counter_prompts_for_any_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gemstone Caverns".to_string(),
            Zone::Battlefield,
        );
        let ability = gemstone_caverns_mana_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.counters
            .insert(CounterType::Generic("luck".to_string()), 1);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            context,
        } = waiting
        else {
            panic!("expected ChooseManaColor, got {waiting:?}");
        };
        assert_eq!(player, PlayerId(0));
        assert_eq!(
            options,
            vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
        );

        let pending = expect_mana_ability_context(context);
        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: options.clone(),
            },
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            0
        );
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);
    }

    /// CR 305.2a + CR 608.2c + CR 605.1a + CR 605.3b: River of Tears, built end-
    /// to-end from its Oracle text via `parse_oracle_text`, swaps {U}→{B} at
    /// resolution exactly when the controller has played a land this turn. This
    /// exercises both branches of `apply_condition_instead_mana_swap` against the
    /// *parsed* AST (parser + runtime integration proof).
    #[test]
    fn river_of_tears_mana_swaps_blue_to_black_after_land_played() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "{T}: Add {U}. If you played a land this turn, add {B} instead.",
            "River of Tears",
            &[],
            &["Land".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1, "single mana ability");
        let ability = parsed.abilities[0].clone();

        let mut state = GameState::new_two_player(42);

        // No land played this turn (lands_played_this_turn == 0): base {U}.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "River of Tears".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability.clone());
        assert_eq!(state.players[0].lands_played_this_turn, 0);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert_eq!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source).unwrap().tapped);

        // A land has now been played this turn: the {U}→{B} instead-swap fires.
        state.players[0].mana_pool.clear();
        state.players[0].lands_played_this_turn = 1;
        let source2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "River of Tears".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&source2).unwrap().abilities)
            .push(ability.clone());

        let mut events2 = Vec::new();
        activate_mana_ability(
            &mut state,
            source2,
            PlayerId(0),
            0,
            &ability,
            &mut events2,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.objects.get(&source2).unwrap().tapped);
    }

    #[test]
    fn exhaust_mana_ability_only_once_is_enforced_and_emits_mana_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Loot, the Pathfinder".to_string(),
            Zone::Battlefield,
        );
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .activation_restrictions(vec![ActivationRestriction::OnlyOnce]);
        def.ability_tag = Some(AbilityTag::Exhaust);
        Arc::make_mut(&mut state.objects.get_mut(&obj_id).unwrap().abilities).push(def.clone());

        let mut events = Vec::new();
        activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        let second = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );

        assert!(second.is_err());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(0),
                source_id,
                is_mana_ability: true,
            } if *source_id == obj_id
        )));
    }

    #[test]
    fn exhaust_prompted_mana_ability_records_after_choice() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Exhaust Filter".to_string(),
            Zone::Battlefield,
        );
        let mut def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Base,
                    color_options: vec![ManaColor::White, ManaColor::Blue],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .activation_restrictions(vec![ActivationRestriction::OnlyOnce]);
        def.ability_tag = Some(AbilityTag::Exhaust);
        Arc::make_mut(&mut state.objects.get_mut(&obj_id).unwrap().abilities).push(def.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::KeywordAbilityActivated { .. })));
        let WaitingFor::ChooseManaColor {
            choice, context, ..
        } = waiting
        else {
            panic!("expected ChooseManaColor");
        };
        let pending = expect_mana_ability_context(context);

        handle_choose_mana_color(
            &mut state,
            &pending,
            &choice,
            ManaChoice::SingleColor(ManaType::White),
            &mut events,
        )
        .unwrap();
        let second = activate_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );

        assert!(second.is_err());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::KeywordAbilityActivated {
                ability_tag: AbilityTag::Exhaust,
                player_id: PlayerId(0),
                source_id,
                is_mana_ability: true,
            } if *source_id == obj_id
        )));
    }

    // CR 106.6: A mana ability that attaches a spend restriction (Flamebraider:
    // "Spend this mana only to cast Elemental spells or activate abilities of
    // Elemental sources") must thread that restriction onto every produced
    // `ManaUnit`. Previously `produce_mana_from_ability` destructured
    // `Effect::Mana { produced, .. }` and discarded `restrictions`, so the
    // mana landed in the pool unrestricted.
    #[test]
    fn resolve_mana_ability_attaches_spend_restrictions() {
        use crate::types::ability::ManaSpendRestriction;
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Flamebraider".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Fixed { value: 2 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                },
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type: "Elemental".to_string(),
                    ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                }],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 2);
        // Every produced unit must carry the Elemental restriction.
        for unit in &pool.mana {
            assert_eq!(
                unit.restrictions,
                vec![
                    crate::types::mana::ManaRestriction::OnlyForTypeSpellsOrAbilities {
                        spell_type: "Elemental".to_string(),
                        ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                    }
                ],
                "Flamebraider mana must carry Elemental restriction"
            );
        }

        // Spending for a non-Elemental creature must fail.
        use crate::types::mana::{PaymentContext, SpellMeta};
        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
        };
        let goblin_ctx = PaymentContext::Spell(&goblin_spell);
        let mut pool_clone = pool.clone();
        let first_color = pool_clone.mana[0].color;
        assert!(
            pool_clone.spend_for(first_color, &goblin_ctx).is_none(),
            "Flamebraider mana must not be spendable on non-Elemental spells"
        );

        // Spending for an Elemental creature succeeds.
        let elemental_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
        };
        let elemental_ctx = PaymentContext::Spell(&elemental_spell);
        assert!(
            pool_clone.spend_for(first_color, &elemental_ctx).is_some(),
            "Flamebraider mana must be spendable on Elemental spells"
        );

        // CR 106.6: The ability-activation half of the OR. A non-Elemental
        // source's activation context must reject Elemental-restricted mana;
        // an Elemental source's activation context must accept it.
        let non_elemental_types = vec!["Creature".to_string()];
        let non_elemental_subtypes = vec!["Goblin".to_string()];
        let non_elemental_activation = PaymentContext::Activation {
            source_types: &non_elemental_types,
            source_subtypes: &non_elemental_subtypes,
            ability_tag: None,
        };
        let mut pool_clone2 = pool.clone();
        assert!(
            pool_clone2
                .spend_for(first_color, &non_elemental_activation)
                .is_none(),
            "Flamebraider mana must not pay non-Elemental source's ability cost"
        );

        let elemental_subtypes = vec!["Elemental".to_string()];
        let elemental_activation = PaymentContext::Activation {
            source_types: &non_elemental_types,
            source_subtypes: &elemental_subtypes,
            ability_tag: None,
        };
        assert!(
            pool_clone2
                .spend_for(first_color, &elemental_activation)
                .is_some(),
            "Flamebraider mana must pay an Elemental source's ability cost"
        );
    }

    #[test]
    fn resolve_mana_ability_fails_if_already_tapped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().tapped = true;

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        let result = resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None);

        assert!(result.is_err());
    }

    #[test]
    fn resolve_mana_ability_colorless_produced() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sol Ring".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    /// CR 614.1a positive case: the `Add {C}{C}{C} instead` sub-ability is a
    /// replacement effect (the word "instead" per CR 614.1a). When its `And`
    /// condition is satisfied (all three Urza lands controlled), the delta
    /// replaces the base `Add {C}` production and the pool ends with three
    /// colorless mana.
    #[test]
    fn resolve_mana_ability_conditional_urza_delta() {
        let mut state = GameState::new_two_player(42);
        let tower = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Urza's Tower".to_string(),
            Zone::Battlefield,
        );
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Urza's Mine".to_string(),
            Zone::Battlefield,
        );
        let plant = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Urza's Power Plant".to_string(),
            Zone::Battlefield,
        );
        for (id, subtype) in [(tower, "Tower"), (mine, "Mine"), (plant, "Power-Plant")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Urza's".to_string());
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .condition(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Mine".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Power-Plant".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                ],
            }),
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, tower, PlayerId(0), &ability, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            3
        );
    }

    /// CR 614.1a negative case: when the sub-ability's "instead" replacement
    /// (CR 614.1a) cannot fire because its condition is false (here, Urza's
    /// Power-Plant is missing), only the base `Add {C}` resolves and the
    /// `And { Mine, Power-Plant }` delta does not apply — the pool ends with
    /// one colorless, not three. Mirrors
    /// `resolve_mana_ability_conditional_urza_delta` but omits Power-Plant.
    #[test]
    fn resolve_mana_ability_urza_delta_skips_when_companion_land_missing() {
        let mut state = GameState::new_two_player(42);
        let tower = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Urza's Tower".to_string(),
            Zone::Battlefield,
        );
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Urza's Mine".to_string(),
            Zone::Battlefield,
        );
        // Note: no Urza's Power Plant — the `And` condition cannot be
        // satisfied, so the sub-ability must not fire.
        for (id, subtype) in [(tower, "Tower"), (mine, "Mine")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Urza's".to_string());
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .condition(AbilityCondition::And {
                conditions: vec![
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Mine".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                    AbilityCondition::ControllerControlsMatching {
                        filter: TargetFilter::Typed(
                            TypedFilter::land()
                                .subtype("Power-Plant".to_string())
                                .controller(ControllerRef::You),
                        ),
                    },
                ],
            }),
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, tower, PlayerId(0), &ability, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1,
            "with Power-Plant absent the And condition is false and only the base \
             Add {{C}} fires; pool = {:?}",
            state.players[0].mana_pool.mana,
        );
    }

    #[test]
    fn resolve_mana_ability_fixed_multi_color_produces_each_unit() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hybrid Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::White, ManaColor::Blue],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn hand_self_exile_mana_ability_is_legal_and_exiles_source() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        let source = create_object(
            &mut state,
            CardId(157),
            player,
            "Elvish Spirit Guide".to_string(),
            Zone::Hand,
        );

        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone: Some(Zone::Hand),
            count: 1,
        });
        ability.activation_zone = Some(Zone::Hand);
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability);

        let actions = crate::ai_support::legal_actions(&state);
        assert!(actions.iter().any(|action| matches!(
            action,
            crate::types::actions::GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == source
        )));

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .expect("hand-zone self-exile mana ability should activate");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.objects[&source].zone, Zone::Exile);
        assert!(!state.players[0].hand.contains(&source));
    }

    #[test]
    fn resolve_composite_cost_taps_and_sacrifices() {
        // CR 111.10a + CR 605.3b: Treasure — Composite {Tap, Sacrifice} mana ability
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, obj_id, PlayerId(0), &def, &mut events, None).unwrap();

        // Mana was produced
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        // Object was sacrificed (moved out of battlefield)
        let obj = state.objects.get(&obj_id);
        assert!(
            obj.is_none() || obj.unwrap().zone != Zone::Battlefield,
            "Treasure should be sacrificed (removed from battlefield)"
        );
        // Events include both tap and sacrifice
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    /// CR 605.1a + CR 701.17a: Millikin — `{T}, Mill a card: Add {C}`. The mill
    /// is a non-mana cost component of a mana ability. Regression: before the
    /// `Mill` arm existed in the mana-ability cost payer, paying the cost errored
    /// (`Unsupported mana ability sub-cost: Mill`), so the readiness simulation
    /// in `can_activate_mana_ability_now` failed and the ability was never
    /// offered — the user could not tap Millikin for mana.
    #[test]
    fn millikin_mills_a_card_and_adds_colorless() {
        let mut state = GameState::new_two_player(42);
        // Stock player 0's library so there is a card to mill.
        create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Island".to_string(),
            Zone::Library,
        );

        // Millikin is an artifact creature; mark it un-sick so the {T} gate passes.
        let id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Millikin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = false;
        }

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        let library_before = state.players[0].library.len();

        // The user-facing regression: the ability must be offered as activatable.
        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "Millikin's {{T}}, Mill a card: Add {{C}} ability must be activatable"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        // {C} produced, source tapped, exactly one card milled to the graveyard.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(state.objects.get(&id).unwrap().tapped);
        assert_eq!(state.players[0].library.len(), library_before - 1);
        assert_eq!(state.players[0].graveyard.len(), 1);
    }

    /// CR 118.3: Wall of Roots — `Put a -0/-1 counter on ~: Add {G}`. The cost is
    /// an `EffectCost` (put-counter-on-self), delegated to the single-authority
    /// cost payer. Regression: before the self-contained delegation arm, this
    /// errored and the ability was never offered.
    #[test]
    fn wall_of_roots_effect_cost_adds_green() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Wall of Roots".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::EffectCost {
            effect: Box::new(Effect::PutCounter {
                counter_type: CounterType::PowerToughness {
                    power: 0,
                    toughness: -1,
                },
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }),
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        let counters = &state.objects.get(&id).unwrap().counters;
        assert_eq!(
            counters
                .get(&CounterType::PowerToughness {
                    power: 0,
                    toughness: -1
                })
                .copied(),
            Some(1),
            "the -0/-1 counter cost was paid onto Wall of Roots"
        );
    }

    /// CR 107.14: Aether Hub class — `{T}, Pay {E}: Add {C}`. The `PayEnergy`
    /// sub-cost delegates to the single-authority cost payer alongside the tap.
    #[test]
    fn energy_cost_mana_ability_spends_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 1;
        let id = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Aether Hub".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });
        // `can_activate_mana_ability_now` re-fetches the ability from the object,
        // so it must be attached at the activation index.
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "energy-cost mana ability must be activatable with enough energy"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(state.players[0].energy, 0, "the {{E}} cost was spent");
    }

    /// CR 107.14: With insufficient energy the `PayEnergy` cost is unpayable
    /// (CR 118.3), so the ability is not offered.
    #[test]
    fn energy_cost_mana_ability_unavailable_without_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 0;
        let id = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Aether Hub".to_string(),
            Zone::Battlefield,
        );
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());
        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), id, 0, &def),
            "energy-cost mana ability must NOT be activatable without energy"
        );
    }

    /// CR 107.6 + CR 302.6: Pili-Pala — `{Q}: Add {C}`. The untap symbol requires
    /// a currently-tapped source; once paid the source untaps. A summoning-sick
    /// creature can't activate it (CR 302.6 names {Q} alongside {T}).
    #[test]
    fn untap_cost_mana_ability_requires_tapped_source() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(33),
            PlayerId(0),
            "Pili-Pala".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = false;
            obj.tapped = true;
        }

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Untap);
        // `can_activate_mana_ability_now` re-fetches the ability from the object.
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def.clone());

        // Tapped + un-sick: activatable.
        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Untapped: the {Q} cost can't be paid, so it's not offered.
        state.objects.get_mut(&id).unwrap().tapped = false;
        assert!(!can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Summoning sick (while tapped): CR 302.6 blocks {Q}.
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.tapped = true;
            obj.summoning_sick = true;
        }
        assert!(!can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            id,
            0,
            &def
        ));

        // Pay it: untaps and produces {C}.
        state.objects.get_mut(&id).unwrap().summoning_sick = false;
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(!state.objects.get(&id).unwrap().tapped, "{{Q}} untapped it");
    }

    /// CR 118.3 + CR 602.2b: Grinning Ignus — `Return ~ to its owner's hand:
    /// Add {C}`. The self-`ReturnToHand` cost delegates to the single-authority
    /// cost payer; the source ends up in hand.
    #[test]
    fn self_return_to_hand_cost_mana_ability() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(34),
            PlayerId(0),
            "Grinning Ignus".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: None,
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert_eq!(
            state.objects.get(&id).unwrap().zone,
            Zone::Hand,
            "source returned to hand as the cost"
        );
    }

    /// CR 701.43: Oasis Ritualist class — `{T}, Exert ~: Add {C}`. The `Exert`
    /// sub-cost delegates to the single-authority cost payer alongside the tap.
    #[test]
    fn exert_cost_mana_ability_taps_and_produces() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(35),
            PlayerId(0),
            "Oasis Ritualist".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().summoning_sick = false;

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Exert],
        });

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, id, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(state.objects.get(&id).unwrap().tapped);
    }

    /// CR 605.1a: A loyalty ability that adds mana (Chandra `[+1]: Add {R}{R}`)
    /// is NOT a mana ability — it uses the stack and obeys loyalty timing. The
    /// classifier must exclude it; an otherwise-identical `{T}` cost stays a mana
    /// ability.
    #[test]
    fn loyalty_ability_is_not_a_mana_ability() {
        let mana_effect = || Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Red, ManaColor::Red],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };

        let loyalty_ability = AbilityDefinition::new(AbilityKind::Activated, mana_effect())
            .cost(AbilityCost::Loyalty { amount: 1 });
        assert!(
            !is_mana_ability(&loyalty_ability),
            "a [+1]: Add {{R}}{{R}} loyalty ability is not a mana ability (CR 605.1a)"
        );

        let tap_ability =
            AbilityDefinition::new(AbilityKind::Activated, mana_effect()).cost(AbilityCost::Tap);
        assert!(
            is_mana_ability(&tap_ability),
            "the same effect with a {{T}} cost remains a mana ability"
        );
    }

    /// Build a Treasure-style token — `{T}, Sacrifice this: Add one mana of any
    /// color` over `colors` — attached as ability index 0. The
    /// `Composite { Tap, Sacrifice SelfRef }` cost is choice-free, so two
    /// definition-identical copies are batchable twins (CR 605.3a).
    fn make_any_color_treasure(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        colors: Vec<ManaColor>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: colors,
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
    }

    /// Build a creature with a pure `{T}: Add one mana of any color` ability at
    /// index 0. Unlike `make_any_color_treasure`, the Creature core type makes it
    /// subject to the CR 302.6 summoning-sickness gate, so `summoning_sick`
    /// controls whether the `{T}` mana ability is currently ready.
    fn make_tap_any_color_creature(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        summoning_sick: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            "Mana Dork".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.summoning_sick = summoning_sick;
        }
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
    }

    #[test]
    fn token_treasure_choose_color_uses_activation_snapshot_after_self_sacrifice() {
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9000, PlayerId(0), ManaColor::ALL.to_vec());
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.is_token = true;
            let ability = Arc::make_mut(&mut obj.abilities).get_mut(0).unwrap();
            let Effect::Mana {
                produced: ManaProduction::AnyOneColor { count, .. },
                ..
            } = ability.effect.as_mut()
            else {
                panic!("test treasure must have AnyOneColor mana production");
            };
            *count = QuantityExpr::Fixed { value: 2 };
        }

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("token Treasure should activate into a color prompt");
        assert!(
            matches!(result.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "Goldspan-style Treasure should wait for a color choice"
        );
        let mut sba_events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut sba_events);
        assert!(
            !state.objects.contains_key(&treasure),
            "a sacrificed token Treasure has ceased to exist before the color choice"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        )
        .expect("color choice must resolve from the activation-time ability snapshot");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            2,
            "Goldspan-style Treasure adds two mana of the chosen color"
        );
    }

    /// CR 605.3a: One color choice with `count = N` activates the tapped source
    /// plus `N - 1` identical, choice-free twins — `N` mana of the chosen color,
    /// `N` sources sacrificed, and a per-source tap each twin (the events a
    /// sacrifice observer such as Mayhem Devil/Korvold sees).
    #[test]
    fn batch_activation_taps_multiple_identical_treasures() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9001, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9002, PlayerId(0), ManaColor::ALL.to_vec());
        let c = make_any_color_treasure(&mut state, 9003, PlayerId(0), ManaColor::ALL.to_vec());

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("Treasure should activate into a color prompt");

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            vec![b, c],
            "the other two Treasures are batchable twins"
        );

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 3,
            },
        )
        .expect("bulk color choice should resolve");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            3,
            "three Treasures each produced one red"
        );
        let on_battlefield = [a, b, c]
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.zone == Zone::Battlefield)
            })
            .count();
        assert_eq!(on_battlefield, 0, "all three Treasures were sacrificed");
        // CR 106.12 + CR 605.3a: each twin taps independently during the choice
        // step (the first source was tapped earlier, before the prompt).
        let twin_taps = result
            .events
            .iter()
            .filter(|e| matches!(e, GameEvent::PermanentTapped { .. }))
            .count();
        assert_eq!(twin_taps, 2, "two twins tapped during the bulk activation");
    }

    /// CR 605.3a: Activating one of `N` identical batchable Treasures must compute
    /// the sibling set in linear time. Pre-fix, `batch_eligible_siblings` filtered
    /// each candidate through `activatable_mana_options`, which cloned + simulated +
    /// recursed `can_activate_mana_ability_now`, giving O(N!) readiness calls. The
    /// single-authority non-simulating predicate caps the count at O(N).
    #[test]
    fn bulk_treasure_activation_is_linear_not_factorial() {
        const N: usize = 6;
        let mut state = GameState::new_two_player(42);
        let ids: Vec<ObjectId> = (0..N)
            .map(|i| {
                make_any_color_treasure(
                    &mut state,
                    9200 + i as u64,
                    PlayerId(0),
                    ManaColor::ALL.to_vec(),
                )
            })
            .collect();

        MANA_READINESS_CALLS.store(0, Ordering::Relaxed);
        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: ids[0],
                ability_index: 0,
            },
        )
        .expect("Treasure should activate into a color prompt");

        // O(N!) pre-fix blows far past this; O(N) post-fix stays well under it.
        assert!(
            MANA_READINESS_CALLS.load(Ordering::Relaxed) <= 4 * N,
            "readiness calls must be linear in N (got {}, bound {})",
            MANA_READINESS_CALLS.load(Ordering::Relaxed),
            4 * N
        );

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            ids[1..].to_vec(),
            "the other five Treasures are batchable twins"
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: N as u32,
            },
        )
        .expect("bulk color choice should resolve");
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            N,
            "all six Treasures each produced one red"
        );
    }

    /// CR 106.12: A tapped `{T}: Add` source can't pay its tap cost, so the cheap
    /// gate (`mana_ability_ready_without_simulation`) rejects it BEFORE the skip
    /// shortcut and before any legality clone — A(a).
    #[test]
    fn cheap_gate_rejects_tapped_tap_mana_source_without_clone() {
        let mut state = GameState::new_two_player(42);
        let dork = make_tap_any_color_creature(&mut state, 9300, PlayerId(0), false);
        state.objects.get_mut(&dork).unwrap().tapped = true;
        let def = state.objects.get(&dork).unwrap().abilities[0].clone();

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), dork, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            !activatable,
            "a tapped {{T}} mana source can't pay its tap cost (CR 106.12)"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "cheap gate rejects before any legality clone"
        );
    }

    /// CR 601.2g: A `Composite{{Tap, Sacrifice}}` mana cost (Treasure) is NOT
    /// conclusively decided by the cheap gate, so it must still simulate — the
    /// must-simulate path is preserved (clone >= 1) even though it is activatable.
    /// A(b). The self-sacrifice is always a legal target, so this does NOT build a
    /// cost that passes `is_payable` yet fails simulation.
    #[test]
    fn composite_tap_sacrifice_still_simulates() {
        let mut state = GameState::new_two_player(42);
        let treasure =
            make_any_color_treasure(&mut state, 9301, PlayerId(0), ManaColor::ALL.to_vec());
        let def = state.objects.get(&treasure).unwrap().abilities[0].clone();

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), treasure, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "an untapped Treasure with a legal self-sacrifice is activatable"
        );
        assert!(
            snap.state_clone_for_legality >= 1,
            "a Composite with a Sacrifice component must still simulate (CR 601.2g)"
        );
    }

    /// CR 605.3a + CR 106.12: A ready plain `{T}: Add` source is activatable and
    /// its `{T}`-only cost is conclusively payable by the cheap gate, so NO
    /// legality clone is taken — B (revert-failing: pre-fix takes one clone here).
    #[test]
    fn plain_tap_mana_source_skips_legality_clone() {
        let mut state = GameState::new_two_player(42);
        let dork = make_tap_any_color_creature(&mut state, 9302, PlayerId(0), false);
        let def = state.objects.get(&dork).unwrap().abilities[0].clone();

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), dork, 0, &def);
        let snap = crate::game::perf_counters::snapshot();

        assert!(activatable, "a ready {{T}}: Add source is activatable");
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "a {{T}}-only mana cost is conclusively payable by the cheap gate — no clone"
        );
    }

    /// CR 601.2g: A filter land's `Composite{{Mana, Tap}}` cost still simulates —
    /// the cheap-gate skip must not apply to mana sub-costs. Affordable pool keeps
    /// it activatable (behavior preserved). C — behavior-preservation only, so we
    /// do NOT assert a zero clone count.
    #[test]
    fn filter_land_composite_still_activatable_via_simulation() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        crate::game::perf_counters::reset();
        let activatable = can_activate_mana_ability_now(&state, PlayerId(0), ruins, 0, &ability);
        let snap = crate::game::perf_counters::snapshot();

        assert!(
            activatable,
            "an affordable filter land remains activatable (behavior preserved)"
        );
        assert!(
            snap.state_clone_for_legality >= 1,
            "a Composite{{Mana, Tap}} cost must still simulate (CR 601.2g)"
        );
    }

    /// CR 605.3a: The board-wide mana-display sweep over N untapped `{T}: Add`
    /// sources takes ZERO legality clones — the headline regression. Pre-fix every
    /// source cloned + simulated (N clones, the Cryptolith-Rite clone-storm). D.
    #[test]
    fn mana_display_sweep_is_clone_free_for_tap_only_sources() {
        const N: usize = 8;
        let mut state = GameState::new_two_player(42);
        for i in 0..N {
            make_tap_any_color_creature(&mut state, 9400 + i as u64, PlayerId(0), false);
        }
        assert_eq!(
            state.battlefield.len(),
            N,
            "board has exactly N {{T}}: Add sources"
        );

        crate::game::public_state::mark_mana_display_dirty(&mut state);
        crate::game::perf_counters::reset();
        crate::game::derived::derive_display_state(&mut state);
        let snap = crate::game::perf_counters::snapshot();

        assert_eq!(
            snap.mana_display_sweeps, 1,
            "exactly one board-wide mana sweep"
        );
        assert_eq!(
            snap.mana_display_swept_objects, N as u64,
            "the sweep visited all N battlefield objects"
        );
        assert_eq!(
            snap.state_clone_for_legality, 0,
            "no per-source legality clone for {{T}}-only sources (revert-failing: pre-fix = N clones)"
        );
    }

    /// Direct classifier unit tests for `AbilityCost::all_components_cheap_gate_covered`
    /// and the `cost_conclusively_payable_by_cheap_gate` wrapper anchor guard.
    #[test]
    fn cheap_gate_cost_classification_units() {
        assert!(AbilityCost::Tap.all_components_cheap_gate_covered());
        assert!(AbilityCost::Untap.all_components_cheap_gate_covered());
        assert!(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Untap]
        }
        .all_components_cheap_gate_covered());
        assert!(!AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Mana {
                    cost: ManaCost::generic(1)
                }
            ]
        }
        .all_components_cheap_gate_covered());
        assert!(!AbilityCost::Mana {
            cost: ManaCost::generic(1)
        }
        .all_components_cheap_gate_covered());
        // Empty composite is vacuously all()-true at the classifier level...
        assert!(AbilityCost::Composite { costs: vec![] }.all_components_cheap_gate_covered());

        // ...but the wrapper's {T}/{Q} anchor guards the degenerate empty
        // Composite, and a None cost is conclusively payable (no cost to pay).
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(&None));
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &Some(AbilityCost::Composite { costs: vec![] })
        ));
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(
            &Some(AbilityCost::Tap)
        ));
    }

    /// Hostile classifier coverage: costs whose every component is NOT a
    /// tap/untap symbol must NOT be skipped (the wrapper returns false, so the
    /// caller falls through to full simulation). Mill needs a populated library
    /// and EffectCost an arbitrary effect, so these are asserted at the
    /// classifier/wrapper level rather than as full runtime cards; the runtime
    /// "falls through to simulate" path itself is exercised by
    /// `composite_tap_sacrifice_still_simulates` (A(b)) and
    /// `filter_land_composite_still_activatable_via_simulation` (C).
    #[test]
    fn cheap_gate_hostile_costs_must_simulate() {
        let tap_mill = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, AbilityCost::Mill { count: 1 }],
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &tap_mill
        ));

        let effect_cost = Some(AbilityCost::EffectCost {
            effect: Box::new(Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }),
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &effect_cost
        ));

        // Bare Mana-only cost (no {T}) — the wrapper's anchor requires a {T}/{Q}
        // component, so a mana-only cost is never skipped.
        let mana_only = Some(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        assert!(!mana_sources::cost_conclusively_payable_by_cheap_gate(
            &mana_only
        ));

        // None cost is conclusively payable (covered above too) — sanity anchor.
        assert!(mana_sources::cost_conclusively_payable_by_cheap_gate(&None));
    }

    /// CR 302.6 / CR 702.10: A summoning-sick creature's `{T}` mana ability is not a
    /// batch sibling, but granting Haste lifts the gate so the twin batches again.
    #[test]
    fn batch_excludes_summoning_sick_tap_mana_creature() {
        let mut state = GameState::new_two_player(42);
        let ready = make_tap_any_color_creature(&mut state, 9600, PlayerId(0), false);
        let sick = make_tap_any_color_creature(&mut state, 9601, PlayerId(0), true);
        let def = state.objects.get(&ready).unwrap().abilities[0].clone();

        // CR 302.6: summoning-sick {T} mana creature is NOT a batch sibling.
        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            !siblings.contains(&sick),
            "summoning-sick {{T}} mana creature must not batch (CR 302.6)"
        );

        // CR 702.10: Haste lifts the gate → the twin becomes a valid sibling.
        state
            .objects
            .get_mut(&sick)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Haste);
        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            siblings.contains(&sick),
            "a hasty {{T}} mana creature IS a batch sibling (CR 702.10)"
        );
    }

    /// CR 702.26b: Phased-out permanents are treated as though they do not
    /// exist, so a phased-out mana source cannot batch with a visible twin.
    #[test]
    fn batch_excludes_phased_out_mana_sibling() {
        let mut state = GameState::new_two_player(42);
        let ready = make_any_color_treasure(&mut state, 9700, PlayerId(0), ManaColor::ALL.to_vec());
        let phased =
            make_any_color_treasure(&mut state, 9701, PlayerId(0), ManaColor::ALL.to_vec());
        let def = state.objects.get(&ready).unwrap().abilities[0].clone();

        let mut events = Vec::new();
        crate::game::phasing::phase_out_object(
            &mut state,
            phased,
            crate::game::game_object::PhaseOutCause::Directly,
            &mut events,
        );

        let siblings = batch_eligible_siblings(&state, PlayerId(0), ready, &def);
        assert!(
            !siblings.contains(&phased),
            "phased-out mana sources must not batch (CR 702.26b)"
        );
        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), phased, 0, &def),
            "phased-out mana sources must fail the readiness gate (CR 702.26b)"
        );
        let rejected = activate_mana_ability(
            &mut state,
            phased,
            PlayerId(0),
            0,
            &def,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        );
        assert!(
            matches!(rejected, Err(EngineError::ActionNotAllowed(_))),
            "phased-out mana sources must fail executor activation (CR 702.26b)"
        );
    }

    /// CR 605.3a: A count larger than the available sources is rejected before
    /// any mana is produced — no partial application.
    #[test]
    fn batch_activation_rejects_count_above_available() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9101, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9102, PlayerId(0), ManaColor::ALL.to_vec());

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate first Treasure");

        let rejected = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 5,
            },
        );
        assert!(
            rejected.is_err(),
            "count 5 with only two sources is illegal"
        );
        assert!(
            state
                .objects
                .get(&b)
                .is_some_and(|o| o.zone == Zone::Battlefield),
            "the sibling is untouched by the rejected batch"
        );
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "no mana is produced when the batch is rejected"
        );
    }

    /// CR 605.3b: The default `count = 1` resolves a single source — twins are
    /// left untouched (back-compatible single-tap behavior).
    #[test]
    fn batch_activation_default_count_resolves_single_source() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9151, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9152, PlayerId(0), ManaColor::ALL.to_vec());

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate first Treasure");
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        )
        .expect("single color choice resolves");

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert!(
            state
                .objects
                .get(&b)
                .is_some_and(|o| o.zone == Zone::Battlefield),
            "the sibling remains untapped on the battlefield"
        );
    }

    /// CR 605.3a: Only definition-identical twins batch together — a different
    /// any-color source (distinct ability) is excluded.
    #[test]
    fn batch_groups_only_identical_ability_definitions() {
        let mut state = GameState::new_two_player(42);
        let a = make_any_color_treasure(&mut state, 9201, PlayerId(0), ManaColor::ALL.to_vec());
        let b = make_any_color_treasure(&mut state, 9202, PlayerId(0), ManaColor::ALL.to_vec());
        // Distinct AbilityDefinition (only W/U) → not a twin of the 5-color pair.
        let _other = make_any_color_treasure(
            &mut state,
            9203,
            PlayerId(0),
            vec![ManaColor::White, ManaColor::Blue],
        );

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: a,
                ability_index: 0,
            },
        )
        .expect("activate the 5-color Treasure");

        let WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(pending),
            ..
        } = &result.waiting_for
        else {
            panic!("expected ChooseManaColor, got {:?}", result.waiting_for);
        };
        assert_eq!(
            pending.batch_siblings,
            vec![b],
            "only the identical 5-color Treasure is offered as a twin"
        );
    }

    /// CR 605.3a: `cost_resolves_without_choice` is the batch eligibility gate —
    /// a deny-by-default whitelist of `Tap`, self-sacrifice, and `Composite`s of
    /// those. Any choice-bearing or unrecognized cost is excluded.
    #[test]
    fn cost_resolves_without_choice_whitelist() {
        // Treasure: Tap + self-sacrifice → batchable.
        assert!(cost_resolves_without_choice(&Some(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }
        )));
        assert!(cost_resolves_without_choice(&Some(AbilityCost::Tap)));
        assert!(cost_resolves_without_choice(&None));

        // Phyrexian Altar: sacrifice a (non-self) creature → requires a choice.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::creature()),
                1
            ))
        )));
        // Self-sacrifice of more than one is not the single-token shape.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 2))
        )));
        // Filter-land style mana sub-cost requires a payment choice.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 1,
                        },
                    },
                    AbilityCost::Tap,
                ],
            }
        )));
        // Pay-life is non-interactive but conservatively excluded (deny-by-default).
        assert!(!cost_resolves_without_choice(&Some(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        })));
    }

    #[test]
    fn resolve_composite_cost_taps_pays_life_and_produces_mana() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );

        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::White, ManaColor::Blue],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        });

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .unwrap();

        assert!(state.objects.get(&obj_id).unwrap().tapped);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged {
                player_id,
                amount: -1,
            } if *player_id == PlayerId(0)
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::ManaAdded { .. })));
    }

    #[test]
    fn lions_eye_diamond_discards_hand_and_then_produces_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let led = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Lion's Eye Diamond".to_string(),
            Zone::Battlefield,
        );
        let c1 = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Card One".to_string(),
            Zone::Hand,
        );
        let c2 = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Card Two".to_string(),
            Zone::Hand,
        );

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 3 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Discard {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::HandSize {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&led).unwrap().abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            led,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::Discard,
                count,
                choices: cards,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 2);
                *pending_mana_ability
            }
            other => panic!("expected PayCost Discard (mana ability), got {other:?}"),
        };

        let waiting = handle_discard_for_mana_ability(
            &mut state,
            2,
            &[c1, c2],
            &pending,
            &[c1, c2],
            &mut events,
        )
        .unwrap();

        let pending = match waiting {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                context,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(options.len(), 5);
                *expect_mana_ability_context(context)
            }
            other => panic!("expected ChooseManaColor, got {other:?}"),
        };

        assert!(!state.players[0].hand.contains(&c1));
        assert!(!state.players[0].hand.contains(&c2));
        assert!(state.players[0].graveyard.contains(&c1));
        assert!(state.players[0].graveyard.contains(&c2));
        assert_ne!(
            state.objects.get(&led).map(|obj| obj.zone),
            Some(Zone::Battlefield)
        );

        handle_choose_mana_color(
            &mut state,
            &pending,
            &ManaChoicePrompt::SingleColor {
                options: vec![
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Green,
                ],
            },
            ManaChoice::SingleColor(ManaType::Red),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 3);
    }

    /// Helper: build a Pit-of-Offerings-style permanent with a `{T}: Add one mana
    /// of any of the exiled cards' colors` mana ability and exile a card linked
    /// to it via `state.exile_links` (the same relation populated by the
    /// `ChangeZone` resolver during the ETB trigger).
    fn pit_of_offerings_with_exiled_card(
        state: &mut GameState,
        owner: PlayerId,
        exiled_card_name: &str,
        exiled_colors: Vec<ManaColor>,
    ) -> (ObjectId, ObjectId) {
        let pit = create_object(
            state,
            CardId(1000),
            owner,
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pit).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
        let exiled = create_object(
            state,
            CardId(2000),
            owner,
            exiled_card_name.to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&exiled).unwrap().color = exiled_colors;
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: pit,
            kind: ExileLinkKind::TrackedBySource,
        });
        (pit, exiled)
    }

    #[test]
    fn pit_of_offerings_with_no_exiled_colored_cards_produces_no_mana() {
        // CR 605.1a + CR 106.5: With zero linked colored exiles the ability has
        // no defined mana type — produces no mana even though the tap cost is
        // paid (the ability is still legal to activate per CR 605.3a).
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pit).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert!(state.objects.get(&pit).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.total(), 0);
        // can_activate_mana_ability_now confirms it's still legal — paying the
        // tap is a valid resolution even when no mana is produced.
    }

    #[test]
    fn pit_of_offerings_colorless_exiled_card_produces_no_mana() {
        // CR 106.5: A Mountain card itself has no `colors` (red is implied via
        // its mana ability, not by intrinsic color). For Pit of Offerings the
        // relevant property is the exiled card's printed colors; a card with
        // no printed colors contributes nothing.
        let mut state = GameState::new_two_player(42);
        let (pit, _exiled) =
            pit_of_offerings_with_exiled_card(&mut state, PlayerId(0), "Mountain", vec![]);

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn pit_of_offerings_with_one_colored_exile_produces_that_color() {
        // Single colored exile (Island = Blue): the only legal mana type is {U}.
        let mut state = GameState::new_two_player(42);
        let (pit, _) = pit_of_offerings_with_exiled_card(
            &mut state,
            PlayerId(0),
            "Savannah Lions",
            vec![ManaColor::White],
        );

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, pit, PlayerId(0), &def, &mut events, None).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn pit_of_offerings_color_options_excludes_colorless_exiles() {
        // CR 605.1a + CR 106.5: With a colorless `Mountain` and a blue `Island`
        // exiled, only `{U}` is a legal mana option.
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let mountain = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Exile,
        );
        // Mountain's intrinsic `color` is empty (its red identity comes from its
        // mana ability, not its colors field).
        state.objects.get_mut(&mountain).unwrap().color = vec![];
        let island = create_object(
            &mut state,
            CardId(2002),
            PlayerId(0),
            "Island".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&island).unwrap().color = vec![];
        let counterspell = create_object(
            &mut state,
            CardId(2003),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&counterspell).unwrap().color = vec![ManaColor::Blue];

        for exiled in [mountain, island, counterspell] {
            state.exile_links.push(ExileLink {
                exiled_id: exiled,
                source_id: pit,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        // Direct query of the option set: only blue should be legal.
        let options = crate::game::effects::mana::exiled_color_options(
            &state,
            LinkedExileScope::ThisObject,
            pit,
        );
        assert_eq!(options, vec![ManaType::Blue]);
    }

    #[test]
    fn pit_of_offerings_color_override_picks_chosen_color() {
        // Two colored exiles → two legal mana types. With a `color_override`,
        // the ability produces exactly that color (mirrors AnyOneColor).
        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let white_card = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "White Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&white_card).unwrap().color = vec![ManaColor::White];
        let blue_card = create_object(
            &mut state,
            CardId(2002),
            PlayerId(0),
            "Blue Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&blue_card).unwrap().color = vec![ManaColor::Blue];

        for exiled in [white_card, blue_card] {
            state.exile_links.push(ExileLink {
                exiled_id: exiled,
                source_id: pit,
                kind: ExileLinkKind::TrackedBySource,
            });
        }

        let def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            pit,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    #[test]
    fn pit_of_offerings_etb_exile_populates_links_then_mana_ability_consumes_them() {
        // End-to-end: drive the ETB-style exile through the actual `change_zone`
        // resolver so `state.exile_links` is auto-populated by the engine
        // (mirrors how Pit of Offerings' "When this land enters, exile up to
        // three target cards from graveyards" trigger resolves), then activate
        // the colored mana ability and confirm it produces a color drawn from
        // the just-exiled cards.
        use crate::types::ability::{Effect as Ef, ResolvedAbility, TargetFilter, TargetRef};

        let mut state = GameState::new_two_player(42);
        let pit = create_object(
            &mut state,
            CardId(1000),
            PlayerId(0),
            "Pit of Offerings".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&pit)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&pit).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Ef::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        // Place a single colored creature card in the graveyard for Pit's ETB
        // trigger to exile via `ChangeZone`.
        let lions = create_object(
            &mut state,
            CardId(2001),
            PlayerId(0),
            "Savannah Lions".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&lions).unwrap().color = vec![ManaColor::White];

        // Resolve Pit's ETB exile through the real `change_zone` resolver. This
        // is the same path the trigger system uses; a successful Exile move
        // should automatically push an `ExileLink::TrackedBySource` into
        // `state.exile_links` (see `change_zone::execute_zone_move`).
        let etb = ResolvedAbility::new(
            Ef::ChangeZone {
                origin: Some(Zone::Graveyard),
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
            vec![TargetRef::Object(lions)],
            pit,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::change_zone::resolve(&mut state, &etb, &mut events).unwrap();

        // Sanity: the ETB resolver populated the link.
        assert!(
            state
                .exile_links
                .iter()
                .any(|link| link.source_id == pit && link.exiled_id == lions),
            "ETB-style exile must populate state.exile_links via the standard \
             change_zone resolver (CR 610.3)"
        );

        // Now activate the colored mana ability. With one white-colored exiled
        // card, the only legal mana type is `{W}`.
        let mana_def = state.objects.get(&pit).unwrap().abilities[0].clone();
        let mut mana_events = Vec::new();
        resolve_mana_ability(
            &mut state,
            pit,
            PlayerId(0),
            &mana_def,
            &mut mana_events,
            None,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
    }

    #[test]
    fn pit_of_offerings_blink_clears_exile_links() {
        // CR 400.7: A blink (LTB then re-ETB) creates a new object that inherits
        // no linkage — the re-entered Pit's mana ability must not read the old
        // incarnation's exiled cards.
        //
        // CR 607.2a: `TrackedBySource` links now SURVIVE the leg into exile (a
        // self-exiled source stays the linked-ability referent for its pile —
        // Mechtitan Core); the blink reset happens on RE-ENTRY, not on the exit.
        // While Pit sits in exile the preserved link is inert (its "{T}: add mana
        // among cards exiled with ~" ability can only be activated on the
        // battlefield), so this staged blink still ends with no stale linkage.
        let mut state = GameState::new_two_player(42);
        let (pit, _exiled) = pit_of_offerings_with_exiled_card(
            &mut state,
            PlayerId(0),
            "Llanowar Elves",
            vec![ManaColor::Green],
        );

        assert_eq!(state.exile_links.len(), 1, "precondition: link was created");

        // LTB into exile: the link is preserved (CR 607.2a durability), not yet cleared.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, pit, Zone::Exile, &mut events);
        assert!(
            state.exile_links.iter().any(|link| link.source_id == pit),
            "an exit into exile preserves the TrackedBySource link (CR 607.2a)"
        );

        // Re-ETB (the blink completes): the new object sheds the stale linkage.
        crate::game::zones::move_to_zone(&mut state, pit, Zone::Battlefield, &mut events);
        assert!(
            state.exile_links.iter().all(|link| link.source_id != pit),
            "TrackedBySource exile links must be cleared when the source re-enters \
             the battlefield as a new object (CR 400.7)"
        );
    }

    #[test]
    fn color_override_produces_specified_color() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Any Color Source".to_string(),
            Zone::Battlefield,
        );

        let def = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::White, ManaColor::Blue, ManaColor::Black],
            contribution: ManaContribution::Base,
        });
        let mut events = Vec::new();
        // Override to produce Black specifically
        resolve_mana_ability(
            &mut state,
            obj_id,
            PlayerId(0),
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Black)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
    }

    // ─────────────────────────────────────────────────────────────
    // is_triggered_mana_ability — CR 605.1b classifier edge cases.
    // ─────────────────────────────────────────────────────────────

    fn mana_producing_resolved() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn draw_resolved() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn tapped_for_mana_event() -> GameEvent {
        GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            produced: vec![ManaType::Green],
            tap_state: crate::types::events::ManaTapState::FromTap,
        }
    }

    #[test]
    fn classifier_accepts_head_effect_mana_on_tapped_for_mana() {
        let ability = mana_producing_resolved();
        assert!(is_triggered_mana_ability(
            &ability,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_non_tapped_for_mana_event() {
        // CR 605.1b criterion (b) + CR 106.12a: only a `TappedForMana` event
        // (a `{T}`-cost mana ability resolving) qualifies. An unrelated event
        // (e.g. `AbilityActivated`) must not route through the inline resolver.
        let ability = mana_producing_resolved();
        let ev = GameEvent::AbilityActivated {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            kind: crate::types::events::ActivatedAbilityKind::Normal,
        };
        assert!(!is_triggered_mana_ability(&ability, Some(&ev)));
    }

    #[test]
    fn classifier_accepts_all_mana_chain() {
        // CR 605.1b criterion (c): every reachable link must be mana. A chain
        // with head + sub both producing mana (e.g., "add G, then add G") is
        // inline-safe.
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(mana_producing_resolved()));
        assert!(is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_mixed_mana_plus_non_mana_chain() {
        // CR 605.1b criterion (c): "every link is mana" — a chain with mana
        // at the head but a non-mana sub (e.g., draw a card) MUST use the
        // stack. Routing such a chain inline would silently perform the
        // non-mana effect without giving players priority.
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_chain_without_any_mana_effect() {
        let mut head = draw_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_sub_ability_with_multi_target() {
        // CR 605.1b criterion (a) + CR 115.6: any link declaring targets
        // anywhere in the chain disqualifies inline resolution.
        let mut sub = mana_producing_resolved();
        sub.multi_target = Some(MultiTargetSpec::fixed(1, 1));
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(sub));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_rejects_sub_ability_with_resolved_targets() {
        // Symmetric to multi_target: a non-empty `targets` vec (as produced
        // by auto_select_targets_for_ability at trigger time) on any link
        // also disqualifies. Covers the `|| multi_target.is_some()` branch
        // separately from the `!targets.is_empty()` branch.
        let mut sub = mana_producing_resolved();
        sub.targets = vec![crate::types::ability::TargetRef::Object(ObjectId(99))];
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(sub));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_walks_else_ability_for_criterion_c() {
        // CR 608.2c: `else_ability` is the "Otherwise" branch of a
        // conditional ability. A mana head with a non-mana `else_ability`
        // (e.g. "if X, add G; otherwise draw a card") must still use the
        // stack — inline resolution of the else branch would skip priority
        // on the draw.
        let mut head = mana_producing_resolved();
        head.else_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn classifier_walks_else_ability_for_criterion_a() {
        // Mirror for criterion (a): a targeted `else_ability` branch
        // disqualifies even when the main chain is target-free.
        let mut else_branch = mana_producing_resolved();
        else_branch.targets = vec![crate::types::ability::TargetRef::Object(ObjectId(7))];
        let mut head = mana_producing_resolved();
        head.else_ability = Some(Box::new(else_branch));
        assert!(!is_triggered_mana_ability(
            &head,
            Some(&tapped_for_mana_event())
        ));
    }

    #[test]
    fn inline_triggered_mana_ability_resolves_trigger_event_mana_type() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::TriggerEventManaType,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            ObjectId(77),
            PlayerId(0),
        );
        let event = GameEvent::TappedForMana {
            player_id: PlayerId(0),
            source_id: ObjectId(1),
            produced: vec![ManaType::Red],
            tap_state: crate::types::events::ManaTapState::FromTap,
        };
        let mut events = Vec::new();

        resolve_triggered_mana_ability_inline(
            &mut state,
            &ability,
            Some(&event),
            &mut events,
            None,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.current_trigger_event.is_none());
    }

    #[test]
    fn taps_for_mana_trigger_adds_trigger_event_mana_to_triggering_player() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Mountain".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let mana_flare = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mana Flare".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&mana_flare)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(TypedFilter::land())),
            );

        crate::game::triggers::process_triggers(
            &mut state,
            &[GameEvent::TappedForMana {
                player_id: PlayerId(1),
                source_id: land,
                produced: vec![ManaType::Red],
                tap_state: crate::types::events::ManaTapState::FromTap,
            }],
        );

        assert_eq!(state.players[0].mana_pool.total(), 0);
        assert_eq!(state.players[1].mana_pool.count_color(ManaType::Red), 1);
    }

    #[test]
    fn taps_for_mana_cant_untap_trigger_binds_triggering_land() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        let land = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            obj.tapped = true;
        }

        let vorinclex = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        let duration = Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::Controller,
        };
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::GenericEffect {
                                static_abilities: vec![StaticDefinition::new(
                                    StaticMode::CantUntap,
                                )
                                .affected(TargetFilter::ParentTarget)
                                .modifications(vec![ContinuousModification::AddStaticMode {
                                    mode: StaticMode::CantUntap,
                                }])],
                                duration: Some(duration.clone()),
                                target: Some(TargetFilter::TriggeringSource),
                            },
                        )
                        .duration(duration),
                    )
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::Opponent),
                    )),
            );

        crate::game::triggers::process_triggers(
            &mut state,
            &[GameEvent::TappedForMana {
                player_id: PlayerId(1),
                source_id: land,
                produced: vec![ManaType::Green],
                tap_state: crate::types::events::ManaTapState::FromTap,
            }],
        );
        assert_eq!(state.stack.len(), 1);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(state.transient_continuous_effects.iter().any(|effect| {
            effect.affected == (TargetFilter::SpecificObject { id: land })
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    })
        }));

        crate::game::turns::execute_untap(&mut state, &mut events);
        assert!(state.objects[&land].tapped);
        assert!(state.transient_continuous_effects.is_empty());
    }

    #[test]
    fn activate_any_one_color_pauses_for_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        state.turn_number = 3;

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(options, &[ManaType::Red, ManaType::Green]);
            }
            _ => panic!("expected ChooseManaColor::SingleColor, got {:?}", result),
        }
    }

    #[test]
    fn handle_choose_mana_color_produces_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::SingleColor {
            options: vec![ManaType::Red, ManaType::Green],
        };
        let mut events = Vec::new();

        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();

        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "should resume to Priority"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            1,
            "should have 1 green mana"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            0,
            "should have 0 red mana"
        );
    }

    // --- Foraging Wickermaw: "Add one mana of any color. This creature becomes
    // that color until end of turn." (parser arm + mana-producer color record) ---

    /// Parse Foraging Wickermaw's verbatim activated line into its single mana
    /// ability. The `{1}` cost, `AnyOneColor` head, `becomes that color` sub-chain
    /// (now `AddChosenColor`), UEOT duration, and once-each-turn restriction all
    /// come from the real parser pipeline.
    fn foraging_wickermaw_mana_ability() -> AbilityDefinition {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "{1}: Add one mana of any color. This creature becomes that color until end of turn. Activate only once each turn.",
            "Foraging Wickermaw",
            &[],
            &["Creature".to_string()],
            &[],
        );
        assert_eq!(
            parsed.abilities.len(),
            1,
            "expected exactly one activated mana ability"
        );
        parsed.abilities.into_iter().next().unwrap()
    }

    fn foraging_wickermaw_setup() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Foraging Wickermaw".to_string(),
            Zone::Battlefield,
        );
        let ability = foraging_wickermaw_mana_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(ability);
        (state, source)
    }

    fn wubrg_single_color_prompt() -> ManaChoicePrompt {
        ManaChoicePrompt::SingleColor {
            options: vec![
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ],
        }
    }

    fn pending_for(source: ObjectId) -> PendingManaAbility {
        PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_snapshot: None,
            ability_index: 0,
            rules_execution_node: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        }
    }

    /// Drive the real color-choice completion handler (the entry point the engine
    /// dispatches a `ChooseManaColor` answer to), then recompute layers.
    fn activate_and_choose(state: &mut GameState, source: ObjectId, color: ManaType) {
        let pending = pending_for(source);
        let mut events = Vec::new();
        handle_choose_mana_color(
            state,
            &pending,
            &wubrg_single_color_prompt(),
            ManaChoice::SingleColor(color),
            &mut events,
        )
        .unwrap();
        crate::game::layers::mark_layers_full(state);
        crate::game::layers::flush_layers(state);
    }

    /// The load-bearing discriminating test: the creature's color is a FUNCTION of
    /// the mana choice (Red -> Red, Blue -> Blue), impossible to pass with any
    /// baked/first-color constant. Reverting EITHER half fails:
    ///  - parser arm removed -> become stays `Effect::Unimplemented` -> no color change;
    ///  - mana-producer record removed -> `AddChosenColor` reads `chosen_color()==None`.
    #[test]
    fn foraging_wickermaw_becomes_the_produced_color() {
        for (chosen, expected) in [
            (ManaType::Red, ManaColor::Red),
            (ManaType::Blue, ManaColor::Blue),
        ] {
            let (mut state, source) = foraging_wickermaw_setup();
            activate_and_choose(&mut state, source, chosen);
            assert_eq!(
                state.objects[&source].color,
                vec![expected],
                "creature must become the color of the mana it produced ({chosen:?})"
            );
            assert_eq!(
                state.players[0].mana_pool.count_color(chosen),
                1,
                "the produced mana also lands in the pool"
            );
        }
    }

    /// CR 400.7: `chosen_attributes` persist on the permanent across turns (cleared
    /// only on zone change). A later activation must OVERWRITE the stored `Color`,
    /// not accumulate — `chosen_color()` is first-match. Reverting the retain-drop
    /// to a plain push leaves `[Color(Red), Color(Green)]` and reads stale Red.
    #[test]
    fn foraging_wickermaw_reactivation_replaces_color_not_accumulates() {
        let (mut state, source) = foraging_wickermaw_setup();

        // Turn N: choose Red.
        activate_and_choose(&mut state, source, ManaType::Red);
        assert_eq!(state.objects[&source].color, vec![ManaColor::Red]);

        // Cross the turn boundary: the turn-N UEOT color effect expires, but the
        // stored `ChosenAttribute::Color(Red)` persists on the permanent (CR 400.7).
        crate::game::layers::prune_end_of_turn_effects(&mut state);
        state.turn_number += 1;
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);

        // Turn N+1: choose Green.
        activate_and_choose(&mut state, source, ManaType::Green);

        assert_eq!(
            state.objects[&source].color,
            vec![ManaColor::Green],
            "re-activation replaces the stored color (not stale Red, not [Red, Green])"
        );
        let color_attrs = state.objects[&source]
            .chosen_attributes
            .iter()
            .filter(|a| matches!(a, ChosenAttribute::Color(_)))
            .count();
        assert_eq!(
            color_attrs, 1,
            "exactly one stored Color attribute after re-activation"
        );
    }

    /// Gate airtightness: a bare `Add one mana of any color` ability with NO
    /// `becomes that color` clause must NOT record `ChosenAttribute::Color` — zero
    /// blast radius for basics / City of Brass / painlands / filter lands.
    /// Reverting the gate (unconditional write) makes this producer gain a spurious
    /// `Color` attribute. The pool assertion is a positive reach-guard proving the
    /// write site was reached and merely gated, not skipped upstream.
    #[test]
    fn plain_any_color_producer_records_no_chosen_color() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "City of Brass".to_string(),
            Zone::Battlefield,
        );
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: ManaColor::ALL.to_vec(),
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities).push(ability);

        let pending = pending_for(source);
        let mut events = Vec::new();
        handle_choose_mana_color(
            &mut state,
            &pending,
            &wubrg_single_color_prompt(),
            ManaChoice::SingleColor(ManaType::Red),
            &mut events,
        )
        .unwrap();

        assert!(
            state.objects[&source]
                .chosen_attributes
                .iter()
                .all(|a| !matches!(a, ChosenAttribute::Color(_))),
            "a plain any-color producer must not record a chosen color (gate must suppress the write)"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            1,
            "reach-guard: the ability really did produce mana"
        );
    }

    /// Parser-shape (reach-guard): the verbatim become clause lowers to a
    /// `GenericEffect` carrying `AddChosenColor` with UEOT duration, and NOTHING in
    /// the chain is `Effect::Unimplemented` (proving it parsed past the old
    /// fallthrough, not vacuously).
    #[test]
    fn foraging_wickermaw_become_clause_parses_to_add_chosen_color() {
        let ability = foraging_wickermaw_mana_ability();
        assert!(
            matches!(&*ability.effect, Effect::Mana { .. }),
            "head is the mana production"
        );
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("become sub-ability present");
        match &*sub.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert!(
                    static_abilities.iter().any(|s| s
                        .modifications
                        .iter()
                        .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. }))),
                    "become clause maps to AddChosenColor"
                );
                assert!(
                    *duration == Some(Duration::UntilEndOfTurn)
                        || sub.duration == Some(Duration::UntilEndOfTurn),
                    "color change lasts until end of turn"
                );
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
        assert!(
            !matches!(&*sub.effect, Effect::Unimplemented { .. }),
            "become did not fall through to Unimplemented"
        );
        assert!(!matches!(&*ability.effect, Effect::Unimplemented { .. }));
    }

    #[test]
    fn handle_choose_mana_color_resolves_pain_land_damage_for_each_color() {
        for chosen in [ManaType::Green, ManaType::White] {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(77),
                PlayerId(0),
                "Brushland".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());

            let pending = PendingManaAbility {
                player: PlayerId(0),
                source_id: source,
                ability_index: 0,
                rules_execution_node: None,
                ability_snapshot: None,
                color_override: None,
                resume: ManaAbilityResume::Priority,
                cost_move_resume: None,
                chosen_tappers: Vec::new(),
                chosen_discards: Vec::new(),
                chosen_mana_payment: None,
                chosen_counter_count: None,
                chosen_x: None,
                collected_evidence: Vec::new(),
                chosen_exiled: Vec::new(),
                chosen_sacrificed_battlefield: Vec::new(),
                cost_paid_object: None,
                batch_siblings: Vec::new(),
            };
            let prompt = ManaChoicePrompt::SingleColor {
                options: vec![ManaType::Green, ManaType::White],
            };
            let mut events = Vec::new();

            let result = handle_choose_mana_color(
                &mut state,
                &pending,
                &prompt,
                ManaChoice::SingleColor(chosen),
                &mut events,
            )
            .unwrap();

            assert!(matches!(result, WaitingFor::Priority { .. }));
            assert_eq!(state.players[0].mana_pool.count_color(chosen), 1);
            assert_eq!(state.players[0].life, 19);
        }
    }

    #[test]
    fn color_override_bypasses_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let ability = make_mana_ability(ManaProduction::AnyOneColor {
            count: QuantityExpr::Fixed { value: 1 },
            color_options: vec![ManaColor::Red, ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        state.turn_number = 3;

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "auto-tap with color_override should resolve immediately"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn color_override_pain_land_still_deals_damage() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(78),
            PlayerId(0),
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = brushland_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
        assert_eq!(state.players[0].life, 19);
    }

    // ─────────────────────────────────────────────────────────────
    // ChoiceAmongCombinations (filter lands — Shadowmoor/Eventide).
    // ─────────────────────────────────────────────────────────────

    fn sunken_ruins_colored_ability() -> AbilityDefinition {
        // CR 605.3b + CR 106.1a: `{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}`.
        // The real printed cost is composite: one hybrid `{U/B}` plus `{T}`.
        // Tests must use the real shape — truncating to `AbilityCost::Tap`
        // masks the Composite + Mana sub-cost bug path.
        use crate::types::mana::{ManaCost, ManaCostShard};
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongCombinations {
                    options: vec![
                        vec![ManaColor::Blue, ManaColor::Blue],
                        vec![ManaColor::Blue, ManaColor::Black],
                        vec![ManaColor::Black, ManaColor::Black],
                    ],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::BlueBlack],
                        generic: 0,
                    },
                },
                AbilityCost::Tap,
            ],
        })
    }

    #[test]
    fn activate_filter_land_prompts_with_combination_options() {
        // CR 605.3b: Manual activation of a filter land (no override) must
        // surface a Combination prompt, not a SingleColor prompt.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = sunken_ruins_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        // Seed the pool with one {U} so the `{U/B}` sub-cost has a single
        // unambiguous plan — this test focuses on the output Combination
        // prompt, not the input mana-payment prompt.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { options },
                ..
            } => {
                assert_eq!(
                    options,
                    &vec![
                        vec![ManaType::Blue, ManaType::Blue],
                        vec![ManaType::Blue, ManaType::Black],
                        vec![ManaType::Black, ManaType::Black],
                    ]
                );
            }
            _ => panic!("expected ChooseManaColor::Combination, got {:?}", result),
        }
        // CR 605.3b: tap cost is paid before the prompt.
        assert!(state.objects.get(&ruins).unwrap().tapped);
        // CR 601.2h + CR 107.4e: {U/B} sub-cost was debited from the seeded pool — only
        // the two combination-produced units remain.
        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn handle_choose_combination_produces_exact_sequence() {
        // CR 605.3b: The chosen combination lands verbatim in the pool.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        let mut events = Vec::new();

        handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::Combination(vec![ManaType::Blue, ManaType::Black]),
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn combination_override_bypasses_choice_and_produces_exact_mana() {
        // Auto-tap path: override short-circuits the prompt and emits the
        // combination atomically.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let ability = sunken_ruins_colored_ability();
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        // Seed one {B} so the {U/B} sub-cost is unambiguously payable; the
        // auto-tap path then short-circuits both mana-payment and
        // combination-choice prompts.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::Combination(vec![
                ManaType::Blue,
                ManaType::Black,
            ])),
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        // Pool starts with 1 {B}; {U/B} sub-cost debits that {B}; production
        // adds 1 {U} + 1 {B} per the override.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn handle_choose_rejects_mismatched_choice_shape() {
        // A SingleColor answer to a Combination prompt must error out.
        let mut state = GameState::new_two_player(42);
        let ruins = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        let mut events = Vec::new();
        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Blue),
            &mut events,
        );
        assert!(result.is_err(), "mismatched shape must be rejected");
    }

    // ─────────────────────────────────────────────────────────────
    // Filter-land mana sub-cost regression tests.
    // CR 605.3a + CR 601.2h + CR 107.4e.
    // ─────────────────────────────────────────────────────────────

    fn setup_sunken_ruins(state: &mut GameState) -> (ObjectId, AbilityDefinition) {
        let ruins = create_object(
            state,
            CardId(500),
            PlayerId(0),
            "Sunken Ruins".to_string(),
            Zone::Battlefield,
        );
        let ability = sunken_ruins_colored_ability();
        let obj = state.objects.get_mut(&ruins).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        (ruins, ability)
    }

    #[test]
    fn filter_land_auto_pays_unambiguous_mana_sub_cost() {
        // CR 605.3a + CR 107.4e: Pool has only {U}; the single legal plan
        // auto-pays without surfacing `PayManaAbilityMana`. The flow then
        // lands on `ChooseManaColor` for the combination output.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(
            matches!(
                result,
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::Combination { .. },
                    ..
                }
            ),
            "expected ChooseManaColor after unambiguous mana-sub-cost auto-pay, got {:?}",
            result,
        );
        // Pool had 1 {U}; sub-cost debited it.
        assert_eq!(state.players[0].mana_pool.total(), 0);
        // Tap component also paid.
        assert!(state.objects.get(&ruins).unwrap().tapped);
    }

    #[test]
    fn filter_land_taps_another_source_when_matching_pool_mana_is_spell_only() {
        // CR 106.6 + CR 605.3a: Spell-only {U} already in the pool cannot pay
        // this mana ability's {U/B} activation cost. The engine must ignore it
        // during hybrid-plan discovery and retain the nested mana-ability path,
        // which taps the Island for an eligible {U}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        let island = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }
        seed_pool_with_restriction(
            &mut state,
            PlayerId(0),
            ManaType::Blue,
            ManaRestriction::OnlyForSpell,
        );

        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            ruins,
            0,
            &ability,
        ));

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { .. },
                ..
            }
        ));
        assert!(state.objects.get(&island).unwrap().tapped);
        assert!(state.objects.get(&ruins).unwrap().tapped);
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.mana[0].color, ManaType::Blue);
        assert_eq!(
            pool.mana[0].restrictions,
            vec![ManaRestriction::OnlyForSpell]
        );
    }

    #[test]
    fn filter_land_excludes_ineligible_hybrid_payment_option() {
        // CR 106.6: Of the apparent {U/B} assignments, spell-only {U} is not
        // legal for an activation while unrestricted {B} is. The engine must
        // auto-select the sole legal plan instead of publishing both options.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with_restriction(
            &mut state,
            PlayerId(0),
            ManaType::Blue,
            ManaRestriction::OnlyForSpell,
        );
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::Combination { .. },
                ..
            }
        ));
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.count_color(ManaType::Blue), 1);
        assert_eq!(pool.count_color(ManaType::Black), 0);
        assert_eq!(
            pool.mana[0].restrictions,
            vec![ManaRestriction::OnlyForSpell]
        );
    }

    #[test]
    fn fixed_filter_land_activates_by_tapping_other_mana_source_for_sub_cost() {
        // CR 117.1d + CR 118.2 + CR 602.2b + CR 605.3a: A mana ability with a
        // mana activation cost may activate other mana abilities while paying
        // that cost. Skycloud Expanse class: "{1}, {T}: Add {W}{U}."
        let mut state = GameState::new_two_player(42);
        let forest = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }
        let skycloud = create_object(
            &mut state,
            CardId(502),
            PlayerId(0),
            "Skycloud Expanse".to_string(),
            Zone::Battlefield,
        );
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::White, ManaColor::Blue],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Tap,
            ],
        });
        {
            let obj = state.objects.get_mut(&skycloud).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(ability.clone());
        }

        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            skycloud,
            0,
            &ability,
        ));

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            skycloud,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.objects.get(&forest).unwrap().tapped);
        assert!(state.objects.get(&skycloud).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.total(), 2);
    }

    #[test]
    fn filter_land_prompts_for_ambiguous_hybrid_mana_payment() {
        // CR 107.4e + CR 601.2h: Pool has one {U} and one {B}. Both color
        // assignments for the {U/B} hybrid are legal, so the engine pauses
        // at `PayManaAbilityMana` with both options.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        match &result {
            WaitingFor::PayManaAbilityMana { options, .. } => {
                let expected_u = vec![ManaType::Blue];
                let expected_b = vec![ManaType::Black];
                assert!(options.contains(&expected_u));
                assert!(options.contains(&expected_b));
                assert_eq!(options.len(), 2);
            }
            _ => panic!("expected PayManaAbilityMana, got {:?}", result),
        }
        // Tap MUST NOT have happened yet — cost payment is atomic: if the
        // prompt is still pending, no part of the cost has been paid.
        // (The Composite handler pays all sub-costs in order, after the
        // hybrid plan is resolved.)
        assert!(
            !state.objects.get(&ruins).unwrap().tapped,
            "source must not be tapped while mana payment is pending",
        );
    }

    #[test]
    fn filter_land_resume_with_blue_choice_produces_requested_combination() {
        // End-to-end: enter PayManaAbilityMana, pick {U}, then resume and
        // pick the {U}{U} combination. Pool debits {U} for cost, produces
        // {U}{U}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let (options, pending) = match result {
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            } => (options, pending_mana_ability),
            other => panic!("expected PayManaAbilityMana, got {:?}", other),
        };

        let pay_result = handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Blue],
            &mut events,
        )
        .unwrap();

        // Now at ChooseManaColor::Combination, and the {U} has been debited.
        assert!(
            matches!(
                pay_result,
                WaitingFor::ChooseManaColor {
                    choice: ManaChoicePrompt::Combination { .. },
                    ..
                }
            ),
            "expected ChooseManaColor after PayManaAbilityMana",
        );
        // {U} debited, {B} still in pool (only the hybrid shard consumed one mana).
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert!(state.objects.get(&ruins).unwrap().tapped);

        let combo_pending = match pay_result {
            WaitingFor::ChooseManaColor { context, .. } => expect_mana_ability_context(context),
            other => panic!("unexpected variant: {:?}", other),
        };
        let combo_prompt = ManaChoicePrompt::Combination {
            options: vec![
                vec![ManaType::Blue, ManaType::Blue],
                vec![ManaType::Blue, ManaType::Black],
                vec![ManaType::Black, ManaType::Black],
            ],
        };
        handle_choose_mana_color(
            &mut state,
            &combo_pending,
            &combo_prompt,
            ManaChoice::Combination(vec![ManaType::Blue, ManaType::Blue]),
            &mut events,
        )
        .unwrap();

        // Produced {U}{U}; plus the {B} still floating.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 2);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn filter_land_resume_with_black_choice_debits_black_from_pool() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Blue, 1);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            ruins,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        let (options, pending) = match waiting {
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            } => (options, pending_mana_ability),
            other => panic!("expected PayManaAbilityMana, got {:?}", other),
        };

        handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Black],
            &mut events,
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn filter_land_colored_ability_not_activatable_with_empty_pool() {
        // CR 605.3a + CR 601.2h: Payability gate — colored filter-land
        // ability must not surface as activatable when the pool has no
        // {U} or {B}.
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        // Pool intentionally empty of {U}/{B}; put one {G} so pool isn't totally empty.
        seed_pool_with(&mut state, PlayerId(0), ManaType::Green, 1);

        assert!(
            !can_activate_mana_ability_now(&state, PlayerId(0), ruins, 0, &ability),
            "filter-land colored ability must be un-activatable without the mana to pay {{U/B}}",
        );
    }

    #[test]
    fn filter_land_colored_ability_activatable_with_sufficient_pool() {
        let mut state = GameState::new_two_player(42);
        let (ruins, ability) = setup_sunken_ruins(&mut state);
        seed_pool_with(&mut state, PlayerId(0), ManaType::Black, 1);
        assert!(can_activate_mana_ability_now(
            &state,
            PlayerId(0),
            ruins,
            0,
            &ability,
        ));
    }

    #[test]
    fn chosen_color_devotion_mana_ability_uses_activation_choice_for_count() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let nykthos = create_object(
            &mut state,
            CardId(8100),
            player,
            "Nykthos, Shrine to Nyx".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nykthos)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let green_permanent = create_object(
            &mut state,
            CardId(8101),
            player,
            "Green Permanent".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&green_permanent).unwrap().mana_cost =
            crate::types::mana::ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 0,
            };

        let ability = make_mana_ability(ManaProduction::ChosenColor {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Devotion {
                    colors: DevotionColors::ChosenColor,
                },
            },
            contribution: ManaContribution::Base,
            fixed_alternative: None,
        });
        Arc::make_mut(&mut state.objects.get_mut(&nykthos).unwrap().abilities)
            .push(ability.clone());

        let prompt = mana_choice_prompt(&ability.effect, &state, nykthos, None)
            .expect("chosen-color mana should prompt for a color");
        assert!(matches!(prompt, ManaChoicePrompt::SingleColor { .. }));

        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            nykthos,
            player,
            &ability,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 2);
    }

    /// Issue #460 + CR 106.12a: Vorinclex's `TapsForMana` trigger must fire
    /// **once per mana-ability resolution**, not once per mana unit. Activating
    /// Nykthos for 9 green (devotion = 9) plus a single Vorinclex fire = 10
    /// green total. Pre-fix the per-`ManaAdded` trigger scan fired Vorinclex 9
    /// times → 18. Drives the real action pipeline (ActivateAbility → pay {2}
    /// from pool → ChooseManaColor → Green).
    #[test]
    fn nykthos_with_vorinclex_produces_exactly_ten_green() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        // Nykthos: {2}{T}, choose a color, add mana equal to devotion to it.
        let nykthos = create_object(
            &mut state,
            CardId(8200),
            player,
            "Nykthos, Shrine to Nyx".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&nykthos)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let nykthos_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Devotion {
                            colors: DevotionColors::ChosenColor,
                        },
                    },
                    contribution: ManaContribution::Base,
                    fixed_alternative: None,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
                AbilityCost::Tap,
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&nykthos).unwrap().abilities)
            .push(nykthos_ability);

        // Vorinclex: whenever a land you control is tapped for mana, add one
        // mana of any type that land produced. Two green pips ({G}{G}).
        let vorinclex = create_object(
            &mut state,
            CardId(8201),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&vorinclex).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 5,
            };
            obj.trigger_definitions.push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))
                    .valid_target(TargetFilter::Controller),
            );
        }

        // Seven more single-green-pip permanents → devotion to green = 9.
        for i in 0..7 {
            let pip = create_object(
                &mut state,
                CardId(8300 + i),
                player,
                format!("Green Pip {i}"),
                Zone::Battlefield,
            );
            state.objects.get_mut(&pip).unwrap().mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        // Seed the pool with {2} so Nykthos's generic cost is paid without
        // tapping any other land (keeps the test focused on Nykthos's tap).
        seed_pool_with(&mut state, player, ManaType::Colorless, 2);

        let result = crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: nykthos,
                ability_index: 0,
            },
        )
        .expect("Nykthos's {2}{T} ability should activate");
        assert!(
            matches!(result.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "expected ChooseManaColor, got {:?}",
            result.waiting_for
        );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            },
        )
        .expect("color choice should resolve");

        // 9 green from Nykthos (devotion) + 1 from a single Vorinclex fire = 10.
        // Pre-fix: Vorinclex fired once per `ManaAdded` (9×) → 18.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            10,
            "Nykthos 9 + Vorinclex 1 = 10 green (NOT 18 from per-unit firing)"
        );
        assert_eq!(state.players[0].mana_pool.total(), 10);
    }

    /// Regression: a 1-mana `{T}` producer with Vorinclex out yields exactly
    /// 2 — proving the `TapsForMana` trigger fires exactly once per resolution.
    #[test]
    fn one_mana_producer_with_vorinclex_yields_exactly_two() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        let forest = create_object(
            &mut state,
            CardId(8400),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        let vorinclex = create_object(
            &mut state,
            CardId(8401),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    ))
                    .valid_target(TargetFilter::Controller),
            );

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: forest,
                ability_index: 0,
            },
        )
        .expect("Forest's {T} ability should activate");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            2,
            "1 base + 1 Vorinclex = 2 (single fire on a 1-mana producer)"
        );
    }

    /// Issue #465 — true pipeline regression for the `valid_card` controller
    /// scope on "Whenever you tap a land for mana" triggers. Drives `apply` /
    /// `apply_as_current` (`ActivateAbility` → mana resolution → trigger
    /// matching), not hand-built state.
    ///
    /// CR 603.2 + CR 106.12a: the trigger event must match a land *you* tapped,
    /// so the source filter (`valid_card`) carries `ControllerRef::You`.
    ///
    /// This test deliberately leaves `valid_target = None` so `valid_card` is
    /// the *sole* gate — isolating the issue #465 fix. (The real card also
    /// parses `valid_target = Controller`; that field independently gates
    /// `valid_player_matches`, so including it would shadow `valid_card` and
    /// the mutation-check below would not discriminate. `valid_target` does NOT
    /// route the `TriggerEventManaType` mana — `effects/mana.rs` routes that to
    /// the `TappedForMana` event's `player_id` directly — so omitting it does
    /// not change the positive-case mana total.)
    ///
    /// Mutation-check: replacing `valid_card`'s `TypedFilter::land()
    /// .controller(ControllerRef::You)` with the pre-fix unscoped
    /// `TypedFilter::land()` makes the negative assertion FAIL — the opponent's
    /// tap fires Vorinclex's triggered mana ability, adding a second green to
    /// the opponent's pool (1 base + 1 Vorinclex = 2 instead of 1). Verified.
    #[test]
    fn vorinclex_you_tap_trigger_ignores_opponent_land_tap() {
        let mut state = GameState::new_two_player(42);
        let me = PlayerId(0);
        let opp = PlayerId(1);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = me;
        state.priority_player = me;
        state.waiting_for = WaitingFor::Priority { player: me };

        // Vorinclex under PlayerId(0)'s control, with the controller-scoped
        // `valid_card` produced by the issue #465 parser fix. `valid_target` is
        // intentionally omitted (see the test's doc comment) so `valid_card` is
        // the sole gate.
        let vorinclex = create_object(
            &mut state,
            CardId(8600),
            me,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::You),
                    )),
            );

        // A Forest controlled by the opponent.
        let opp_forest = create_object(
            &mut state,
            CardId(8601),
            opp,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&opp_forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        // A Forest controlled by Vorinclex's controller.
        let my_forest = create_object(
            &mut state,
            CardId(8602),
            me,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&my_forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        Arc::make_mut(&mut state.objects.get_mut(&my_forest).unwrap().abilities).push(
            make_mana_ability(ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            }),
        );

        // Negative case: opponent taps their Forest for mana. Vorinclex's
        // "you tap" trigger must NOT fire. Vorinclex's trigger is a triggered
        // mana ability (Effect::Mana) — when it fires, `TriggerEventManaType`
        // mana is added to the `TappedForMana` event's `player_id`, i.e. the
        // *tapping* player (CR 106.3 + CR 109.5). So the discriminating signal
        // is the OPPONENT's green pool: pre-fix (unscoped `valid_card`) the
        // opponent would receive 1 base + 1 from Vorinclex = 2; post-fix the
        // trigger does not fire, so the opponent receives 1 base only.
        // CR 605.3a: a mana ability may be activated whenever a player has
        // priority; hand priority to the opponent so they may activate.
        state.priority_player = opp;
        state.waiting_for = WaitingFor::Priority { player: opp };
        crate::game::engine::apply(
            &mut state,
            opp,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: opp_forest,
                ability_index: 0,
            },
        )
        .expect("opponent's Forest {T} ability should activate");
        assert_eq!(
            state.players[1].mana_pool.count_color(ManaType::Green),
            1,
            "opponent tapping a land yields only its 1 base green — Vorinclex's \
             'you tap' trigger must not fire on an opponent's land tap"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            0,
            "Vorinclex's controller gains no mana from an opponent's land tap"
        );

        // Positive case: Vorinclex's controller taps their own Forest.
        // 1 base green + 1 from Vorinclex's trigger = 2.
        state.priority_player = me;
        state.waiting_for = WaitingFor::Priority { player: me };
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: my_forest,
                ability_index: 0,
            },
        )
        .expect("controller's Forest {T} ability should activate");
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            2,
            "1 base + 1 Vorinclex = 2 when the controller taps their own land"
        );
    }

    /// Regression: effect-produced (non-tap) mana does not fire Vorinclex.
    /// CR 106.12a — only a `{T}`-cost mana ability is "tapped for mana"; an
    /// `Effect::Mana` resolution from a spell/non-mana ability emits no
    /// `TappedForMana` event.
    #[test]
    fn effect_produced_mana_does_not_fire_vorinclex() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        let vorinclex = create_object(
            &mut state,
            CardId(8500),
            player,
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&vorinclex)
            .unwrap()
            .trigger_definitions
            .push(
                crate::types::ability::TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Any)
                    .valid_target(TargetFilter::Controller),
            );

        // Effect-produced mana: `produce_mana` with `tapped_for_mana = false`.
        let source = create_object(
            &mut state,
            CardId(8501),
            player,
            "Mana Spell".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        mana_payment::produce_mana(
            &mut state,
            source,
            ManaType::Green,
            player,
            false,
            &mut events,
        );
        crate::game::triggers::process_triggers(&mut state, &events);

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            1,
            "effect-produced mana adds 1, Vorinclex does not fire (no TappedForMana)"
        );
    }

    #[test]
    fn pay_mana_ability_mana_rejects_unlisted_payment() {
        // Handler rejects a payment vector not present in `options`.
        let mut state = GameState::new_two_player(42);
        let (ruins, _ability) = setup_sunken_ruins(&mut state);
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let options = vec![vec![ManaType::Blue], vec![ManaType::Black]];
        let mut events = Vec::new();
        let result = handle_pay_mana_ability_mana(
            &mut state,
            &options,
            &pending,
            &[ManaType::Red],
            &mut events,
        );
        assert!(result.is_err());
    }

    // Regression: Gemstone Mine's `{T}, Remove a mining counter` ability could
    // not activate because the replacement parser emitted "MINING" (uppercase)
    // while the cost parser emitted "mining" (lowercase), and
    // `CounterType::Generic` used the raw string as the HashMap key, so the
    // payability check found 0 counters and blocked activation.
    //
    // This fixture exercises the full depletion-land pattern — composite
    // Tap+RemoveCounter cost — so that any regression in counter-type
    // normalisation surfaces immediately. The negative test below
    // (`gemstone_mine_unpayable_without_counters`) locks in the *other*
    // direction: the payability gate must remain coupled to the canonical
    // key, so that counters going to zero correctly blocks activation
    // rather than the gate silently passing on a stale uppercase key.
    fn make_gemstone_mine(state: &mut GameState, player: PlayerId) -> ObjectId {
        let land = create_object(
            state,
            CardId(8000),
            player,
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        // Seed with three mining counters via `parse_counter_type` to mirror
        // the actual effect pipeline (the ETB replacement emits "MINING" in
        // uppercase; `parse_counter_type` must normalise it to the same key
        // that the cost-payability check uses, which parses "mining" lowercase).
        // Using the uppercase spelling here exercises the normalisation fix
        // end-to-end: if the fix were reverted, the HashMap key would be
        // `Generic("MINING")` while the lookup key would be `Generic("mining")`
        // and the payability check would return false.
        let mining_key = crate::types::counter::parse_counter_type("MINING");
        obj.counters.insert(mining_key, 3);

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::RemoveCounter {
                    count: 1,
                    counter_type: CounterMatch::OfType(CounterType::Generic("mining".to_string())),
                    target: None,
                    selection: crate::types::ability::CounterCostSelection::SingleObject,
                },
            ],
        });
        Arc::make_mut(&mut obj.abilities).push(ability);
        land
    }

    #[test]
    fn gemstone_mine_activates_and_consumes_counter() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine(&mut state, player);

        // Sanity: payability gate must pass while counters are present.
        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        assert!(
            can_activate_mana_ability_now(&state, player, land, 0, &def),
            "Gemstone Mine must be activatable while it has mining counters"
        );

        // Activate: produce green mana with the single-color override.
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            land,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Green)),
        )
        .expect("Gemstone Mine activation must not fail with counters present");

        // One green mana must land in the pool.
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Green),
            1,
            "Gemstone Mine must add one green mana on activation"
        );
        // The land must be tapped.
        assert!(
            state.objects.get(&land).unwrap().tapped,
            "Gemstone Mine must be tapped after activation"
        );
        // One mining counter must have been removed (3 → 2).
        let remaining = state
            .objects
            .get(&land)
            .unwrap()
            .counters
            .get(&CounterType::Generic("mining".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            remaining, 2,
            "Gemstone Mine must lose one mining counter per activation"
        );
    }

    #[test]
    fn any_number_counter_mana_ability_prompts_and_removes_chosen_count() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = create_object(
            &mut state,
            CardId(8002),
            player,
            "Storage Land".to_string(),
            Zone::Battlefield,
        );
        let storage = CounterType::Generic("storage".to_string());
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.counters.insert(storage.clone(), 3);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: Vec::new(),
                        grants: Vec::new(),
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::RemoveCounter {
                            count: REMOVE_COUNTER_COST_ANY_NUMBER,
                            counter_type: CounterMatch::OfType(storage.clone()),
                            target: None,
                            selection: crate::types::ability::CounterCostSelection::SingleObject,
                        },
                    ],
                }),
            );
        }

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::ActivateAbility {
                source_id: land,
                ability_index: 0,
            },
        )
        .expect("storage mana ability should prompt for a counter count");

        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                resource, min, max, ..
            } => {
                assert_eq!(*resource, PayableResource::Counters);
                assert_eq!(*min, 0);
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::SubmitPayAmount { amount: 1 },
        )
        .expect("chosen counter count should resume mana production");

        assert_eq!(
            state.objects[&land]
                .counters
                .get(&storage)
                .copied()
                .unwrap_or(0),
            2,
            "any-number counter mana costs must remove only the chosen count"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn gemstone_mine_unpayable_without_counters() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_gemstone_mine(&mut state, player);

        // Drain all counters so the cost cannot be paid.
        let mining_key = crate::types::counter::parse_counter_type("MINING");
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .counters
            .insert(mining_key, 0);

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();
        assert!(
            !can_activate_mana_ability_now(&state, player, land, 0, &def),
            "Gemstone Mine must not be activatable when it has no mining counters"
        );
    }

    #[test]
    fn cabal_coffers_pays_generic_taps_and_counts_swamps() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let coffers = create_object(
            &mut state,
            CardId(9001),
            player,
            "Cabal Coffers".to_string(),
            Zone::Battlefield,
        );
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Subtype("Swamp".to_string()))
                                    .controller(ControllerRef::You),
                            ),
                        },
                    },
                    color_options: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
                AbilityCost::Tap,
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&coffers).unwrap().abilities)
            .push(ability.clone());

        for idx in 0..3 {
            let swamp = create_object(
                &mut state,
                CardId(9010 + idx),
                player,
                "Swamp".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&swamp).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Swamp".to_string());
        }
        seed_pool_with(&mut state, player, ManaType::Black, 2);

        assert!(
            can_activate_mana_ability_now(&state, player, coffers, 0, &ability),
            "Cabal Coffers must be activatable with two mana available"
        );

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, coffers, player, &ability, &mut events, None)
            .expect("Cabal Coffers activation must pay {2}, tap, and add mana");

        assert!(state.objects.get(&coffers).unwrap().tapped);
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Black),
            3
        );
    }

    /// CR 602.2a + CR 605.1a: An activated ability's controller is the
    /// player who activated it (not the owner of the source permanent).
    /// A `Controller`-scoped damage sub-effect therefore resolves against
    /// the activator — opponent-controlled painlands damage the opponent,
    /// not the original owner.
    #[test]
    fn pain_land_damage_routes_to_activator_not_original_owner() {
        let mut state = GameState::new_two_player(42);
        let brushland = create_object(
            &mut state,
            CardId(1001),
            PlayerId(1), // opponent controls it
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&brushland).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(1),
            source_id: brushland,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let prompt = ManaChoicePrompt::SingleColor {
            options: vec![ManaType::Green, ManaType::White],
        };
        let mut events = Vec::new();

        let result = handle_choose_mana_color(
            &mut state,
            &pending,
            &prompt,
            ManaChoice::SingleColor(ManaType::Green),
            &mut events,
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(
            state.players[1].life, 19,
            "activator (PlayerId(1)) should take 1 damage"
        );
        assert_eq!(
            state.players[0].life, 20,
            "non-activator (PlayerId(0)) should be unharmed"
        );
    }

    /// A 2-damage painland variant (Ancient Tomb shape) must route through
    /// the same sub-ability continuation path as the 1-damage case — the
    /// handler is parameterized over `amount`, not hardcoded.
    #[test]
    fn two_damage_painland_variant_deals_full_amount() {
        let mut state = GameState::new_two_player(42);
        let tomb = create_object(
            &mut state,
            CardId(1002),
            PlayerId(0),
            "Ancient Tomb".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&tomb).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                    damage_source: None,
                    excess: None,
                },
            )),
        );

        let ability = state.objects[&tomb].abilities[0].clone();
        let mut events = Vec::new();
        let result = activate_mana_ability(
            &mut state,
            tomb,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2
        );
        assert_eq!(
            state.players[0].life, 18,
            "Ancient Tomb should deal 2 damage to its controller"
        );
    }

    // ---------------------------------------------------------------
    // CR 605.3b + CR 605.1a: Painland-style self-damage sub-abilities
    // resolve inline with the mana ability.
    // ---------------------------------------------------------------

    fn make_painland(state: &mut GameState, player: PlayerId, color: ManaColor) -> ObjectId {
        let land = create_object(
            state,
            CardId(7000),
            player,
            "Painland".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let sub = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
                excess: None,
            },
        );

        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![color],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        ability.sub_ability = Some(Box::new(sub));
        Arc::make_mut(&mut obj.abilities).push(ability);
        land
    }

    #[test]
    fn painland_deals_one_damage_when_tapped_for_color() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_painland(&mut state, player, ManaColor::White);
        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        let starting_life = state.players[player.0 as usize].life;
        let mut events = Vec::new();
        resolve_mana_ability(&mut state, land, player, &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[player.0 as usize].life,
            starting_life - 1,
            "Painland must deal 1 damage to its controller"
        );
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::White),
            1,
            "Painland must still produce the colored mana"
        );
        assert!(
            state.objects.get(&land).unwrap().tapped,
            "Painland must tap"
        );
    }

    #[test]
    fn painland_kills_controller_at_one_life_via_sba_trigger() {
        // Activating the colored mana at 1 life drops the controller to 0.
        // The life-drop event must be emitted — SBAs triggered on the next
        // engine pass will eliminate the player.
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let land = make_painland(&mut state, player, ManaColor::White);
        state.players[player.0 as usize].life = 1;

        let def = state
            .objects
            .get(&land)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        let mut events = Vec::new();
        resolve_mana_ability(&mut state, land, player, &def, &mut events, None).unwrap();

        assert_eq!(
            state.players[player.0 as usize].life, 0,
            "Controller must hit 0 life after the painland damage"
        );
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::White),
            1,
            "Mana production must still occur"
        );
    }

    // ---------------------------------------------------------------------
    // CR 117.1 + CR 202.3: Cost-paid object mana value (Food Chain class)
    // ---------------------------------------------------------------------

    /// Build a Food Chain mana ability:
    /// "Exile a creature you control: Add X mana of any one color, where
    ///  X is 1 plus the exiled creature's mana value. Spend this mana only
    ///  to cast creature spells."
    fn make_food_chain_ability() -> AbilityDefinition {
        use crate::types::ability::{
            ManaSpendRestriction, ObjectScope, QuantityRef, TargetFilter as TF, TypedFilter,
        };
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: ObjectScope::CostPaidObject,
                            },
                        }),
                        offset: 1,
                    },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![ManaSpendRestriction::SpellType("Creature".to_string())],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(TF::Typed(
                TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
            )),
        })
    }

    fn make_phyrexian_altar_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature()),
            1,
        )))
    }

    fn make_titans_nest_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }]),
            )),
        })
    }

    /// Helper: spawn `name` on the battlefield with a printed mana cost
    /// and the Creature core type.
    fn spawn_creature_with_cost(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        cost: ManaCost,
    ) -> ObjectId {
        use crate::types::card_type::{CardType, CoreType};
        let id = create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.mana_cost = cost;
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }
        id
    }

    #[test]
    fn phyrexian_altar_prompts_for_controlled_creature_then_adds_mana() {
        let mut state = GameState::new_two_player(42);
        let altar = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Phyrexian Altar".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        Arc::make_mut(&mut state.objects.get_mut(&altar).unwrap().abilities).push(ability.clone());

        let creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(0),
            "Grizzly Bears",
            ManaCost::generic(2),
        );
        let opponent_creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(1),
            "Runeclaw Bear",
            ManaCost::generic(2),
        );

        assert!(
            can_activate_mana_ability_now(&state, PlayerId(0), altar, 0, &ability),
            "Phyrexian Altar must be activatable when its controller has a creature to sacrifice"
        );

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            altar,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Black)),
        )
        .expect("activation should surface the sacrifice choice");

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::Sacrifice,
                count,
                choices: permanents,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![creature]);
                assert!(!permanents.contains(&opponent_creature));
                pending_mana_ability
            }
            other => panic!("expected PayCost Sacrifice (mana ability), got {other:?}"),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("sacrifice choice should resolve the mana ability");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&creature).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn titans_nest_exiles_own_graveyard_card_for_colorless_mana() {
        let mut state = GameState::new_two_player(42);
        let nest = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Titans' Nest".to_string(),
            Zone::Battlefield,
        );
        let ability = make_titans_nest_ability();
        Arc::make_mut(&mut state.objects.get_mut(&nest).unwrap().abilities).push(ability.clone());

        let own_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First graveyard card".to_string(),
            Zone::Graveyard,
        );
        let own_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second graveyard card".to_string(),
            Zone::Graveyard,
        );
        let own_stolen = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Stolen graveyard card".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&own_stolen).unwrap().controller = PlayerId(1);
        let opponent_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opponent graveyard card".to_string(),
            Zone::Graveyard,
        );

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            nest,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .expect("Titans' Nest should ask which graveyard card pays the cost");

        let pending = match waiting {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromManaZone { zone },
                count,
                choices: cards,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(zone, Zone::Graveyard);
                assert!(cards.contains(&own_a));
                assert!(cards.contains(&own_b));
                assert!(cards.contains(&own_stolen));
                assert!(!cards.contains(&opponent_card));
                pending_mana_ability
            }
            other => panic!("expected PayCost ExileFromManaZone (mana ability), got {other:?}"),
        };

        let result = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[own_a, own_b],
            &pending,
            &[own_a],
            &mut events,
        )
        .expect("exile choice should resolve the mana ability");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&own_a).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&own_b).unwrap().zone, Zone::Graveyard);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
    }

    #[test]
    fn exile_for_mana_ability_rejects_duplicate_selected_cards() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Two-card Exile Source".to_string(),
            Zone::Battlefield,
        );
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First graveyard card".to_string(),
            Zone::Graveyard,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second graveyard card".to_string(),
            Zone::Graveyard,
        );
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };

        let result = handle_exile_for_mana_ability(
            &mut state,
            2,
            &[first, second],
            &pending,
            &[first, first],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert_eq!(state.objects.get(&first).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.objects.get(&second).unwrap().zone, Zone::Graveyard);
    }

    #[test]
    fn sacrifice_mana_cost_rejects_prohibited_selected_permanent() {
        let mut state = GameState::new_two_player(42);
        let altar = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Phyrexian Altar".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        Arc::make_mut(&mut state.objects.get_mut(&altar).unwrap().abilities).push(ability);

        let creature = spawn_creature_with_cost(
            &mut state,
            PlayerId(0),
            "Grizzly Bears",
            ManaCost::generic(2),
        );
        let lock = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Cost Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::Sacrifice {
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
            }));

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: altar,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Black)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Battlefield
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn sacrifice_creature_mana_cost_can_use_creature_source_itself() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Thermopod".to_string(),
            Zone::Battlefield,
        );
        let ability = make_phyrexian_altar_ability();
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(ability.clone());

        let mut events = Vec::new();
        let waiting = activate_mana_ability(
            &mut state,
            source,
            PlayerId(0),
            0,
            &ability,
            &mut events,
            ManaAbilityResume::Priority,
            Some(ProductionOverride::SingleColor(ManaType::Red)),
        )
        .expect("creature source should be eligible to pay its own sacrifice-a-creature cost");

        let pending = match waiting {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                count,
                choices: permanents,
                resume:
                    CostResume::ManaAbility {
                        mana_ability: pending_mana_ability,
                    },
                ..
            } => {
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![source]);
                pending_mana_ability
            }
            other => panic!("expected PayCost Sacrifice (mana ability), got {other:?}"),
        };

        let result = handle_sacrifice_for_mana_ability(
            &mut state,
            1,
            &[source],
            &pending,
            &[source],
            &mut events,
        )
        .expect("source creature should be sacrificed and produce mana");

        assert!(matches!(
            result,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.objects.get(&source).unwrap().zone, Zone::Graveyard);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
    }

    /// (a) Sacrificing a 3-mana-value creature gives 4 mana from Food Chain.
    #[test]
    fn food_chain_exiles_three_mana_value_creature_produces_four_mana() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        // Stash the food-chain ability so the dispatch can find it by index.
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        // 3-MV creature: cost {2}{G}.
        let three_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        };
        let creature =
            spawn_creature_with_cost(&mut state, PlayerId(0), "Grizzly Bears", three_cost);

        // Player picks the creature to exile via the resume handler.
        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the chosen creature");

        // 1 plus mana value of {2}{G} = 4 mana.
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Green),
            4,
            "Food Chain must produce 4 green mana for a 3-MV exiled creature"
        );
        // Creature is now in exile.
        assert_eq!(
            state.objects.get(&creature).unwrap().zone,
            Zone::Exile,
            "Exiled creature must be in the exile zone after cost is paid"
        );
    }

    /// (b) Exiling a 0-mana-value creature gives 1 mana (offset = 1).
    #[test]
    fn food_chain_exiles_zero_mana_value_creature_produces_one_mana() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        // 0-MV creature (Memnite-style): no shards, no generic.
        let zero_cost = ManaCost::Cost {
            shards: vec![],
            generic: 0,
        };
        let creature = spawn_creature_with_cost(&mut state, PlayerId(0), "Memnite", zero_cost);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Red)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the 0-MV creature");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Red),
            1,
            "Food Chain must produce 1 red mana for a 0-MV exiled creature"
        );
    }

    /// (c) Burnt-Offering / Metamorphosis class — an `AbilityResolution`
    /// stamped with a captured mana value resolves
    /// `ObjectManaValue { CostPaidObject }` to that value at production time.
    #[test]
    fn cost_paid_object_resolves_via_resolved_ability_field() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::{CostPaidObjectSnapshot, ObjectScope, QuantityRef};

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    },
                    color_options: vec![ManaColor::Black, ManaColor::Red],
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let mut paid = crate::game::game_object::GameObject::new(
            ObjectId(99),
            CardId(99),
            PlayerId(0),
            "Paid Creature".to_string(),
            Zone::Battlefield,
        );
        paid.mana_cost = crate::types::mana::ManaCost::generic(5);
        ability.set_cost_paid_object_recursive(CostPaidObjectSnapshot {
            object_id: paid.id,
            lki: paid.snapshot_for_mana_spent(),
        });

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 5,
            "CostPaidObject must resolve to the captured mana value"
        );
    }

    /// Resolver returns 0 when no cost-paid object snapshot is in scope —
    /// regression guard that avoids spurious mana production for unrelated
    /// abilities.
    #[test]
    fn cost_paid_object_returns_zero_without_snapshot() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::{ObjectScope, QuantityRef};

        let state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        // No `set_cost_paid_object_recursive` — field stays None.

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            },
            &ability,
        );
        assert_eq!(
            resolved, 0,
            "CostPaidObject must return 0 when no snapshot was captured"
        );
    }

    /// Food Chain mana carries `ManaSpendRestriction::SpellType("Creature")`
    /// so the produced mana cannot pay non-creature spell costs.
    #[test]
    fn food_chain_mana_is_creature_spell_only() {
        let mut state = GameState::new_two_player(42);
        let chain = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Food Chain".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&chain).unwrap().abilities)
            .push(make_food_chain_ability());

        let three_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        };
        let creature =
            spawn_creature_with_cost(&mut state, PlayerId(0), "Grizzly Bears", three_cost);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: chain,
            ability_index: 0,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_for_mana_ability(
            &mut state,
            1,
            &[creature],
            &pending,
            &[creature],
            &mut events,
        )
        .expect("food chain exile handler must accept the chosen creature");

        // Every produced unit must carry the SpellType("Creature") restriction.
        let pool = &state.players[0].mana_pool;
        assert_eq!(pool.total(), 4);
        for unit in &pool.mana {
            assert_eq!(
                unit.restrictions,
                vec![crate::types::mana::ManaRestriction::OnlyForSpellType(
                    "Creature".to_string()
                )],
                "Food Chain mana must carry the Creature spell-type restriction"
            );
        }
    }

    /// CR 602.5: the mana-ability executor must reject submissions that violate
    /// an active `CantActivateDuring` static, not only the legal-action filter.
    /// Discriminating end-to-end test against the City of Solitude class: a
    /// hostile/buggy client submitting `activate_mana_ability` directly must
    /// receive `EngineError::ActionNotAllowed`.
    #[test]
    fn city_of_solitude_rejects_mana_ability_at_executor() {
        use crate::types::statics::{ActivationExemption, CastingProhibitionCondition};

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        state.active_player = p0;
        state.phase = Phase::PreCombatMain;

        // P0 controls a City of Solitude analogue (AllPlayers / NotDuringAffectedPlayersTurn
        // / exemption: None — per the 2009-10-01 ruling).
        let prohibitor = create_object(
            &mut state,
            CardId(1),
            p0,
            "City of Solitude".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohibitor)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantActivateDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::NotDuringAffectedPlayersTurn,
                exemption: ActivationExemption::None,
            }));

        // P1 controls a Forest-like permanent with a tap-for-green mana ability.
        let forest = create_object(
            &mut state,
            CardId(2),
            p1,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let mana_ability = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities)
            .push(mana_ability.clone());

        // On P0's turn, P1 attempts to activate the mana ability directly through
        // the executor. The CR 602.5 gate at the top of `activate_mana_ability`
        // must reject before any cost is paid or mana is produced.
        let mut events = Vec::new();
        let err = activate_mana_ability(
            &mut state,
            forest,
            p1,
            0,
            &mana_ability,
            &mut events,
            ManaAbilityResume::Priority,
            None,
        )
        .expect_err("City of Solitude must reject P1's mana ability at the executor on P0's turn");
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "expected ActionNotAllowed, got {err:?}"
        );
        // No mana was produced and the ability source was not tapped.
        assert_eq!(state.players[1].mana_pool.total(), 0);
        assert!(!state.objects.get(&forest).unwrap().tapped);
        assert!(events.is_empty());
    }

    /// Perf-gate correctness (`ManaActivationGates`, Fix A): when a
    /// CantActivateDuring (City of Solitude class) static is present the hoisted
    /// gate flag is set, so the per-source readiness scan must still run and
    /// report the mana ability UNAVAILABLE. Exercises the `gate=true` arm of
    /// `mana_ability_ready_without_simulation_gated` that the board-global mana
    /// display sweep depends on — without this the fast tests only cover the
    /// `gate=false` (no-prohibition) arm.
    #[test]
    fn can_activate_mana_ability_now_respects_cant_activate_during_via_gate() {
        use crate::types::statics::{ActivationExemption, CastingProhibitionCondition};

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        let p1 = PlayerId(1);
        state.active_player = p0; // NOT p1's turn
        state.phase = Phase::PreCombatMain;

        // P0 controls a City of Solitude analogue (AllPlayers /
        // NotDuringAffectedPlayersTurn / exemption: None).
        let prohibitor = create_object(
            &mut state,
            CardId(1),
            p0,
            "City of Solitude".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohibitor)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantActivateDuring {
                who: ProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::NotDuringAffectedPlayersTurn,
                exemption: ActivationExemption::None,
            }));

        let forest = create_object(
            &mut state,
            CardId(2),
            p1,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let mana_ability = make_mana_ability(ManaProduction::Fixed {
            colors: vec![ManaColor::Green],
            contribution: ManaContribution::Base,
        });
        Arc::make_mut(&mut state.objects.get_mut(&forest).unwrap().abilities)
            .push(mana_ability.clone());

        // gate=true arm: prohibition exists → scan runs → unavailable on P0's turn.
        assert!(
            !can_activate_mana_ability_now(&state, p1, forest, 0, &mana_ability),
            "City of Solitude must make P1's mana ability unavailable on P0's turn (gate=true)"
        );

        // Control: on the affected player's own turn the prohibition lifts.
        state.active_player = p1;
        assert!(
            can_activate_mana_ability_now(&state, p1, forest, 0, &mana_ability),
            "on the affected player's own turn the mana ability is available again"
        );
    }

    // ---------------------------------------------------------------
    // Standalone RemoveCounter mana ability (Pentad Prism class)
    // ---------------------------------------------------------------

    /// Pentad Prism: `Remove a charge counter from ~: Add one mana of any color.`
    /// The cost is a bare `RemoveCounter` (NOT inside `Composite`).
    fn make_pentad_prism(state: &mut GameState, player: PlayerId) -> ObjectId {
        let prism = create_object(
            state,
            CardId(8100),
            player,
            "Pentad Prism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&prism).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        let charge_key = crate::types::counter::parse_counter_type("charge");
        obj.counters.insert(charge_key, 2);

        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![
                        ManaColor::White,
                        ManaColor::Blue,
                        ManaColor::Black,
                        ManaColor::Red,
                        ManaColor::Green,
                    ],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::RemoveCounter {
            count: 1,
            counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
            target: None,
            selection: crate::types::ability::CounterCostSelection::SingleObject,
        });
        Arc::make_mut(&mut obj.abilities).push(ability);
        prism
    }

    #[test]
    fn standalone_remove_counter_mana_ability_activates() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let prism = make_pentad_prism(&mut state, player);

        let def = state
            .objects
            .get(&prism)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        // Readiness must pass — the prism has charge counters.
        assert!(
            can_activate_mana_ability_now(&state, player, prism, 0, &def),
            "Pentad Prism must be activatable with charge counters"
        );

        // Activate: produce blue mana.
        let mut events = Vec::new();
        resolve_mana_ability(
            &mut state,
            prism,
            player,
            &def,
            &mut events,
            Some(ProductionOverride::SingleColor(ManaType::Blue)),
        )
        .expect("Standalone RemoveCounter mana ability must not fail");

        // One blue mana in pool.
        assert_eq!(
            state.players[player.0 as usize]
                .mana_pool
                .count_color(ManaType::Blue),
            1,
        );
        // One charge counter removed (2 → 1).
        let remaining = state
            .objects
            .get(&prism)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(remaining, 1);
        // Source is NOT tapped (no tap cost).
        assert!(
            !state.objects.get(&prism).unwrap().tapped,
            "Pentad Prism must not be tapped — cost is only RemoveCounter"
        );
    }

    #[test]
    fn standalone_remove_counter_mana_ability_unpayable_without_counters() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let prism = make_pentad_prism(&mut state, player);

        // Remove all charge counters.
        state.objects.get_mut(&prism).unwrap().counters.clear();

        let def = state
            .objects
            .get(&prism)
            .unwrap()
            .abilities
            .first()
            .cloned()
            .unwrap();

        // Readiness must fail — no counters to remove.
        assert!(
            !can_activate_mana_ability_now(&state, player, prism, 0, &def),
            "Pentad Prism must not be activatable without charge counters"
        );
    }
}
