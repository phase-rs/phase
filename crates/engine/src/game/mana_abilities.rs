use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ChoiceValue, CostPaidObjectSnapshot, Effect,
    ManaProduction, ResolvedAbility, TargetFilter,
};
#[cfg(test)]
use crate::types::counter::CounterMatch;
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    GameState, ManaAbilityResume, ManaChoice, ManaChoiceContext, ManaChoicePrompt,
    PendingManaAbility, ProductionOverride, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost, ManaPool, ManaType, PaymentContext};
#[cfg(test)]
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::mana::resolve_restrictions;
use super::engine::EngineError;
use super::filter::{matches_target_filter, FilterContext};
use super::life_costs::{self, PayLifeCostResult};
use super::mana_payment;
use super::mana_sources;
use super::mana_sources::{mana_color_to_type, mana_type_to_color};
use super::sacrifice;

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
    ability_def.multi_target.is_none() && target_attached.is_none()
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
/// from. CR 605.1b also admits "triggered from the activation/resolution of an
/// activated mana ability" in general, but mana abilities bypass the stack and
/// do not emit a distinguishable `AbilityActivated` event; widening (b) to that
/// axis requires first emitting such an event. No real card exercises the gap
/// today.
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
    // (b) CR 106.12a: triggered by a `{T}`-cost mana ability resolving and
    // producing mana. See the doc comment above for the deliberately-not-yet-
    // widened `AbilityActivated` axis.
    matches!(trigger_event, Some(GameEvent::TappedForMana { .. }))
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

/// CR 605.3b: Resolve a triggered mana ability inline (stack-skipped).
/// The ability's effect chain is executed immediately; mana additions land in the
/// controller's pool before any player could respond.
pub fn resolve_triggered_mana_ability_inline(
    state: &mut GameState,
    ability: &ResolvedAbility,
    trigger_event: Option<&GameEvent>,
    events: &mut Vec<GameEvent>,
) {
    let previous_trigger_event = state.current_trigger_event.clone();
    state.current_trigger_event = trigger_event.cloned();
    // Use the standard resolution entry so sub_ability chains resolve uniformly.
    let _ = super::effects::resolve_ability_chain(state, ability, events, 0);
    state.current_trigger_event = previous_trigger_event;
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
    // Pay the full ability cost (tap, sacrifice, etc.)
    pay_mana_ability_cost(state, source_id, player, &ability_def.cost, events)?;

    // CR 117.1 + CR 202.3: This non-interactive entry point is reachable only
    // when no cost-paid-object snapshot is needed (no battlefield exile
    // selection); pass `None`. The interactive Food Chain path threads its
    // captured value through `produce_mana_from_ability` directly.
    produce_mana_from_ability(
        state,
        source_id,
        player,
        ability_def,
        events,
        color_override,
        None,
    );
    Ok(())
}

/// Produce mana from a resolved mana ability without paying costs.
/// Shared by `resolve_mana_ability` (cost paid inline) and `handle_choose_mana_color`
/// (cost already paid during the `TapCreaturesForManaAbility` phase).
///
/// `cost_paid_object` carries the captured public characteristics of any
/// object exiled or sacrificed as part of cost payment so production counts can
/// resolve cost-paid-object refs (Food Chain / Burnt Offering class).
fn produce_mana_from_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
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
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) -> ResolvedAbility {
    let mut resolved =
        super::ability_utils::build_resolved_from_def(ability_def, source_id, player);
    if let Some(snapshot) = cost_paid_object {
        resolved.set_cost_paid_object_recursive(snapshot);
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
    super::restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    advance_mana_ability_activation(
        state,
        PendingManaAbility {
            player,
            source_id,
            ability_index,
            color_override,
            resume,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))?;

    produce_mana_from_ability(
        state,
        pending.source_id,
        pending.player,
        &ability_def,
        events,
        Some(override_value),
        pending.cost_paid_object.clone(),
    );
    complete_mana_ability_activation(
        state,
        pending.source_id,
        pending.ability_index,
        pending.player,
        events,
    );

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

/// CR 117.1 + CR 118.3 + CR 605.3b + CR 202.3: Complete the
/// exile-from-battlefield mana-ability cost selection (Food Chain class).
/// Captures the cost-paid object's mana value snapshot from the live
/// battlefield before the cost is paid, then resumes the activation flow.
pub fn handle_exile_from_battlefield_for_mana_ability(
    state: &mut GameState,
    count: usize,
    legal_permanents: &[ObjectId],
    pending: &PendingManaAbility,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must exile exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for mana ability exile cost".to_string(),
            ));
        }
    }

    // CR 117.1 + CR 400.7j + CR 608.2k: Capture the cost-paid object's public
    // characteristics before it leaves the battlefield.
    let captured = chosen.first().and_then(|id| {
        state.objects.get(id).map(|obj| CostPaidObjectSnapshot {
            object_id: *id,
            lki: obj.snapshot_for_mana_spent(),
        })
    });

    let mut updated = pending.clone();
    updated.chosen_exiled_battlefield = chosen.to_vec();
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

pub fn can_activate_mana_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability_def: &AbilityDefinition,
) -> bool {
    // CR 701.35a: Detained permanents' activated abilities can't be activated
    // (mana abilities are activated abilities).
    if state
        .objects
        .get(&source_id)
        .is_some_and(|obj| !obj.detained_by.is_empty())
    {
        return false;
    }
    // CR 605.3a + CR 601.2h: Mana abilities pay their cost at activation
    // time ("unpayable costs can't be paid"). Gate on pool affordability +
    // choice-of-object eligibility before simulating — this surfaces filter
    // lands as un-activatable when the player has no {W/U}-style mana
    // available.
    if let Some(cost) = &ability_def.cost {
        if !cost.is_payable_for_mana_ability(state, player, source_id) {
            return false;
        }
    }
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

fn advance_mana_ability_activation(
    state: &mut GameState,
    mut pending: PendingManaAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ability_def = state
        .objects
        .get(&pending.source_id)
        .and_then(|obj| obj.abilities.get(pending.ability_index))
        .cloned()
        .ok_or_else(|| EngineError::InvalidAction("Mana ability no longer exists".to_string()))?;

    if pending.chosen_discards.is_empty() {
        if let Some((count, cards)) =
            discard_cost_choice(state, pending.player, pending.source_id, &ability_def.cost)
        {
            if cards.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard for mana ability".to_string(),
                ));
            }
            return Ok(WaitingFor::DiscardForManaAbility {
                player: pending.player,
                count,
                cards,
                pending_mana_ability: Box::new(pending),
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
            return Ok(WaitingFor::TapCreaturesForManaAbility {
                player: pending.player,
                count,
                creatures,
                pending_mana_ability: Box::new(pending),
            });
        }
    }

    // CR 117.1 + CR 118.3: Non-self exile-from-battlefield as a mana ability cost
    // (Food Chain class). Surface the player choice before producing mana so the
    // cost-paid object's mana value can be captured at cost-payment time
    // (CR 202.3).
    if pending.chosen_exiled_battlefield.is_empty() {
        if let Some((count, permanents)) = exile_from_battlefield_cost_choice(
            state,
            pending.player,
            pending.source_id,
            &ability_def.cost,
        ) {
            if permanents.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to exile for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::ExileFromBattlefieldForManaAbility {
                player: pending.player,
                count,
                permanents,
                pending_mana_ability: Box::new(pending),
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
            if permanents.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to sacrifice for mana ability cost".to_string(),
                ));
            }
            return Ok(WaitingFor::SacrificeForManaAbility {
                player: pending.player,
                count,
                permanents,
                pending_mana_ability: Box::new(pending),
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
            let pool = &state.players[pending.player.0 as usize].mana_pool;
            let plans = enumerate_hybrid_payment_plans(pool, sub_cost);
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

    if pending.color_override.is_none() {
        let resolved_for_prompt = resolved_mana_ability_for_current_state(
            state,
            pending.source_id,
            pending.player,
            &ability_def,
            pending.cost_paid_object.clone(),
        );
        if let Some(choice) = mana_choice_prompt(
            &resolved_for_prompt.effect,
            state,
            pending.source_id,
            Some(&resolved_for_prompt),
        ) {
            let events_before = events.len();
            pay_mana_ability_cost_with_choices(
                state,
                pending.source_id,
                pending.player,
                &ability_def.cost,
                events,
                &mut pending.chosen_tappers.iter().copied(),
                &mut pending.chosen_discards.iter().copied(),
                &mut pending.chosen_exiled_battlefield.iter().copied(),
                &mut pending.chosen_sacrificed_battlefield.iter().copied(),
                pending.chosen_mana_payment.as_deref(),
            )?;
            // CR 603.2a + CR 603.2g + CR 605.3b: Cost-payment events (Tap,
            // Sacrifice, etc.) generated during a mana ability's cost step
            // trigger external abilities normally — CR 603.2a allows triggers
            // to fire even when cost payment is in flight, and CR 603.2g
            // demands that any event that actually occurs trigger its
            // observers. Mana abilities (CR 605.3b) resolve in two halves
            // around an interactive `WaitingFor::ChooseManaColor` prompt:
            // this branch pays the cost and returns the prompt without
            // flowing back through `run_post_action_pipeline`, which is the
            // engine's normal trigger scan site. Scan inline here so
            // cost-payment triggers register before the prompt; otherwise
            // the events are stranded and never fire any observers (Crime
            // Novelist, Mayhem Devil, Cruel Celebrant, Korvold, Syr Ginger,
            // …).
            if events.len() > events_before {
                let cost_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &cost_events);
            }
            // CR 605.3a: When the prompt is a single shared color choice and the
            // cost resolves with no further player input, surface this source's
            // identical activatable twins so `GameAction::ChooseManaColor` can
            // bulk-activate them with the chosen color in one round-trip
            // (the player's 20 Treasures, etc.).
            if matches!(choice, ManaChoicePrompt::SingleColor { .. })
                && cost_resolves_without_choice(&ability_def.cost)
            {
                pending.batch_siblings =
                    batch_eligible_siblings(state, pending.player, pending.source_id, &ability_def);
            }
            return Ok(WaitingFor::ChooseManaColor {
                player: pending.player,
                choice,
                context: ManaChoiceContext::ManaAbility(Box::new(pending)),
            });
        }
    }

    resolve_mana_ability_with_selected_choices(
        state,
        pending.source_id,
        pending.player,
        &ability_def,
        events,
        pending.color_override.clone(),
        &pending.chosen_tappers,
        &pending.chosen_discards,
        &pending.chosen_exiled_battlefield,
        &pending.chosen_sacrificed_battlefield,
        pending.chosen_mana_payment.as_deref(),
        pending.cost_paid_object,
    )?;
    complete_mana_ability_activation(
        state,
        pending.source_id,
        pending.ability_index,
        pending.player,
        events,
    );
    Ok(resume_waiting_for(pending.player, pending.resume))
}

/// Pay the full cost of a mana ability. This is the single authority for mana ability
/// cost resolution — callers dispatch activation, they never inspect individual cost
/// components. Handles `Tap`, `Composite { Tap, Sacrifice }`, and future cost variants.
fn pay_mana_ability_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        cost,
        events,
        &mut std::iter::empty(),
        &mut std::iter::empty(),
        &mut std::iter::empty(),
        &mut std::iter::empty(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_mana_ability_with_selected_choices(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    tapped_creatures: &[ObjectId],
    discarded_cards: &[ObjectId],
    exiled_battlefield: &[ObjectId],
    sacrificed_battlefield: &[ObjectId],
    chosen_hybrid_payment: Option<&[ManaType]>,
    cost_paid_object: Option<CostPaidObjectSnapshot>,
) -> Result<(), EngineError> {
    let mut chosen = tapped_creatures.iter().copied();
    let mut discarded = discarded_cards.iter().copied();
    let mut exiled = exiled_battlefield.iter().copied();
    let mut sacrificed = sacrificed_battlefield.iter().copied();
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        &ability_def.cost,
        events,
        &mut chosen,
        &mut discarded,
        &mut exiled,
        &mut sacrificed,
        chosen_hybrid_payment,
    )?;
    if chosen.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many creatures selected for mana ability cost".to_string(),
        ));
    }
    if exiled.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many permanents selected for mana ability exile cost".to_string(),
        ));
    }
    if discarded.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability cost".to_string(),
        ));
    }
    if sacrificed.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many permanents selected for mana ability sacrifice cost".to_string(),
        ));
    }

    // CR 117.1 + CR 202.3: Build a transient `ResolvedAbility` carrying the
    // cost-paid object snapshot so production-count resolution sees it
    // (Food Chain class).
    let resolved_for_quantity = resolved_mana_ability_for_current_state(
        state,
        source_id,
        player,
        ability_def,
        cost_paid_object,
    );

    // CR 106.6: Thread restrictions, grants, and expiry through the
    // selected-choices path too — otherwise color-picked or hybrid-paid mana
    // abilities would still emit unrestricted mana.
    let (produced_mana, restrictions, grants, expiry) = match &resolved_for_quantity.effect {
        Effect::Mana {
            produced,
            restrictions,
            grants,
            expiry,
            target: None,
        } => {
            let mana = match color_override {
                Some(ProductionOverride::Combination(types)) => types,
                Some(ProductionOverride::SingleColor(color)) => {
                    resolve_single_color_override(state, produced, &resolved_for_quantity, color)
                }
                None => super::effects::mana::resolve_mana_types_for_ability(
                    produced,
                    &*state,
                    &resolved_for_quantity,
                ),
            };
            let concrete = resolve_restrictions(restrictions, &*state, source_id);
            (mana, concrete, grants.clone(), *expiry)
        }
        _ => (Vec::new(), Vec::new(), Vec::new(), None),
    };

    // CR 106.12: a permanent is "tapped for mana" when the activated mana
    // ability's cost includes the `{T}` symbol.
    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for &mana_type in &produced_mana {
        mana_payment::produce_mana_with_attributes(
            state,
            source_id,
            mana_type,
            player,
            tapped,
            &restrictions,
            &grants,
            expiry,
            events,
        );
    }

    // CR 106.12a: emit one `TappedForMana` per resolution of a `{T}`-cost mana
    // ability that produces mana, so the `TapsForMana` matcher fires exactly
    // once. Mirrors `produce_mana_from_ability`; this is the selected-choices /
    // no-prompt resolution path.
    if tapped && !produced_mana.is_empty() {
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id,
            produced: produced_mana,
            tap_state: ManaTapState::from_tap(tapped),
        });
    }

    // CR 605.3b + CR 605.1a: Resolve the sub-ability chain inline (painlands'
    // "deals 1 damage to you", Llanowar Wastes-style self-damage, etc.).
    resolve_mana_ability_sub_chain(state, &resolved_for_quantity, events);

    Ok(())
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

#[allow(clippy::too_many_arguments)]
fn pay_mana_ability_cost_with_choices<I, J, K, L>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
    chosen_discards: &mut J,
    chosen_exiled_battlefield: &mut K,
    chosen_sacrificed_battlefield: &mut L,
    chosen_hybrid_payment: Option<&[ManaType]>,
) -> Result<(), EngineError>
where
    I: Iterator<Item = ObjectId>,
    J: Iterator<Item = ObjectId>,
    K: Iterator<Item = ObjectId>,
    L: Iterator<Item = ObjectId>,
{
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
            )?;
        }
        Some(AbilityCost::PayLife { amount }) => {
            // CR 119.4 + CR 903.4: QuantityExpr resolves against the activator's
            // current state (e.g. commander color identity count).
            let resolved =
                super::quantity::resolve_quantity(state, amount, player, source_id).max(0) as u32;
            pay_life_cost(state, player, resolved, events)?
        }
        Some(AbilityCost::TapCreatures { count, filter }) => {
            for _ in 0..*count {
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
            random,
            self_ref,
        }) => {
            if *random {
                return Err(EngineError::InvalidAction(
                    "Unsupported random discard cost for mana ability".to_string(),
                ));
            }
            if *self_ref {
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
        Some(AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            ..
        }) => {
            if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot sacrifice this permanent as a cost".to_string(),
                ));
            }
            let _ = sacrifice::sacrifice_permanent(state, source_id, player, events)?;
        }
        // CR 117.1 + CR 118.3 + CR 605.3b: Non-self sacrifice-from-battlefield
        // as a mana ability cost (Phyrexian Altar class). The interactive flow
        // has already captured the chosen permanents; verify each is still
        // legal and route through the sacrifice replacement pipeline.
        Some(AbilityCost::Sacrifice { target, count })
            if !matches!(target, TargetFilter::SelfRef) =>
        {
            for _ in 0..*count {
                let chosen_id = chosen_sacrificed_battlefield.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing sacrificed permanent selection for mana ability".to_string(),
                    )
                })?;
                sacrifice_selected_permanent_for_mana_cost(
                    state, source_id, player, chosen_id, target, events,
                )?;
            }
        }
        // CR 118.3 + CR 605.3b: Self-exile mana ability costs are paid
        // atomically before mana production. The printed cost supplies the
        // activation zone for hand/graveyard abilities; bare self-exile defaults
        // to battlefield.
        Some(AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone,
            count: 1,
        }) => exile_self_for_mana_cost(state, source_id, *zone, events)?,
        // CR 117.1 + CR 118.3 + CR 605.3b: Non-self exile-from-battlefield as a
        // mana ability cost (Food Chain class). The interactive flow has already
        // captured the chosen permanents in `chosen_exiled_battlefield`; here we
        // verify each is still legal and move it to exile. The cost-paid object's
        // mana value is captured by the resume handler before this fn runs.
        Some(AbilityCost::Exile {
            count,
            zone,
            filter: Some(filter),
        }) if matches!(zone, None | Some(Zone::Battlefield))
            && !matches!(filter, TargetFilter::SelfRef) =>
        {
            let ctx = FilterContext::from_source(state, source_id);
            for _ in 0..*count {
                let chosen_id = chosen_exiled_battlefield.next().ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Missing exiled permanent selection for mana ability".to_string(),
                    )
                })?;
                if chosen_id == source_id {
                    return Err(EngineError::ActionNotAllowed(
                        "Source cannot satisfy its own exile cost".to_string(),
                    ));
                }
                let obj = state.objects.get(&chosen_id).ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Selected permanent for exile cost not found".to_string(),
                    )
                })?;
                if obj.zone != Zone::Battlefield || obj.controller != player {
                    return Err(EngineError::ActionNotAllowed(
                        "Selected permanent is not on the battlefield under your control"
                            .to_string(),
                    ));
                }
                if !matches_target_filter(state, chosen_id, filter, &ctx) {
                    return Err(EngineError::ActionNotAllowed(
                        "Selected permanent does not match the exile cost filter".to_string(),
                    ));
                }
                super::zones::move_to_zone(state, chosen_id, Zone::Exile, events);
            }
        }
        Some(AbilityCost::Composite { costs }) => {
            let exclude_source = costs
                .iter()
                .any(|sub_cost| matches!(sub_cost, AbilityCost::Tap));
            for sub_cost in costs {
                match sub_cost {
                    AbilityCost::Tap => tap_source(state, source_id, events)?,
                    AbilityCost::PayLife { amount } => {
                        // CR 119.4 + CR 903.4: Resolve dynamic life amount at activation.
                        let resolved =
                            super::quantity::resolve_quantity(state, amount, player, source_id)
                                .max(0) as u32;
                        pay_life_cost(state, player, resolved, events)?
                    }
                    AbilityCost::TapCreatures { count, filter } => {
                        for _ in 0..*count {
                            let chosen_id = chosen_tappers.next().ok_or_else(|| {
                                EngineError::InvalidAction(
                                    "Missing tapped creature selection for mana ability"
                                        .to_string(),
                                )
                            })?;
                            tap_selected_creature_for_mana_cost(
                                state,
                                source_id,
                                player,
                                chosen_id,
                                filter,
                                exclude_source,
                                events,
                            )?;
                        }
                    }
                    AbilityCost::Discard {
                        count,
                        filter,
                        random,
                        self_ref,
                    } => {
                        if *random {
                            return Err(EngineError::InvalidAction(
                                "Unsupported random discard cost for mana ability".to_string(),
                            ));
                        }
                        if *self_ref {
                            match crate::game::effects::discard::discard_as_cost(
                                state, source_id, player, events,
                            ) {
                                crate::game::effects::discard::DiscardOutcome::Complete => {}
                                crate::game::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => {}
                            }
                        } else {
                            let resolved =
                                super::quantity::resolve_quantity(state, count, player, source_id)
                                    .max(0) as usize;
                            for _ in 0..resolved {
                                let chosen_id = chosen_discards.next().ok_or_else(|| {
                                    EngineError::InvalidAction(
                                        "Missing discarded card selection for mana ability"
                                            .to_string(),
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
                    AbilityCost::Sacrifice {
                        target: TargetFilter::SelfRef,
                        ..
                    } => {
                        if super::static_abilities::player_cant_sacrifice_as_cost(
                            state, player, source_id,
                        ) {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot sacrifice this permanent as a cost".to_string(),
                            ));
                        }
                        let _ = sacrifice::sacrifice_permanent(state, source_id, player, events)?;
                    }
                    AbilityCost::Sacrifice { target, count } => {
                        for _ in 0..*count {
                            let chosen_id =
                                chosen_sacrificed_battlefield.next().ok_or_else(|| {
                                    EngineError::InvalidAction(
                                        "Missing sacrificed permanent selection for mana ability"
                                            .to_string(),
                                    )
                                })?;
                            sacrifice_selected_permanent_for_mana_cost(
                                state, source_id, player, chosen_id, target, events,
                            )?;
                        }
                    }
                    AbilityCost::Exile {
                        filter: Some(TargetFilter::SelfRef),
                        zone,
                        count: 1,
                    } => exile_self_for_mana_cost(state, source_id, *zone, events)?,
                    // CR 122.1 + CR 601.2b: RemoveCounter-on-self as part of a
                    // composite mana-ability cost (e.g. Gemstone Mine: `{T}, Remove
                    // a mining counter from this land: Add one mana of any color`).
                    // Resolves `CounterMatch::Any` (untyped "remove a counter")
                    // to the concrete counter type currently present on the
                    // source via `resolve_counter_match_for_removal`, then
                    // delegates to the replacement-aware helper so replacement
                    // effects on counter removal apply.
                    AbilityCost::RemoveCounter {
                        count,
                        counter_type,
                        target: None,
                    } => {
                        if let Some(resolved) =
                            super::effects::counters::resolve_counter_match_for_removal(
                                state,
                                source_id,
                                counter_type,
                            )
                        {
                            super::effects::counters::remove_counter_with_replacement(
                                state, source_id, resolved, *count, events,
                            );
                        }
                    }
                    // CR 605.3a + CR 601.2h + CR 107.4e: Mana sub-cost inside a
                    // Composite mana-ability cost (filter lands' `{W/U}, {T}`).
                    // The caller (via `chosen_mana_payment`) has already resolved
                    // any hybrid color choices (CR 107.4e); auto-pay the remaining
                    // cost from the activator's pool.
                    AbilityCost::Mana { cost } => {
                        pay_mana_sub_cost(
                            state,
                            source_id,
                            player,
                            cost,
                            chosen_hybrid_payment,
                            events,
                        )?;
                    }
                    other => {
                        return Err(EngineError::InvalidAction(format!(
                            "Unsupported mana ability sub-cost: {other:?}"
                        )));
                    }
                }
            }
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

fn exile_self_for_mana_cost(
    state: &mut GameState,
    source_id: ObjectId,
    zone: Option<Zone>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let required_zone = zone.unwrap_or(Zone::Battlefield);
    let source = state.objects.get(&source_id).ok_or_else(|| {
        EngineError::InvalidAction("Source object not found for exile cost".to_string())
    })?;
    if source.zone != required_zone {
        return Err(EngineError::ActionNotAllowed(format!(
            "Cannot exile from {:?}: source is not in that zone",
            required_zone
        )));
    }
    super::zones::move_to_zone(state, source_id, Zone::Exile, events);
    Ok(())
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
        AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            count,
        } => *count == 1,
        AbilityCost::Composite { costs } => costs.iter().all(cost_component_choice_free),
        _ => false,
    }
}

/// CR 605.3a: The controller's *other* permanents that could be activated for
/// the same `SingleColor` mana choice — identical ability definition, choice-
/// free cost, and currently activatable (untapped, on the battlefield, not
/// summoning-sick, via the shared `activatable_mana_options` gate). These are
/// the sources `GameAction::ChooseManaColor` may bulk-activate with the chosen
/// color. `exclude` is the just-activated source (already cost-paid, so omitted).
/// Sorted by id for deterministic ordering across the WASM/multiplayer boundary.
fn batch_eligible_siblings(
    state: &GameState,
    player: PlayerId,
    exclude: ObjectId,
    ability_def: &AbilityDefinition,
) -> Vec<ObjectId> {
    let candidates: Vec<ObjectId> = state
        .objects
        .iter()
        .filter_map(|(id, obj)| {
            (*id != exclude
                && obj.controller == player
                && obj.zone == Zone::Battlefield
                && obj.abilities.iter().any(|ability| ability == ability_def))
            .then_some(*id)
        })
        .collect();
    let mut siblings: Vec<ObjectId> = candidates
        .into_iter()
        .filter(|&id| !mana_sources::activatable_mana_options(state, id, player).is_empty())
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
fn enumerate_hybrid_payment_plans(pool: &ManaPool, cost: &ManaCost) -> Vec<Vec<ManaType>> {
    let hybrid_pairs = hybrid_shard_pairs(cost);
    let mut plans = Vec::new();
    enumerate_plans_rec(pool, cost, &hybrid_pairs, &mut Vec::new(), &mut plans);
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
    hybrid_pairs: &[(ManaType, ManaType)],
    chosen: &mut Vec<ManaType>,
    out: &mut Vec<Vec<ManaType>>,
) {
    if chosen.len() == hybrid_pairs.len() {
        if try_pay_with_hybrid_plan(pool, cost, chosen).is_some() {
            out.push(chosen.clone());
        }
        return;
    }
    let (a, b) = hybrid_pairs[chosen.len()];
    chosen.push(a);
    enumerate_plans_rec(pool, cost, hybrid_pairs, chosen, out);
    chosen.pop();
    if a != b {
        chosen.push(b);
        enumerate_plans_rec(pool, cost, hybrid_pairs, chosen, out);
        chosen.pop();
    }
}

/// CR 107.4e: Simulate paying `cost` from a clone of `pool` with hybrid
/// shards pinned to the colors in `plan`. Returns `Some(())` when the pool
/// covers the cost, `None` otherwise. Deterministic — uses the same
/// auto-pay rules as `pay_cost` except hybrid shards defer to `plan`.
fn try_pay_with_hybrid_plan(pool: &ManaPool, cost: &ManaCost, plan: &[ManaType]) -> Option<()> {
    let mut sim = pool.clone();
    // Simulation path — `None` context preserves the prior "can pool cover
    // this at all" semantics. Restriction-aware affordability is checked at
    // the real payment site via `pay_mana_sub_cost`.
    debit_cost_with_plan(&mut sim, cost, plan, None).ok()
}

/// CR 107.4e + CR 601.2h: Debit `cost` from `pool` using `plan` for hybrid
/// shards. Non-hybrid shards (single, Phyrexian, snow, colorless-hybrid,
/// hybrid-Phyrexian, two-generic-hybrid, X) are routed through the same
/// auto-pay rules the casting flow uses via `mana_payment::pay_cost`, but
/// with the hybrid shards already resolved, the plan is unambiguous.
///
/// Implementation: build a scratch cost with hybrid shards rewritten to
/// single-color shards per `plan`, then delegate to `pay_cost`. This keeps
/// every shard-kind's payment rules in one place.
fn debit_cost_with_plan(
    pool: &mut ManaPool,
    cost: &ManaCost,
    plan: &[ManaType],
    ctx: Option<&PaymentContext<'_>>,
) -> Result<(), mana_payment::PaymentError> {
    use crate::types::mana::ManaCostShard;
    let ManaCost::Cost { shards, generic } = cost else {
        return Ok(());
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
    mana_payment::pay_cost_with_demand_and_choices(
        pool,
        &scratch_cost,
        None,
        ctx,
        false,
        None,
        crate::types::mana::LifePaymentColors::EMPTY,
    )
    .map(|_| ())
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
fn pay_mana_sub_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &ManaCost,
    hybrid_plan: Option<&[ManaType]>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if hybrid_plan.is_none() {
        let excluded_sources = std::collections::HashSet::from([source_id]);
        return super::casting::pay_ability_mana_cost_excluding(
            state,
            player,
            source_id,
            cost,
            events,
            &excluded_sources,
        );
    }

    // CR 106.6: The mana sub-cost of a mana ability is paid as part of an
    // ability activation — spend-restrictions must be evaluated through
    // `allows_activation` (via `PaymentContext::Activation`), not through the
    // pool's restriction-blind `pay_cost`. Without this, activation-only
    // mana (e.g. Heart of Ramos) would silently pay through for the {R} half
    // of a hypothetical "{R}: Add {G}{G}" mana ability.
    let (source_types, source_subtypes) = super::casting::activation_source_types(state, source_id);
    let ctx = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
    };
    let pool = &mut state.players[player.0 as usize].mana_pool;
    let (spent, _life) = match hybrid_plan {
        Some(plan) => debit_cost_with_plan(pool, cost, plan, Some(&ctx))
            .map(|_| (Vec::new(), Vec::new()))
            .map_err(|_| {
                EngineError::ActionNotAllowed(
                    "Mana pool cannot cover mana ability cost".to_string(),
                )
            })?,
        None => mana_payment::pay_cost_with_demand_and_choices(
            pool,
            cost,
            None,
            Some(&ctx),
            false,
            None,
            // CR 107.4f: same K'rrik-not-applicable rationale as above.
            crate::types::mana::LifePaymentColors::EMPTY,
        )
        .map_err(|_| {
            EngineError::ActionNotAllowed("Mana pool cannot cover mana ability cost".to_string())
        })?,
    };
    let _ = spent;
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
    let obj = state.objects.get_mut(&source_id).unwrap();
    obj.tapped = true;
    events.push(GameEvent::PermanentTapped {
        object_id: source_id,
        caused_by: None,
    });
    Ok(())
}

fn tap_creature_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = find_tap_creatures_cost(cost.as_ref()?)?;
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

fn find_tap_creatures_cost(cost: &AbilityCost) -> Option<(u32, &TargetFilter)> {
    match cost {
        AbilityCost::TapCreatures { count, filter } => Some((*count, filter)),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_tap_creatures_cost),
        _ => None,
    }
}

/// CR 117.1 + CR 118.3: Match `AbilityCost::Exile` shapes that target a
/// non-self battlefield permanent. Returns `(count, filter)` if found, else
/// `None`. Both `zone: None` (parser convention when the filter implies
/// battlefield) and `zone: Some(Battlefield)` are accepted.
fn find_exile_from_battlefield_cost(cost: &AbilityCost) -> Option<(u32, &TargetFilter)> {
    match cost {
        AbilityCost::Exile {
            count,
            zone,
            filter: Some(filter),
        } if matches!(zone, None | Some(Zone::Battlefield))
            && !matches!(filter, TargetFilter::SelfRef) =>
        {
            Some((*count, filter))
        }
        AbilityCost::Composite { costs } => costs.iter().find_map(find_exile_from_battlefield_cost),
        _ => None,
    }
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible battlefield permanents
/// for an `AbilityCost::Exile { zone: None|Battlefield, filter: !SelfRef }`
/// mana ability cost (Food Chain class). Excludes the source object so
/// abilities cannot exile themselves to satisfy their own cost.
fn exile_from_battlefield_cost_choice(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &Option<AbilityCost>,
) -> Option<(usize, Vec<ObjectId>)> {
    let (count, filter) = find_exile_from_battlefield_cost(cost.as_ref()?)?;
    let ctx = FilterContext::from_source(state, source_id);
    let permanents = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if id == source_id {
                return false;
            }
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.zone != Zone::Battlefield || obj.controller != player {
                return false;
            }
            matches_target_filter(state, id, filter, &ctx)
        })
        .collect();
    Some((count as usize, permanents))
}

/// CR 117.1 + CR 118.3 + CR 605.3b: Surface eligible battlefield permanents
/// for an `AbilityCost::Sacrifice { target: !SelfRef }` mana ability cost.
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
            self_ref: false,
            random: false,
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

    state.objects.get_mut(&chosen_id).unwrap().tapped = true;
    events.push(GameEvent::PermanentTapped {
        object_id: chosen_id,
        caused_by: None,
    });
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
) -> Result<(), EngineError> {
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
    match sacrifice::sacrifice_permanent(state, chosen_id, player, events)? {
        sacrifice::SacrificeOutcome::Complete => Ok(()),
        sacrifice::SacrificeOutcome::NeedsReplacementChoice(_) => Ok(()),
    }
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

fn resume_waiting_for(player: PlayerId, resume: ManaAbilityResume) -> WaitingFor {
    match resume {
        ManaAbilityResume::Priority => WaitingFor::Priority { player },
        ManaAbilityResume::ManaPayment { convoke_mode } => WaitingFor::ManaPayment {
            player,
            convoke_mode,
        },
        ManaAbilityResume::UnlessPayment {
            cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        } => WaitingFor::UnlessPayment {
            player,
            cost: *cost,
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityKind, AbilityTag, ActivationRestriction, Comparator,
        ContinuousModification, ControllerRef, DevotionColors, Duration, Effect, LinkedExileScope,
        ManaContribution, ManaProduction, MultiTargetSpec, ObjectScope, PlayerScope, QuantityExpr,
        QuantityRef, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
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
        use crate::types::mana::ManaUnit;
        for _ in 0..count {
            state.players[player.0 as usize].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
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
    fn non_mana_api_type_not_detected() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
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
                restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation(
                    "Elemental".to_string(),
                )],
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
                    crate::types::mana::ManaRestriction::OnlyForTypeSpellsOrAbilities(
                        "Elemental".to_string()
                    )
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
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
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
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(def);
        id
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
                    AbilityCost::Sacrifice {
                        target: TargetFilter::SelfRef,
                        count: 1,
                    },
                ],
            }
        )));
        assert!(cost_resolves_without_choice(&Some(AbilityCost::Tap)));
        assert!(cost_resolves_without_choice(&None));

        // Phyrexian Altar: sacrifice a (non-self) creature → requires a choice.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: 1,
            }
        )));
        // Self-sacrifice of more than one is not the single-token shape.
        assert!(!cost_resolves_without_choice(&Some(
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 2,
            }
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
                    random: false,
                    self_ref: false,
                },
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
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
            WaitingFor::DiscardForManaAbility {
                player,
                count,
                cards,
                pending_mana_ability,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(cards.len(), 2);
                *pending_mana_ability
            }
            other => panic!("expected DiscardForManaAbility, got {other:?}"),
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
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
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
        // CR 400.7 + CR 610.3: When Pit of Offerings leaves the battlefield,
        // its `TrackedBySource` exile links are dropped. A blink (LTB then
        // re-ETB) creates a new object that inherits no linkage.
        let mut state = GameState::new_two_player(42);
        let (pit, _exiled) = pit_of_offerings_with_exiled_card(
            &mut state,
            PlayerId(0),
            "Llanowar Elves",
            vec![ManaColor::Green],
        );

        assert_eq!(state.exile_links.len(), 1, "precondition: link was created");

        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, pit, Zone::Exile, &mut events);

        // The TrackedBySource link keyed to the (departed) Pit object must be gone.
        assert!(
            state.exile_links.iter().all(|link| link.source_id != pit),
            "TrackedBySource exile links must be pruned when the source leaves \
             the battlefield (CR 400.7)"
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

        resolve_triggered_mana_ability_inline(&mut state, &ability, Some(&event), &mut events);

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
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
                color_override: None,
                resume: ManaAbilityResume::Priority,
                chosen_tappers: Vec::new(),
                chosen_discards: Vec::new(),
                chosen_mana_payment: None,
                chosen_exiled_battlefield: Vec::new(),
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
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
        .cost(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: 1,
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
            WaitingFor::SacrificeForManaAbility {
                player,
                count,
                permanents,
                pending_mana_ability,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![creature]);
                assert!(!permanents.contains(&opponent_creature));
                pending_mana_ability
            }
            other => panic!("expected SacrificeForManaAbility, got {other:?}"),
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
            color_override: Some(ProductionOverride::SingleColor(ManaType::Black)),
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
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
            WaitingFor::SacrificeForManaAbility {
                count,
                permanents,
                pending_mana_ability,
                ..
            } => {
                assert_eq!(count, 1);
                assert_eq!(permanents, vec![source]);
                pending_mana_ability
            }
            other => panic!("expected SacrificeForManaAbility, got {other:?}"),
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
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_from_battlefield_for_mana_ability(
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
            color_override: Some(ProductionOverride::SingleColor(ManaType::Red)),
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_from_battlefield_for_mana_ability(
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
            color_override: Some(ProductionOverride::SingleColor(ManaType::Green)),
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_exiled_battlefield: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        };
        let mut events = Vec::new();
        let _ = handle_exile_from_battlefield_for_mana_ability(
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
}
