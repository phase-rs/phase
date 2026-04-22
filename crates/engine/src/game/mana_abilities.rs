use crate::types::ability::{
    AbilityCost, AbilityDefinition, Effect, ManaProduction, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, ManaAbilityResume, ManaChoice, ManaChoicePrompt, PendingManaAbility,
    ProductionOverride, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::mana::resolve_mana_types;
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
    if !matches!(*ability_def.effect, Effect::Mana { .. }) {
        return false;
    }
    // CR 605.1a: A targeted mana-producing ability is not a mana ability.
    // multi_target is the explicit targeting mechanism on AbilityDefinition.
    ability_def.multi_target.is_none()
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

    produce_mana_from_ability(
        state,
        source_id,
        player,
        ability_def,
        events,
        color_override,
    );
    Ok(())
}

/// Produce mana from a resolved mana ability without paying costs.
/// Shared by `resolve_mana_ability` (cost paid inline) and `handle_choose_mana_color`
/// (cost already paid during the `TapCreaturesForManaAbility` phase).
fn produce_mana_from_ability(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    ability_def: &AbilityDefinition,
    events: &mut Vec<GameEvent>,
    color_override: Option<ProductionOverride>,
) {
    let produced_mana = match &*ability_def.effect {
        Effect::Mana { produced, .. } => match color_override {
            // `Combination` is pre-chosen — skip `resolve_mana_types` entirely
            // so the exact sequence lands in the pool (CR 605.3b).
            Some(ProductionOverride::Combination(types)) => types,
            Some(ProductionOverride::SingleColor(color)) => {
                let resolved = resolve_mana_types(produced, state, player, source_id);
                vec![color; resolved.len()]
            }
            None => resolve_mana_types(produced, state, player, source_id),
        },
        _ => Vec::new(),
    };

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
        mana_payment::produce_mana(state, source_id, mana_type, player, tapped, events);
    }
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

    if pending.color_override.is_none() {
        if let Some(choice) = mana_choice_prompt(&ability_def.effect, state, pending.source_id) {
            pay_mana_ability_cost_with_choices(
                state,
                pending.source_id,
                pending.player,
                &ability_def.cost,
                events,
                &mut pending.chosen_tappers.iter().copied(),
                &mut pending.chosen_discards.iter().copied(),
            )?;
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
) -> Result<(), EngineError> {
    let mut chosen = tapped_creatures.iter().copied();
    let mut discarded = discarded_cards.iter().copied();
    pay_mana_ability_cost_with_choices(
        state,
        source_id,
        player,
        &ability_def.cost,
        events,
        &mut chosen,
        &mut discarded,
    )?;
    if chosen.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many creatures selected for mana ability cost".to_string(),
        ));
    }
    if discarded.next().is_some() {
        return Err(EngineError::InvalidAction(
            "Too many cards selected for mana ability cost".to_string(),
        ));
    }

    let produced_mana = match &*ability_def.effect {
        Effect::Mana { produced, .. } => match color_override {
            Some(ProductionOverride::Combination(types)) => types,
            Some(ProductionOverride::SingleColor(color)) => {
                let resolved = resolve_mana_types(produced, &*state, player, source_id);
                vec![color; resolved.len()]
            }
            None => resolve_mana_types(produced, &*state, player, source_id),
        },
        _ => Vec::new(),
    };

    let tapped = mana_sources::has_tap_component(&ability_def.cost);
    for mana_type in produced_mana {
        mana_payment::produce_mana(state, source_id, mana_type, player, tapped, events);
    }

    Ok(())
}

fn pay_mana_ability_cost_with_choices<I, J>(
    state: &mut GameState,
    source_id: ObjectId,
    player: PlayerId,
    cost: &Option<AbilityCost>,
    events: &mut Vec<GameEvent>,
    chosen_tappers: &mut I,
    chosen_discards: &mut J,
) -> Result<(), EngineError>
where
    I: Iterator<Item = ObjectId>,
    J: Iterator<Item = ObjectId>,
{
    match cost {
        Some(AbilityCost::Tap) => tap_source(state, source_id, events)?,
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
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityKind, Effect, LinkedExileScope, ManaContribution, ManaProduction,
        MultiTargetSpec, QuantityExpr, TargetFilter,
    };
    use crate::types::game_state::{ExileLink, ExileLinkKind};
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaType};
    use crate::types::zones::Zone;

    fn make_mana_ability(produced: ManaProduction) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            },
        )
        .cost(AbilityCost::Tap)
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
        state
            .objects
            .get_mut(&led)
            .unwrap()
            .abilities
            .push(ability.clone());

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
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
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
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::ChoiceAmongExiledColors {
                            source: LinkedExileScope::ThisObject,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
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
        state.objects.get_mut(&pit).unwrap().abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
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
        state.objects.get_mut(&pit).unwrap().abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
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
        state.objects.get_mut(&pit).unwrap().abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Ef::Mana {
                    produced: ManaProduction::ChoiceAmongExiledColors {
                        source: LinkedExileScope::ThisObject,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
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
        obj.abilities.push(ability.clone());
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
        obj.abilities.push(ability);

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: source,
            ability_index: 0,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
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
        obj.abilities.push(ability.clone());
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

    // ─────────────────────────────────────────────────────────────
    // ChoiceAmongCombinations (filter lands — Shadowmoor/Eventide).
    // ─────────────────────────────────────────────────────────────

    fn sunken_ruins_colored_ability() -> AbilityDefinition {
        // CR 605.3b + CR 106.1a: `{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}`.
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
            },
        )
        .cost(AbilityCost::Tap)
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
        obj.abilities.push(ability.clone());

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
        obj.abilities.push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: 0,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
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
        obj.abilities.push(ability.clone());

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
        obj.abilities.push(sunken_ruins_colored_ability());

        let pending = PendingManaAbility {
            player: PlayerId(0),
            source_id: ruins,
            ability_index: 0,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
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
}
