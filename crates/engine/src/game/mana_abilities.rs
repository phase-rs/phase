use crate::types::ability::{
    AbilityCost, AbilityDefinition, Effect, ManaProduction, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ManaAbilityResume, ManaChoice, ManaChoicePrompt, PendingManaAbility,
    ProductionOverride, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaCost, ManaPool, ManaType, PaymentContext};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::mana::resolve_restrictions;
use super::engine::EngineError;
use super::filter::{matches_target_filter, FilterContext};
use super::life_costs::{self, PayLifeCostResult};
use super::mana_payment;
use super::mana_sources;
use super::mana_sources::mana_color_to_type;
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
/// Criterion (b) accepts only `ManaAdded` today. CR 605.1b also admits
/// "triggered from the activation/resolution of an activated mana ability" —
/// but mana abilities bypass the stack and do not currently emit a
/// distinguishable `AbilityActivated` event (see `resolve_mana_ability` — only
/// `ManaAdded` events are produced). A pool-add-less mana ability (hypothetical
/// conditional producer that yields zero mana) would not reach this classifier
/// via `ManaAdded`; widening (b) to `AbilityActivated` requires first emitting
/// an event specifically tied to mana-ability activation so the axis can be
/// distinguished from ordinary activated abilities. No real card exercises the
/// gap today.
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
    // (b) Triggered by mana being added to a pool. See the doc comment above for
    // the deliberately-not-yet-widened `AbilityActivated` axis.
    matches!(trigger_event, Some(GameEvent::ManaAdded { .. }))
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
    events: &mut Vec<GameEvent>,
) {
    // Use the standard resolution entry so sub_ability chains resolve uniformly.
    let _ = super::effects::resolve_ability_chain(state, ability, events, 0);
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
/// `cost_paid_object_mana_value` carries the captured mana value
/// (CR 117.1 + CR 202.3) of any object exiled or sacrificed as part of cost
/// payment so the production count can resolve
/// `QuantityRef::CostPaidObjectManaValue` (Food Chain, Burnt Offering,
/// Metamorphosis class).
fn produce_mana_from_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
    cost_paid_object_mana_value: Option<u32>,
) {
    // CR 117.1 + CR 202.3: Build a transient `ResolvedAbility` carrying the
    // cost-paid object snapshot so quantity resolution sees it. Reused for
    // both production-count and sub-chain resolution paths so the same
    // snapshot is visible end-to-end.
    let mut resolved_for_quantity =
        super::ability_utils::build_resolved_from_def(ability_def, source_id, player);
    if let Some(mv) = cost_paid_object_mana_value {
        resolved_for_quantity.set_cost_paid_object_mana_value_recursive(mv);
    }

    // CR 106.6: Resolve spend-restriction templates, grants, and expiry so they
    // attach to each produced `ManaUnit`. Dropping these here is the bug that
    // made Flamebraider's Elemental-only mana behave as unrestricted mana.
    let (produced_mana, restrictions, grants, expiry) = match &*ability_def.effect {
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
                Some(ProductionOverride::SingleColor(color)) => {
                    let resolved = super::effects::mana::resolve_mana_types_for_ability(
                        produced,
                        state,
                        &resolved_for_quantity,
                    );
                    vec![color; resolved.len()]
                }
                None => super::effects::mana::resolve_mana_types_for_ability(
                    produced,
                    state,
                    &resolved_for_quantity,
                ),
            };
            let concrete = resolve_restrictions(restrictions, state, source_id);
            (mana, concrete, grants.clone(), *expiry)
        }
        _ => (Vec::new(), Vec::new(), Vec::new(), None),
    };

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
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

    // CR 605.3b + CR 605.1a: A mana ability with a non-mana clause in its
    // effect chain (e.g. painlands' "This land deals 1 damage to you.")
    // resolves that chain inline — mana abilities don't use the stack, so
    // the sub-ability runs as part of the same atomic resolution.
    resolve_mana_ability_sub_chain(state, source_id, player, ability_def, events);
}

/// CR 605.3b: Mana abilities resolve immediately unless paying the cost requires a choice.
#[allow(clippy::too_many_arguments)]
pub fn activate_mana_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    _ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    resume: ManaAbilityResume,
    color_override: Option<ProductionOverride>,
) -> Result<WaitingFor, EngineError> {
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
            cost_paid_object_mana_value: None,
        },
        events,
    )
}

/// Extract the prompt shape for a mana ability that requires a player choice.
///
/// Returns `Some(ManaChoicePrompt::SingleColor)` when the player must pick one
/// color from a set (AnyOneColor, ChoiceAmongExiledColors) and
/// `Some(ManaChoicePrompt::Combination)` when the player must pick one of
/// several fixed multi-mana sequences (filter lands). Returns `None` when the
/// production is fully determined (Fixed, Colorless, single-option AnyOneColor).
pub(crate) fn mana_choice_prompt(
    effect: &Effect,
    state: &GameState,
    source_id: ObjectId,
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
        pending.cost_paid_object_mana_value,
    );

    Ok(resume_waiting_for(pending.player, pending.resume.clone()))
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

    // CR 117.1 + CR 202.3: Capture the cost-paid object's mana value before
    // it leaves the battlefield. Uses the first chosen permanent — the class
    // only ever exiles one cost-tracked object (Food Chain). Multi-exile
    // costs are unusual but if they ever appear the resolver reads the first
    // entry; downstream design can extend to a Vec snapshot if a card needs
    // per-object mana values.
    let captured_mv = chosen
        .first()
        .and_then(|id| state.objects.get(id).map(|obj| obj.mana_cost.mana_value()));

    let mut updated = pending.clone();
    updated.chosen_exiled_battlefield = chosen.to_vec();
    updated.cost_paid_object_mana_value = captured_mv;
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
    pending: PendingManaAbility,
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

    // CR 605.3a + CR 601.2h + CR 107.4e: Resolve the mana sub-cost payment before
    // producing any mana or prompting for output choices. If the cost has hybrid
    // shards (CR 107.4e) with more than one legal color assignment given the
    // current pool, surface `PayManaAbilityMana` so the player picks. Unambiguous
    // plans auto-pay.
    if pending.chosen_mana_payment.is_none() {
        if let Some(sub_cost) = mana_sub_cost_of(&ability_def.cost) {
            let pool = &state.players[pending.player.0 as usize].mana_pool;
            let plans = enumerate_hybrid_payment_plans(pool, sub_cost);
            match plans.len() {
                0 => {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay mana cost for mana ability".to_string(),
                    ));
                }
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
        if let Some(choice) = mana_choice_prompt(&ability_def.effect, state, pending.source_id) {
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
            return Ok(WaitingFor::ChooseManaColor {
                player: pending.player,
                choice,
                pending_mana_ability: Box::new(pending),
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
        pending.chosen_mana_payment.as_deref(),
        pending.cost_paid_object_mana_value,
    )?;
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
    chosen_hybrid_payment: Option<&[ManaType]>,
    cost_paid_object_mana_value: Option<u32>,
) -> Result<(), EngineError> {
    let mut chosen = tapped_creatures.iter().copied();
    let mut discarded = discarded_cards.iter().copied();
    let mut exiled = exiled_battlefield.iter().copied();
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        &ability_def.cost,
        events,
        &mut chosen,
        &mut discarded,
        &mut exiled,
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

    // CR 117.1 + CR 202.3: Build a transient `ResolvedAbility` carrying the
    // cost-paid object snapshot so production-count resolution sees it
    // (Food Chain class).
    let mut resolved_for_quantity =
        super::ability_utils::build_resolved_from_def(ability_def, source_id, player);
    if let Some(mv) = cost_paid_object_mana_value {
        resolved_for_quantity.set_cost_paid_object_mana_value_recursive(mv);
    }

    // CR 106.6: Thread restrictions, grants, and expiry through the
    // selected-choices path too — otherwise color-picked or hybrid-paid mana
    // abilities would still emit unrestricted mana.
    let (produced_mana, restrictions, grants, expiry) = match &*ability_def.effect {
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
                    let resolved = super::effects::mana::resolve_mana_types_for_ability(
                        produced,
                        &*state,
                        &resolved_for_quantity,
                    );
                    vec![color; resolved.len()]
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

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
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

    // CR 605.3b + CR 605.1a: Resolve the sub-ability chain inline (painlands'
    // "deals 1 damage to you", Llanowar Wastes-style self-damage, etc.).
    resolve_mana_ability_sub_chain(state, source_id, player, ability_def, events);

    Ok(())
}

/// CR 605.3b + CR 605.1a: Run a mana ability's `sub_ability` chain inline.
/// Mana abilities don't use the stack, so non-mana clauses ("This land deals
/// 1 damage to you.") resolve atomically with the mana production. Walks the
/// full chain via `resolve_ability_chain` so nested effects (DealDamage on
/// controller, GainLife, etc.) route through the standard effect handlers.
fn resolve_mana_ability_sub_chain(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
) {
    let Some(sub_def) = ability_def.sub_ability.as_deref() else {
        return;
    };
    let resolved = super::ability_utils::build_resolved_from_def(sub_def, source_id, player);
    // Errors during the sub-chain are non-fatal — mana has already been
    // added to the pool and the cost has been paid. The damage/life clause
    // of a painland cannot legitimately fail in a well-formed game state.
    let _ = super::effects::resolve_ability_chain(state, &resolved, events, 0);
}

#[allow(clippy::too_many_arguments)]
fn pay_mana_ability_cost_with_choices<I, J, K>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
    chosen_discards: &mut J,
    chosen_exiled_battlefield: &mut K,
    chosen_hybrid_payment: Option<&[ManaType]>,
) -> Result<(), EngineError>
where
    I: Iterator<Item = ObjectId>,
    J: Iterator<Item = ObjectId>,
    K: Iterator<Item = ObjectId>,
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
                        let _ = sacrifice::sacrifice_permanent(state, source_id, player, events)?;
                    }
                    // CR 122.1 + CR 601.2b: RemoveCounter-on-self as part of a
                    // composite mana-ability cost (e.g. Gemstone Mine: `{T}, Remove
                    // a mining counter from this land: Add one mana of any color`).
                    // Delegates to the same replacement-aware helper used by
                    // casting.rs so replacement effects on counter removal apply.
                    AbilityCost::RemoveCounter {
                        count,
                        counter_type,
                        target: None,
                    } => {
                        let counter_kind = crate::types::counter::parse_counter_type(counter_type);
                        super::effects::counters::remove_counter_with_replacement(
                            state,
                            source_id,
                            counter_kind,
                            *count,
                            events,
                        );
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
    match life_costs::pay_life_as_cost(state, player, amount, events) {
        PayLifeCostResult::Paid { .. } => Ok(()),
        PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => Err(
            EngineError::ActionNotAllowed("Cannot pay life cost for mana ability".to_string()),
        ),
    }
}

/// CR 605.3a + CR 605.1a: Extract the nested `ManaCost` from an ability cost
/// that contains a mana sub-cost (either at top level or inside a Composite).
/// Returns `None` for costs with no mana payment component.
fn mana_sub_cost_of(cost: &Option<AbilityCost>) -> Option<&ManaCost> {
    match cost {
        Some(AbilityCost::Mana { cost }) => Some(cost),
        Some(AbilityCost::Composite { costs }) => costs.iter().find_map(|c| match c {
            AbilityCost::Mana { cost } => Some(cost),
            _ => None,
        }),
        _ => None,
    }
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
    mana_payment::pay_cost_with_demand_and_choices(pool, &scratch_cost, None, ctx, false, None)
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

/// CR 605.3a + CR 601.2h: Debit a mana sub-cost from the activator's pool.
/// If `hybrid_plan` is provided, hybrid shards are pinned to those colors;
/// otherwise `pay_cost` auto-decides via the standard casting rules. An
/// `InsufficientMana` error surfaces as `ActionNotAllowed` so the UI can
/// recover cleanly (the pre-activation gate should have prevented this).
fn pay_mana_sub_cost(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &ManaCost,
    hybrid_plan: Option<&[ManaType]>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
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
            effect_description,
        } => WaitingFor::UnlessPayment {
            player,
            cost,
            pending_effect,
            effect_description,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityKind, Effect, LinkedExileScope, ManaContribution, ManaProduction,
        MultiTargetSpec, QuantityExpr, TargetFilter,
    };
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCostShard, ManaType};
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

    use crate::game::test_fixtures::brushland_colored_ability;

    fn seed_pool_with(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        use crate::types::mana::ManaUnit;
        for _ in 0..count {
            state.players[player.0 as usize].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
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
        def.multi_target = Some(MultiTargetSpec {
            min: 1,
            max: Some(1),
        });
        assert!(!is_mana_ability(&def));
    }

    #[test]
    fn draw_ability_is_not_mana_ability() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                up_to: false,
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
                        qty: crate::types::ability::QuantityRef::HandSize,
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
                pending_mana_ability,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(options.len(), 5);
                *pending_mana_ability
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
                up_to: false,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn mana_added_event() -> GameEvent {
        GameEvent::ManaAdded {
            player_id: PlayerId(0),
            mana_type: ManaType::Green,
            source_id: ObjectId(1),
            tapped_for_mana: true,
        }
    }

    #[test]
    fn classifier_accepts_head_effect_mana_on_mana_added() {
        let ability = mana_producing_resolved();
        assert!(is_triggered_mana_ability(
            &ability,
            Some(&mana_added_event())
        ));
    }

    #[test]
    fn classifier_rejects_non_mana_added_event() {
        // CR 605.1b criterion (b): mana abilities don't emit a mana-ability-
        // specific activation event today, so only `ManaAdded` qualifies.
        // An unrelated event (e.g. `AbilityActivated`) must not route through
        // the inline resolver.
        let ability = mana_producing_resolved();
        let ev = GameEvent::AbilityActivated {
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
        assert!(is_triggered_mana_ability(&head, Some(&mana_added_event())));
    }

    #[test]
    fn classifier_rejects_mixed_mana_plus_non_mana_chain() {
        // CR 605.1b criterion (c): "every link is mana" — a chain with mana
        // at the head but a non-mana sub (e.g., draw a card) MUST use the
        // stack. Routing such a chain inline would silently perform the
        // non-mana effect without giving players priority.
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
    }

    #[test]
    fn classifier_rejects_chain_without_any_mana_effect() {
        let mut head = draw_resolved();
        head.sub_ability = Some(Box::new(draw_resolved()));
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
    }

    #[test]
    fn classifier_rejects_sub_ability_with_multi_target() {
        // CR 605.1b criterion (a) + CR 115.6: any link declaring targets
        // anywhere in the chain disqualifies inline resolution.
        let mut sub = mana_producing_resolved();
        sub.multi_target = Some(MultiTargetSpec {
            min: 1,
            max: Some(1),
        });
        let mut head = mana_producing_resolved();
        head.sub_ability = Some(Box::new(sub));
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
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
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
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
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
    }

    #[test]
    fn classifier_walks_else_ability_for_criterion_a() {
        // Mirror for criterion (a): a targeted `else_ability` branch
        // disqualifies even when the main chain is target-free.
        let mut else_branch = mana_producing_resolved();
        else_branch.targets = vec![crate::types::ability::TargetRef::Object(ObjectId(7))];
        let mut head = mana_producing_resolved();
        head.else_ability = Some(Box::new(else_branch));
        assert!(!is_triggered_mana_ability(&head, Some(&mana_added_event())));
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
            cost_paid_object_mana_value: None,
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
                cost_paid_object_mana_value: None,
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
            cost_paid_object_mana_value: None,
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
            cost_paid_object_mana_value: None,
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
            WaitingFor::ChooseManaColor {
                pending_mana_ability,
                ..
            } => pending_mana_ability,
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
            cost_paid_object_mana_value: None,
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
                    counter_type: "mining".to_string(),
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
            cost_paid_object_mana_value: None,
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
            ManaSpendRestriction, QuantityRef, TargetFilter as TF, TypedFilter,
        };
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::CostPaidObjectManaValue,
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
            cost_paid_object_mana_value: None,
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
            cost_paid_object_mana_value: None,
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
    /// `QuantityRef::CostPaidObjectManaValue` to that value at production time.
    #[test]
    fn cost_paid_object_mana_value_resolves_via_resolved_ability_field() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::QuantityRef;

        let state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::CostPaidObjectManaValue,
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
        // Simulate a 5-MV creature being sacrificed as cost.
        ability.set_cost_paid_object_mana_value_recursive(5);

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::CostPaidObjectManaValue,
            },
            &ability,
        );
        assert_eq!(
            resolved, 5,
            "CostPaidObjectManaValue must resolve to the captured mana value"
        );
    }

    /// Resolver returns 0 when no cost-paid object snapshot is in scope —
    /// regression guard that avoids spurious mana production for unrelated
    /// abilities.
    #[test]
    fn cost_paid_object_mana_value_returns_zero_without_snapshot() {
        use crate::game::quantity::resolve_quantity_with_targets;
        use crate::types::ability::QuantityRef;

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
        // No `set_cost_paid_object_mana_value_recursive` — field stays None.

        let resolved = resolve_quantity_with_targets(
            &state,
            &QuantityExpr::Ref {
                qty: QuantityRef::CostPaidObjectManaValue,
            },
            &ability,
        );
        assert_eq!(
            resolved, 0,
            "CostPaidObjectManaValue must return 0 when no snapshot was captured"
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
            cost_paid_object_mana_value: None,
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
