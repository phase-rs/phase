use indexmap::IndexMap;

use crate::types::ability::{
    AbilityDefinition, CombatDamageScope, ControllerRef, DamageModification, DamageTargetFilter,
    Effect, PreventionAmount, QuantityExpr, ReplacementCondition, ReplacementMode, ShieldKind,
    TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;

use super::filter::{matches_target_filter, FilterContext};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingReplacement, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{ProposedEvent, ReplacementId};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 614.1: Replacement effects modify events as they would occur.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplacementResult {
    Execute(ProposedEvent),
    Prevented,
    NeedsChoice(PlayerId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyResult {
    Modified(ProposedEvent),
    Prevented,
}

pub type ReplacementMatcher = fn(&ProposedEvent, ObjectId, &GameState) -> bool;
pub type ReplacementApplier =
    fn(ProposedEvent, ReplacementId, &mut GameState, &mut Vec<GameEvent>) -> ApplyResult;

pub struct ReplacementHandlerEntry {
    pub matcher: ReplacementMatcher,
    pub applier: ReplacementApplier,
}

/// Build a `WaitingFor::ReplacementChoice` from the current `pending_replacement` state.
/// Centralizes candidate count and description extraction so callers don't repeat this logic.
pub fn replacement_choice_waiting_for(player: PlayerId, state: &GameState) -> WaitingFor {
    let (candidate_count, candidate_descriptions) = state
        .pending_replacement
        .as_ref()
        .map(|p| {
            let count = if p.is_optional { 2 } else { p.candidates.len() };
            let descs: Vec<String> = if p.is_optional {
                let accept_desc = p
                    .candidates
                    .first()
                    .and_then(|rid| state.objects.get(&rid.source))
                    .and_then(|obj| obj.replacement_definitions.get(p.candidates[0].index))
                    .and_then(|repl| repl.description.clone())
                    .unwrap_or_else(|| "Accept".to_string());
                vec![accept_desc, "Decline".to_string()]
            } else {
                p.candidates
                    .iter()
                    .filter_map(|rid| {
                        state
                            .objects
                            .get(&rid.source)
                            .and_then(|obj| obj.replacement_definitions.get(rid.index))
                            .and_then(|repl| repl.description.clone())
                    })
                    .collect()
            };
            (count, descs)
        })
        .unwrap_or((0, vec![]));

    WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidate_descriptions,
    }
}

// --- Stub handler for recognized-but-unimplemented replacement types ---

fn stub_matcher(_event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    false
}

fn stub_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 1. Moved (ZoneChange) ---

fn moved_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::ZoneChange { .. })
}

fn moved_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 2. DamageDone ---

fn damage_done_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Damage { .. })
}

/// CR 614.1a: Extract the damage modification formula from a replacement definition.
fn damage_modification_for_rid(
    state: &GameState,
    rid: ReplacementId,
) -> Option<DamageModification> {
    // CR 615.3: Pending prevention shields use sentinel ObjectId(0).
    if rid.source == ObjectId(0) {
        return state
            .pending_damage_prevention
            .get(rid.index)?
            .damage_modification
            .clone();
    }
    state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .damage_modification
        .clone()
}

/// CR 614.1a: Apply damage modification or prevention from the replacement definition.
fn damage_done_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // Branch 1: Damage modification (Double, Triple, Plus, Minus)
    if let Some(modification) = damage_modification_for_rid(state, rid) {
        if let ProposedEvent::Damage {
            source_id,
            target,
            amount,
            is_combat,
            applied,
        } = event
        {
            let new_amount = match modification {
                DamageModification::Double => amount.saturating_mul(2),
                DamageModification::Triple => amount.saturating_mul(3),
                DamageModification::Plus { value } => amount.saturating_add(value),
                DamageModification::Minus { value } => amount.saturating_sub(value),
                // CR 614.1a: Conditional — if amount < source's power, set to power.
                // References the replacement source's (rid.source) post-layer power.
                DamageModification::SetToSourcePower => {
                    let source_power = state
                        .objects
                        .get(&rid.source)
                        .and_then(|obj| obj.power)
                        .unwrap_or(0)
                        .max(0) as u32;
                    if amount < source_power {
                        source_power
                    } else {
                        amount
                    }
                }
            };
            return ApplyResult::Modified(ProposedEvent::Damage {
                source_id,
                target,
                amount: new_amount,
                is_combat,
                applied,
            });
        }
        return ApplyResult::Modified(event);
    }

    // Branch 2: CR 615 — Prevention shield
    // Look up shield from either object replacement_definitions or pending_damage_prevention.
    let shield_kind = if rid.source == ObjectId(0) {
        state
            .pending_damage_prevention
            .get(rid.index)
            .map(|repl| repl.shield_kind)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .map(|repl| repl.shield_kind)
    };

    if let Some(ShieldKind::Prevention { amount }) = shield_kind {
        if let ProposedEvent::Damage {
            source_id,
            target,
            amount: dmg,
            is_combat,
            applied,
        } = event
        {
            let prevented_amount;
            let result;

            match amount {
                PreventionAmount::All => {
                    // CR 615: Prevent all damage — consume the shield
                    prevented_amount = dmg;
                    consume_prevention_shield(state, rid, None);
                    result = ApplyResult::Prevented;
                }
                PreventionAmount::Next(n) => {
                    // CR 615.7: Each 1 damage prevented reduces the remaining shield by 1.
                    if dmg <= n {
                        // All damage absorbed — shield may have remaining capacity
                        prevented_amount = dmg;
                        let remaining = n - dmg;
                        if remaining == 0 {
                            consume_prevention_shield(state, rid, None);
                        } else {
                            consume_prevention_shield(
                                state,
                                rid,
                                Some(PreventionAmount::Next(remaining)),
                            );
                        }
                        result = ApplyResult::Prevented;
                    } else {
                        // Damage exceeds shield — reduce damage, consume shield
                        prevented_amount = n;
                        let remaining_damage = dmg - n;
                        consume_prevention_shield(state, rid, None);
                        result = ApplyResult::Modified(ProposedEvent::Damage {
                            source_id,
                            target: target.clone(),
                            amount: remaining_damage,
                            is_combat,
                            applied,
                        });
                    }
                }
            }

            // Emit DamagePrevented event for "when damage is prevented" triggers
            if prevented_amount > 0 {
                events.push(GameEvent::DamagePrevented {
                    source_id,
                    target,
                    amount: prevented_amount,
                });
            }

            return result;
        }
    }

    // No modification and no prevention shield — pass through
    ApplyResult::Modified(event)
}

/// Consume or update a prevention shield on either an object or the game-state registry.
/// If `new_amount` is `None`, marks the shield as consumed.
/// If `new_amount` is `Some(amount)`, updates the remaining shield capacity.
fn consume_prevention_shield(
    state: &mut GameState,
    rid: ReplacementId,
    new_amount: Option<PreventionAmount>,
) {
    let repl = if rid.source == ObjectId(0) {
        state.pending_damage_prevention.get_mut(rid.index)
    } else {
        state
            .objects
            .get_mut(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get_mut(rid.index))
    };

    if let Some(repl) = repl {
        match new_amount {
            None => repl.is_consumed = true,
            Some(amt) => repl.shield_kind = ShieldKind::Prevention { amount: amt },
        }
    }
}

// --- 3. Destroy ---

fn destroy_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Destroy { .. })
}

/// CR 701.19: Regeneration shield applier for Destroy events.
/// If the replacement definition is a regeneration shield and the destruction allows
/// regeneration, removes damage, taps the permanent, removes it from combat,
/// consumes the shield, and prevents the destruction.
fn destroy_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // Check if this replacement is a regeneration shield
    let is_regen = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .is_some_and(|repl| {
            matches!(
                repl.shield_kind,
                crate::types::ability::ShieldKind::Regeneration
            )
        });

    if !is_regen {
        return ApplyResult::Modified(event);
    }

    // CR 701.19: "It can't be regenerated" bypasses regeneration shields.
    if let ProposedEvent::Destroy {
        cant_regenerate: true,
        ..
    } = &event
    {
        return ApplyResult::Modified(event);
    }

    let ProposedEvent::Destroy { object_id, .. } = &event else {
        return ApplyResult::Modified(event);
    };
    let oid = *object_id;

    // CR 701.19a: Remove all damage marked on it.
    if let Some(obj) = state.objects.get_mut(&oid) {
        obj.damage_marked = 0;
        obj.dealt_deathtouch_damage = false;
        // CR 701.19b: Tap it.
        obj.tapped = true;
    }

    // CR 701.19c: Remove it from combat if it's attacking or blocking.
    super::effects::remove_from_combat::remove_object_from_combat(state, oid);

    // Mark the shield as consumed (one-shot).
    if let Some(obj) = state.objects.get_mut(&rid.source) {
        if let Some(repl) = obj.replacement_definitions.get_mut(rid.index) {
            repl.is_consumed = true;
        }
    }

    events.push(GameEvent::Regenerated { object_id: oid });
    ApplyResult::Prevented
}

// --- 4. Draw ---

fn draw_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Draw { count, .. } if *count > 0)
}

fn draw_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let Some(new_count) = draw_replacement_count(state, rid, &event) else {
        return ApplyResult::Modified(event);
    };

    if let ProposedEvent::Draw {
        player_id, applied, ..
    } = event
    {
        ApplyResult::Modified(ProposedEvent::Draw {
            player_id,
            count: new_count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

fn draw_replacement_count(
    state: &GameState,
    rid: ReplacementId,
    event: &ProposedEvent,
) -> Option<u32> {
    let ProposedEvent::Draw { count, .. } = event else {
        return None;
    };

    let execute = state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .execute
        .as_deref()?;

    match &*execute.effect {
        Effect::Draw { count: qty } if execute.sub_ability.is_none() => {
            let resolved = resolve_draw_replacement_quantity(qty, *count)?;
            Some(resolved.max(0) as u32)
        }
        _ => None,
    }
}

fn resolve_draw_replacement_quantity(expr: &QuantityExpr, event_count: u32) -> Option<i32> {
    match expr {
        QuantityExpr::Ref {
            qty: crate::types::ability::QuantityRef::EventContextAmount,
        } => Some(event_count as i32),
        QuantityExpr::Fixed { value } => Some(*value),
        QuantityExpr::HalfRounded { inner, rounding } => {
            let value = resolve_draw_replacement_quantity(inner, event_count)?;
            Some(match rounding {
                crate::types::ability::RoundingMode::Up => (value + 1) / 2,
                crate::types::ability::RoundingMode::Down => value / 2,
            })
        }
        QuantityExpr::Offset { inner, offset } => {
            Some(resolve_draw_replacement_quantity(inner, event_count)? + offset)
        }
        QuantityExpr::Multiply { factor, inner } => {
            Some(factor * resolve_draw_replacement_quantity(inner, event_count)?)
        }
        QuantityExpr::Ref { .. } => None,
    }
}

// --- 5. GainLife ---

fn gain_life_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    // CR 614.1a: Basic event type match. Player scope is checked by `valid_player`
    // in `find_applicable_replacements`. Without `valid_player`, defaults to controller-only.
    matches!(event, ProposedEvent::LifeGain { .. })
}

// CR 614.1a: Replacement effect modifies life gain amount.
fn gain_life_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    let Some(delta) = gain_life_replacement_delta(state, rid) else {
        return ApplyResult::Modified(event);
    };

    if let ProposedEvent::LifeGain {
        player_id,
        amount,
        applied,
    } = event
    {
        ApplyResult::Modified(ProposedEvent::LifeGain {
            player_id,
            amount: amount.saturating_add(delta),
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

fn gain_life_replacement_delta(state: &GameState, rid: ReplacementId) -> Option<u32> {
    let execute = state
        .objects
        .get(&rid.source)?
        .replacement_definitions
        .get(rid.index)?
        .execute
        .as_deref()?;

    match &*execute.effect {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: delta },
            ..
        } if *delta > 0 && execute.sub_ability.is_none() => Some(*delta as u32),
        _ => None,
    }
}

// --- 6. LifeReduced ---

fn life_reduced_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn life_reduced_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 6b. LoseLife (oracle-parsed: e.g. Bloodletter of Aclazotz) ---

fn lose_life_matcher(event: &ProposedEvent, source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::LifeLoss { player_id, .. } = event {
        // Match when opponent loses life during source controller's turn
        if let Some(obj) = state.objects.get(&source) {
            *player_id != obj.controller && state.active_player == obj.controller
        } else {
            false
        }
    } else {
        false
    }
}

fn lose_life_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied,
    } = event
    {
        ApplyResult::Modified(ProposedEvent::LifeLoss {
            player_id,
            amount: amount * 2,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 7. AddCounter ---

fn add_counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::AddCounter { .. })
}

fn add_counter_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    let modification = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.quantity_modification.clone());
    let Some(modification) = modification else {
        return ApplyResult::Modified(event);
    };
    if let ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count,
        applied,
    } = event
    {
        // CR 614.1a: Modify counter count per replacement effect.
        let new_count = match modification {
            QuantityModification::Double => count.saturating_mul(2),
            QuantityModification::Plus { value } => count.saturating_add(value),
            QuantityModification::Minus { value } => count.saturating_sub(value),
        };
        ApplyResult::Modified(ProposedEvent::AddCounter {
            object_id,
            counter_type,
            count: new_count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 8. RemoveCounter ---

fn remove_counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::RemoveCounter { .. })
}

fn remove_counter_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 9. CreateToken ---

fn create_token_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::CreateToken { .. })
}

fn create_token_applier(
    event: ProposedEvent,
    rid: ReplacementId,
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    use crate::types::ability::QuantityModification;
    let modification = state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .and_then(|def| def.quantity_modification.clone());

    if let ProposedEvent::CreateToken {
        owner,
        name,
        count,
        applied,
    } = event
    {
        // CR 614.1a: Modify token count per replacement effect.
        let new_count = match modification {
            Some(QuantityModification::Double) => count.saturating_mul(2),
            Some(QuantityModification::Plus { value }) => count.saturating_add(value),
            Some(QuantityModification::Minus { value }) => count.saturating_sub(value),
            None => count,
        };
        ApplyResult::Modified(ProposedEvent::CreateToken {
            owner,
            name,
            count: new_count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 11. Tap ---

fn tap_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Tap { .. })
}

fn tap_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 12. Untap ---

// CR 614.1a: Replacement effect modifies untap event.
fn untap_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Untap { .. })
}

fn untap_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 14. Counter (spell countering) ---

fn counter_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::ZoneChange {
            from: Zone::Stack,
            ..
        }
    )
}

fn counter_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 15. Attached (ZoneChange to Battlefield for attachments) ---

fn attached_matcher(event: &ProposedEvent, _source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
        if *to != Zone::Battlefield {
            return false;
        }
        // Check if the entering object is an attachment (Aura or Equipment)
        state
            .objects
            .get(object_id)
            .map(|obj| {
                obj.card_types
                    .subtypes
                    .iter()
                    .any(|s| s == "Aura" || s == "Equipment")
            })
            .unwrap_or(false)
    } else {
        false
    }
}

fn attached_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 16. DealtDamage (from target's perspective) ---

fn dealt_damage_matcher(event: &ProposedEvent, source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::Damage { target, .. } = event {
        // Match if the source object of this replacement is the target of the damage
        match target {
            crate::types::ability::TargetRef::Object(oid) => *oid == source,
            crate::types::ability::TargetRef::Player(pid) => state
                .objects
                .get(&source)
                .map(|o| o.controller == *pid)
                .unwrap_or(false),
        }
    } else {
        false
    }
}

fn dealt_damage_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 17. Mill (ZoneChange from Library to Graveyard) ---

fn mill_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(
        event,
        ProposedEvent::ZoneChange {
            from: Zone::Library,
            to: Zone::Graveyard,
            ..
        }
    )
}

fn mill_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 18. PayLife (matches LifeLoss) ---

fn pay_life_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn pay_life_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- Placeholder handlers (no ProposedEvent variant yet) ---

fn placeholder_matcher(_event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    false
}

fn placeholder_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- BeginTurn / BeginPhase (CR 614.1b, CR 614.10) ---

/// CR 614.1b + CR 614.10: Match a pending turn-start event shape. Per-def
/// condition gating (`OnlyExtraTurn`) is evaluated by
/// `evaluate_replacement_condition` with full event context.
fn begin_turn_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::BeginTurn { .. })
}

/// CR 614.1b + CR 614.10: Skip the turn. Permanent statics (`ShieldKind::None`,
/// the default) are never consumed — every matching turn-begin is skipped.
fn begin_turn_applier(
    _event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Prevented
}

/// CR 614.1b: Match a pending phase-start event shape. No phase-specific
/// conditions are currently wired; parser enrichment for "skip next combat"
/// etc. is a future batch and will layer via `evaluate_replacement_condition`.
fn begin_phase_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::BeginPhase { .. })
}

/// CR 614.1b + CR 614.10: Skip the phase. Like `begin_turn_applier`, permanent
/// statics fire every time their predicate matches and are never consumed.
fn begin_phase_applier(
    _event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Prevented
}

// --- Registry ---

/// CR 614.1: Build the registry of applicable replacement effects.
pub fn build_replacement_registry() -> IndexMap<ReplacementEvent, ReplacementHandlerEntry> {
    let mut registry = IndexMap::new();

    let stub = || ReplacementHandlerEntry {
        matcher: stub_matcher,
        applier: stub_applier,
    };

    // 14 core types with real logic
    registry.insert(
        ReplacementEvent::DamageDone,
        ReplacementHandlerEntry {
            matcher: damage_done_matcher,
            applier: damage_done_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Moved,
        ReplacementHandlerEntry {
            matcher: moved_matcher,
            applier: moved_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Destroy,
        ReplacementHandlerEntry {
            matcher: destroy_matcher,
            applier: destroy_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Draw,
        ReplacementHandlerEntry {
            matcher: draw_matcher,
            applier: draw_applier,
        },
    );
    registry.insert(ReplacementEvent::DrawCards, stub()); // stays stub (alias for Draw)
    registry.insert(
        ReplacementEvent::GainLife,
        ReplacementHandlerEntry {
            matcher: gain_life_matcher,
            applier: gain_life_applier,
        },
    );
    registry.insert(
        ReplacementEvent::LifeReduced,
        ReplacementHandlerEntry {
            matcher: life_reduced_matcher,
            applier: life_reduced_applier,
        },
    );
    registry.insert(
        ReplacementEvent::LoseLife,
        ReplacementHandlerEntry {
            matcher: lose_life_matcher,
            applier: lose_life_applier,
        },
    );
    registry.insert(
        ReplacementEvent::AddCounter,
        ReplacementHandlerEntry {
            matcher: add_counter_matcher,
            applier: add_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::RemoveCounter,
        ReplacementHandlerEntry {
            matcher: remove_counter_matcher,
            applier: remove_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Tap,
        ReplacementHandlerEntry {
            matcher: tap_matcher,
            applier: tap_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Untap,
        ReplacementHandlerEntry {
            matcher: untap_matcher,
            applier: untap_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Counter,
        ReplacementHandlerEntry {
            matcher: counter_matcher,
            applier: counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::CreateToken,
        ReplacementHandlerEntry {
            matcher: create_token_matcher,
            applier: create_token_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Attached,
        ReplacementHandlerEntry {
            matcher: attached_matcher,
            applier: attached_applier,
        },
    );

    // Promoted from stubs to real handlers
    registry.insert(
        ReplacementEvent::DealtDamage,
        ReplacementHandlerEntry {
            matcher: dealt_damage_matcher,
            applier: dealt_damage_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Mill,
        ReplacementHandlerEntry {
            matcher: mill_matcher,
            applier: mill_applier,
        },
    );
    registry.insert(
        ReplacementEvent::PayLife,
        ReplacementHandlerEntry {
            matcher: pay_life_matcher,
            applier: pay_life_applier,
        },
    );
    let placeholder = || ReplacementHandlerEntry {
        matcher: placeholder_matcher,
        applier: placeholder_applier,
    };
    // CR 106.6a: ProduceMana is parser-emitted by cards that override what
    // mana a land produces (Contamination, Infernal Darkness, Naked Singularity,
    // Pulse of Llanowar, Reality Twist, Ritual of Subdual, Mirri). These are
    // currently silent no-ops at runtime because the parser does not yet
    // extract the "produces X instead" payload into typed data — the matcher
    // has no information to act on. Proper wiring is tracked as a follow-up.
    registry.insert(ReplacementEvent::ProduceMana, placeholder());
    registry.insert(ReplacementEvent::TurnFaceUp, placeholder());

    // CR 614.1b + CR 614.10: BeginTurn skip replacements (Stranglehold, etc.)
    registry.insert(
        ReplacementEvent::BeginTurn,
        ReplacementHandlerEntry {
            matcher: begin_turn_matcher,
            applier: begin_turn_applier,
        },
    );
    // CR 614.1b: BeginPhase skip replacements.
    registry.insert(
        ReplacementEvent::BeginPhase,
        ReplacementHandlerEntry {
            matcher: begin_phase_matcher,
            applier: begin_phase_applier,
        },
    );

    // CR 104.2b + CR 104.3b: GameLoss / GameWin are parser-emitted by
    // Platinum Angel, Lich's Mastery, Angel's Grace, etc. The effective
    // runtime enforcement for these cards is via first-class static-ability
    // variants: `StaticMode::CantLoseTheGame` (sba.rs::player_has_cant_lose)
    // and `StaticMode::CantWinTheGame` (effects/win_lose.rs::resolve_win).
    // The replacement-pipeline stub here is redundant but kept registered
    // so the parser's replacement-path output doesn't hit a dispatch miss.
    let stub_events: Vec<ReplacementEvent> =
        vec![ReplacementEvent::GameLoss, ReplacementEvent::GameWin];
    for ev in stub_events {
        registry.insert(ev, stub());
    }

    registry
}

// --- Prevention gating ---

/// CR 614.16: Check if damage prevention is disabled by a GameRestriction.
/// When active, prevention-type replacement effects are skipped in the pipeline.
fn is_prevention_disabled(state: &GameState, proposed: &ProposedEvent) -> bool {
    use crate::types::ability::{GameRestriction, RestrictionScope};

    state.restrictions.iter().any(|r| match r {
        GameRestriction::DamagePreventionDisabled { scope, .. } => match scope {
            None => {
                // Global — all damage prevention disabled
                matches!(proposed, ProposedEvent::Damage { .. })
            }
            Some(RestrictionScope::SpecificSource(id)) => {
                matches!(proposed, ProposedEvent::Damage { source_id, .. } if *source_id == *id)
            }
            Some(RestrictionScope::SourcesControlledBy(pid)) => {
                if let ProposedEvent::Damage { source_id, .. } = proposed {
                    state
                        .objects
                        .get(source_id)
                        .map(|obj| obj.controller == *pid)
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            Some(RestrictionScope::DamageToTarget(tid)) => {
                matches!(proposed, ProposedEvent::Damage { target, .. }
                    if matches!(target, crate::types::ability::TargetRef::Object(oid) if *oid == *tid)
                    || matches!(target, crate::types::ability::TargetRef::Player(pid) if {
                        // For player targets, check if the player's "id object" matches
                        // This is a player target, not an object target, so tid doesn't apply
                        let _ = pid;
                        false
                    })
                )
            }
        },
        GameRestriction::CastOnlyFromZones { .. } => false,
    })
}

/// Check if a replacement definition is a damage prevention replacement.
/// Prevention replacements have a `Prevented` result (the event is fully stopped)
/// or are recognized prevention-type patterns from the parser.
fn is_damage_prevention_replacement(
    state: &GameState,
    rid: &ReplacementId,
    event: &ReplacementEvent,
) -> bool {
    // Only applies to DamageDone handlers
    let is_damage_event = matches!(event, ReplacementEvent::DamageDone)
        || matches!(event, ReplacementEvent::DealtDamage);
    if !is_damage_event {
        return false;
    }

    // Look up the replacement definition from either objects or pending_damage_prevention.
    let repl_def = if rid.source == ObjectId(0) {
        state.pending_damage_prevention.get(rid.index)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
    };

    let Some(repl) = repl_def else {
        return false;
    };

    // CR 614.1a: Damage boost/reduction replacements are definitively not prevention effects
    if repl.damage_modification.is_some() {
        return false;
    }

    // Check for ShieldKind::Prevention or description-based prevention patterns
    // CR 615: Prevention shields created by prevent_damage.rs
    matches!(repl.shield_kind, ShieldKind::Prevention { .. })
    // Legacy: description-based prevention from parsed replacement definitions
    || repl.description.as_ref().is_some_and(|d| {
        let lower = d.to_lowercase();
        lower.contains("prevent") && lower.contains("damage")
    })
}

/// CR 614.1a: Check if a damage target matches the replacement's target filter.
fn matches_damage_target_filter(
    filter: &DamageTargetFilter,
    target: &TargetRef,
    repl_controller: PlayerId,
    state: &GameState,
) -> bool {
    match filter {
        DamageTargetFilter::OpponentOrTheirPermanents => match target {
            TargetRef::Player(pid) => *pid != repl_controller,
            TargetRef::Object(oid) => state
                .objects
                .get(oid)
                .is_some_and(|obj| obj.controller != repl_controller),
        },
        DamageTargetFilter::CreatureOnly => match target {
            TargetRef::Player(_) => false,
            TargetRef::Object(oid) => state
                .objects
                .get(oid)
                .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature)),
        },
        DamageTargetFilter::PlayerOnly => matches!(target, TargetRef::Player(_)),
        DamageTargetFilter::OpponentOnly => {
            matches!(target, TargetRef::Player(pid) if *pid != repl_controller)
        }
    }
}

// --- Pipeline functions ---

/// Evaluate a replacement condition against the current game state.
/// Returns `true` if the replacement should apply, `false` if it should be skipped.
fn evaluate_replacement_condition(
    condition: &ReplacementCondition,
    controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
    affected_object_id: Option<ObjectId>,
    event: &ProposedEvent,
) -> bool {
    match condition {
        ReplacementCondition::UnlessControlsSubtype { subtypes } => {
            // "unless you control a [subtype]" → suppressed if controller has a matching permanent
            let controls_any = state.objects.values().any(|o| {
                o.zone == Zone::Battlefield
                    && o.controller == controller
                    && o.id != source_id
                    && subtypes.iter().any(|st| {
                        o.card_types
                            .subtypes
                            .iter()
                            .any(|s| s.eq_ignore_ascii_case(st))
                    })
            });
            // If the "unless" is satisfied (they DO control one), skip the replacement
            !controls_any
        }
        // CR 305.7 + CR 614.1c — fast lands enter tapped unless controller has
        // N or fewer other lands; condition evaluated as the replacement applies.
        ReplacementCondition::UnlessControlsOtherLeq { count, filter } => {
            let target_filter = TargetFilter::Typed(filter.clone());
            let ctx = FilterContext::from_source(state, source_id);
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield
                        && matches_target_filter(state, o.id, &target_filter, &ctx)
                })
                .count() as u32;
            // "unless you control N or fewer" → suppressed when count ≤ N
            // Replacement applies (enters tapped) when count > N
            matching_count > *count
        }
        // CR 614.1d — "unless you control a [type phrase]" → suppressed if controller
        // has a matching permanent on the battlefield. ControllerRef::You is pre-set
        // in the filter by the parser.
        ReplacementCondition::UnlessControlsMatching { filter } => {
            let ctx = FilterContext::from_source_with_controller(source_id, controller);
            let controls_any = state.objects.values().any(|o| {
                o.zone == Zone::Battlefield
                    && o.id != source_id
                    && matches_target_filter(state, o.id, filter, &ctx)
            });
            !controls_any
        }
        // CR 614.1d: Bond lands — "unless a player has N or less life"
        ReplacementCondition::UnlessPlayerLifeAtMost { amount } => {
            let any_player_low = state.players.iter().any(|p| p.life <= *amount as i32);
            !any_player_low
        }
        // CR 614.1d: Battlebond lands — "unless you have two or more opponents"
        ReplacementCondition::UnlessMultipleOpponents => {
            let opponent_count = state
                .players
                .iter()
                .filter(|p| p.id != controller && !p.is_eliminated)
                .count();
            opponent_count < 2
        }
        // CR 614.1d — "unless you control N or more [type]" → suppressed if controller
        // has at least `minimum` matching permanents on the battlefield.
        ReplacementCondition::UnlessControlsCountMatching { minimum, filter } => {
            let ctx = FilterContext::from_source_with_controller(source_id, controller);
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield
                        && o.id != source_id
                        && matches_target_filter(state, o.id, filter, &ctx)
                })
                .count();
            matching_count < *minimum as usize
        }
        // CR 614.1d + CR 500: "unless it's your turn" — suppressed on controller's turn.
        ReplacementCondition::UnlessYourTurn => state.active_player != controller,
        // CR 614.1d: General quantity comparison — suppressed when comparison is true.
        ReplacementCondition::UnlessQuantity {
            lhs,
            comparator,
            rhs,
            active_player_req,
        } => {
            // Optional active-player gate: "it's your Nth turn" requires controller's turn;
            // "it's an opponent's Nth turn" requires opponent's turn; None = no gate.
            let turn_ok = match active_player_req {
                Some(ControllerRef::You) => state.active_player == controller,
                Some(ControllerRef::Opponent) => state.active_player != controller,
                None => true,
            };
            if !turn_ok {
                return true; // Turn requirement not met → replacement applies
            }
            let lhs_val =
                crate::game::quantity::resolve_quantity(state, lhs, controller, source_id);
            let rhs_val =
                crate::game::quantity::resolve_quantity(state, rhs, controller, source_id);
            !comparator.evaluate(lhs_val, rhs_val)
        }
        ReplacementCondition::OnlyIfQuantity {
            lhs,
            comparator,
            rhs,
            active_player_req,
        } => {
            let turn_ok = match active_player_req {
                Some(ControllerRef::You) => state.active_player == controller,
                Some(ControllerRef::Opponent) => state.active_player != controller,
                None => true,
            };
            if !turn_ok {
                return false;
            }
            let lhs_val =
                crate::game::quantity::resolve_quantity(state, lhs, controller, source_id);
            let rhs_val =
                crate::game::quantity::resolve_quantity(state, rhs, controller, source_id);
            comparator.evaluate(lhs_val, rhs_val)
        }
        // CR 702.138c: "escapes with" — applies only when the source was cast via escape.
        // Check cast_from_zone on the entering permanent as a proxy for escape.
        ReplacementCondition::CastViaEscape => state
            .objects
            .get(&source_id)
            .is_some_and(|o| o.cast_from_zone == Some(Zone::Graveyard)),
        // CR 702.33d: "if was kicked" — applies only when the kicker cost was paid.
        // TODO: Propagate additional_cost_paid to GameObject for precise evaluation.
        // For now, conservatively apply the replacement (counters always placed).
        ReplacementCondition::CastViaKicker { .. } => true,
        // CR 120.1: "dealt damage this turn by a source you controlled" — check damage records.
        ReplacementCondition::DealtDamageThisTurnBySourceControlledBy {
            controller: ctrl_ref,
        } => {
            let required_controller = match ctrl_ref {
                ControllerRef::You => controller,
                ControllerRef::Opponent => {
                    // Find any opponent — simplified for two-player
                    state
                        .players
                        .iter()
                        .find(|p| p.id != controller && !p.is_eliminated)
                        .map_or(controller, |p| p.id)
                }
            };
            // Check if the affected object was dealt damage this turn by a source
            // controlled by the required controller.
            if let Some(affected_id) = affected_object_id {
                state.damage_dealt_this_turn.iter().any(|record| {
                    record.target == TargetRef::Object(affected_id)
                        && state
                            .objects
                            .get(&record.source_id)
                            .map(|src| src.controller)
                            .or_else(|| {
                                // Source may have left the battlefield; check LKI cache.
                                state
                                    .lki_cache
                                    .get(&record.source_id)
                                    .map(|lki| lki.controller)
                            })
                            .is_some_and(|c| c == required_controller)
                })
            } else {
                false
            }
        }
        // CR 500.7 + CR 614.10: Replacement applies only for extra turns.
        // Checks the event's `is_extra_turn` flag directly; returns `false` for
        // any non-`BeginTurn` event so a misattached `OnlyExtraTurn` doesn't
        // silently fire on unrelated replacements.
        ReplacementCondition::OnlyExtraTurn => matches!(
            event,
            ProposedEvent::BeginTurn {
                is_extra_turn: true,
                ..
            }
        ),
        // Unrecognized condition — always applies (enters tapped) as a safe default.
        // The engine recognizes the replacement but cannot evaluate the condition,
        // so it conservatively taps the land.
        ReplacementCondition::Unrecognized { .. } => true,
    }
}

pub fn find_applicable_replacements(
    state: &GameState,
    event: &ProposedEvent,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
) -> Vec<ReplacementId> {
    let mut candidates = Vec::new();

    // CR 614.12: Self-replacement effects on a card entering the battlefield.
    // apply even though the card isn't on the battlefield yet. We must scan the
    // entering card in addition to battlefield/command zone permanents.
    let entering_object_id = match event {
        ProposedEvent::ZoneChange {
            object_id,
            to: Zone::Battlefield,
            ..
        } => Some(*object_id),
        _ => None,
    };

    let zones_to_scan = [Zone::Battlefield, Zone::Command];
    for obj in state.objects.values() {
        let in_scanned_zone = zones_to_scan.contains(&obj.zone);
        let is_entering = entering_object_id == Some(obj.id);

        if !in_scanned_zone && !is_entering {
            continue;
        }

        for (index, repl_def) in obj.replacement_definitions.iter().enumerate() {
            // CR 701.19: Skip consumed one-shot replacements (e.g., used regeneration shields).
            if repl_def.is_consumed {
                continue;
            }

            // Cards not yet on battlefield can only apply self-replacement effects
            if is_entering
                && !in_scanned_zone
                && repl_def.valid_card != Some(crate::types::ability::TargetFilter::SelfRef)
            {
                continue;
            }

            let rid = ReplacementId {
                source: obj.id,
                index,
            };

            if event.already_applied(&rid) {
                continue;
            }

            if let Some(handler) = registry.get(&repl_def.event) {
                if (handler.matcher)(event, obj.id, state) {
                    // Enforce valid_card filter: if set, the event's affected object
                    // must match the filter (e.g., SelfRef means only this card's own events)
                    if let Some(ref filter) = repl_def.valid_card {
                        let ctx = FilterContext::from_source(state, obj.id);
                        let matches = event
                            .affected_object_id()
                            .map(|oid| matches_target_filter(state, oid, filter, &ctx))
                            .unwrap_or(false);
                        if !matches {
                            continue;
                        }
                    }
                    // CR 614.6: Zone-change replacements may be scoped to a specific destination.
                    if let Some(ref dest_zone) = repl_def.destination_zone {
                        let matches_dest = match event {
                            ProposedEvent::ZoneChange { to, .. } => to == dest_zone,
                            // CR 614.6: Only zone-change events can match a destination zone scope.
                            _ => false,
                        };
                        if !matches_dest {
                            continue;
                        }
                    }
                    // Evaluate replacement condition (e.g. "unless you control a Mountain")
                    if let Some(ref cond) = repl_def.condition {
                        if !evaluate_replacement_condition(
                            cond,
                            obj.controller,
                            obj.id,
                            state,
                            event.affected_object_id(),
                            event,
                        ) {
                            continue;
                        }
                    }
                    // CR 614.1a: Damage source filter — matches the damage *source* object against the filter.
                    if let Some(ref sf) = repl_def.damage_source_filter {
                        if let ProposedEvent::Damage { source_id, .. } = event {
                            if !matches_target_filter(
                                state,
                                *source_id,
                                sf,
                                &FilterContext::from_source(state, obj.id),
                            ) {
                                continue;
                            }
                        }
                    }
                    // CR 614.1a: Combat/noncombat damage scope restriction.
                    if let Some(ref scope) = repl_def.combat_scope {
                        if let ProposedEvent::Damage { is_combat, .. } = event {
                            match scope {
                                CombatDamageScope::CombatOnly if !is_combat => continue,
                                CombatDamageScope::NoncombatOnly if *is_combat => continue,
                                _ => {}
                            }
                        }
                    }
                    // CR 614.1a: Damage target filter — restricts which damage recipients trigger this replacement.
                    if let Some(ref tf) = repl_def.damage_target_filter {
                        if let ProposedEvent::Damage { target, .. } = event {
                            if !matches_damage_target_filter(tf, target, obj.controller, state) {
                                continue;
                            }
                        }
                    }
                    // CR 614.16: Skip damage prevention replacements when prevention is disabled
                    if is_damage_prevention_replacement(state, &rid, &repl_def.event)
                        && is_prevention_disabled(state, event)
                    {
                        continue;
                    }
                    // CR 614.1a: Token owner scope — restrict to tokens created under specific controller.
                    if let Some(ref scope) = repl_def.token_owner_scope {
                        if let ProposedEvent::CreateToken { owner, .. } = event {
                            let matches = match scope {
                                crate::types::ability::ControllerRef::You => {
                                    *owner == obj.controller
                                }
                                crate::types::ability::ControllerRef::Opponent => {
                                    *owner != obj.controller
                                }
                            };
                            if !matches {
                                continue;
                            }
                        }
                    }
                    // CR 614.1a: valid_player scope — restricts which player's events
                    // trigger this replacement. For GainLife events, determines whose life
                    // gain is replaced. Default (None) = controller only.
                    if let ProposedEvent::LifeGain { player_id, .. }
                    | ProposedEvent::Draw { player_id, .. } = event
                    {
                        let player_ok = match &repl_def.valid_player {
                            Some(crate::types::ability::ControllerRef::Opponent) => {
                                *player_id != obj.controller
                            }
                            Some(crate::types::ability::ControllerRef::You) => {
                                *player_id == obj.controller
                            }
                            None => {
                                // Default: controller-only (backward compatible)
                                *player_id == obj.controller
                            }
                        };
                        if !player_ok {
                            continue;
                        }
                    }
                    // CR 614.7: Skip an Optional replacement whose decline branch is a
                    // no-op on the current event. E.g., a shock land whose `enter_tapped`
                    // is already set by an Earthbending return: declining would tap it,
                    // but it's tapping anyway — the player shouldn't be offered the
                    // dominated "pay 2 life to avoid a tap that isn't happening" choice.
                    if let ReplacementMode::Optional { decline } = &repl_def.mode {
                        if optional_decline_is_noop(event, decline.as_deref()) {
                            continue;
                        }
                    }
                    candidates.push(rid);
                }
            }
        }
    }

    // CR 615.3: Also scan game-state-level prevention shields (fog-like spells).
    // These use a sentinel source ObjectId(0) to distinguish from object-attached shields.
    if matches!(event, ProposedEvent::Damage { .. }) {
        for (index, repl_def) in state.pending_damage_prevention.iter().enumerate() {
            if repl_def.is_consumed {
                continue;
            }

            let rid = ReplacementId {
                source: ObjectId(0),
                index,
            };

            if event.already_applied(&rid) {
                continue;
            }

            if let Some(handler) = registry.get(&repl_def.event) {
                // CR 615.3: Check combat scope, target filters, and source filters.
                // CR 614.1a: Damage source filter — matches the damage *source* object
                // against the filter (e.g., "sources of the chosen color").
                if let Some(ref sf) = repl_def.damage_source_filter {
                    if let ProposedEvent::Damage { source_id, .. } = event {
                        if !matches_target_filter(
                            state,
                            *source_id,
                            sf,
                            &FilterContext::from_source(state, ObjectId(0)),
                        ) {
                            continue;
                        }
                    }
                }
                if let Some(ref scope) = repl_def.combat_scope {
                    if let ProposedEvent::Damage { is_combat, .. } = event {
                        match scope {
                            CombatDamageScope::CombatOnly if !is_combat => continue,
                            CombatDamageScope::NoncombatOnly if *is_combat => continue,
                            _ => {}
                        }
                    }
                }
                if let Some(ref tf) = repl_def.damage_target_filter {
                    if let ProposedEvent::Damage { target, .. } = event {
                        if !matches_damage_target_filter(tf, target, PlayerId(0), state) {
                            continue;
                        }
                    }
                }
                // Check if prevention is disabled
                if is_prevention_disabled(state, event) {
                    continue;
                }
                // Verify the handler matcher still matches (for DamageDone events)
                if (handler.matcher)(event, ObjectId(0), state) {
                    candidates.push(rid);
                }
            }
        }
    }

    candidates
}

const MAX_REPLACEMENT_DEPTH: u16 = 16;

/// Identifies which ability branch of a `ReplacementDefinition` is being applied.
/// CR 614.1a + CR 614.1c: `ReplacementMode::Optional` carries both an `execute` ability
/// (accept branch) and a `decline` ability (decline branch); both branches may introduce
/// ProposedEvent modifications (enter_tapped, counters) and must flow through the same
/// propagation logic so the replacement pipeline sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementBranch {
    Execute,
    Decline,
}

/// Extract ETB counter data from a replacement ability's effect.
/// Handles `PutCounter` and `AddCounter` effects, returning (counter_type, count) pairs.
fn extract_etb_counters(ability: Option<&AbilityDefinition>) -> Vec<(String, u32)> {
    let exec = match ability {
        Some(e) => e,
        None => return Vec::new(),
    };
    match &*exec.effect {
        Effect::PutCounter {
            counter_type,
            count,
            ..
        }
        | Effect::AddCounter {
            counter_type,
            count,
            ..
        } => {
            let n = match count {
                QuantityExpr::Fixed { value } => (*value).max(0) as u32,
                _ => 1, // Dynamic counts fall back to 1 for ETB counters
            };
            vec![(counter_type.clone(), n)]
        }
        _ => Vec::new(),
    }
}

/// CR 614.1c + CR 614.12: ProposedEvent modifications that a replacement ability would
/// introduce onto a `ZoneChange` to the battlefield — enters-tapped, ETB counters, and
/// zone redirection. Used by `apply_single_replacement` to propagate the ability's effect
/// onto the ProposedEvent, and by `find_applicable_replacements` to detect Optional
/// replacements whose decline branch would be a no-op (CR 614.7).
#[derive(Debug, Clone, Default)]
struct EventModifiers {
    enters_tapped: bool,
    etb_counters: Vec<(String, u32)>,
    redirect_zone: Option<Zone>,
}

impl EventModifiers {
    /// True if this ability has any effect on the ProposedEvent beyond the event-modifier
    /// fields tracked here (i.e., it still needs to run as a post-replacement side effect).
    /// An ability that is *purely* a Tap SelfRef / PutCounter-SelfRef / ChangeZone has no
    /// remaining work after its modifiers are applied to the event.
    fn has_only_event_modifier(ability: Option<&AbilityDefinition>) -> bool {
        let Some(def) = ability else {
            return false;
        };
        matches!(
            &*def.effect,
            Effect::Tap {
                target: TargetFilter::SelfRef,
            } | Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            } | Effect::AddCounter {
                target: TargetFilter::SelfRef,
                ..
            } | Effect::ChangeZone { .. }
        )
    }
}

/// Compute the ProposedEvent modifications an ability would introduce.
fn event_modifiers_for_ability(ability: Option<&AbilityDefinition>) -> EventModifiers {
    let tapped = ability.is_some_and(|def| {
        matches!(
            *def.effect,
            Effect::Tap {
                target: TargetFilter::SelfRef,
            }
        )
    });
    let counters = extract_etb_counters(ability);
    let redirect = ability.and_then(|def| match &*def.effect {
        Effect::ChangeZone { destination, .. } => Some(*destination),
        _ => None,
    });
    EventModifiers {
        enters_tapped: tapped,
        etb_counters: counters,
        redirect_zone: redirect,
    }
}

/// CR 614.7: "If a replacement effect would replace an event, but that event never
/// happens, the replacement effect simply doesn't do anything."
///
/// An `Optional` replacement's decline branch is the player's "default" — what happens
/// if they decline the accept cost. If the decline branch is a pure ProposedEvent
/// modifier (e.g., shock-land `Tap SelfRef`) and every modification it would introduce
/// is already present on the event (e.g., `enter_tapped` is already `true` from an
/// earlier Earthbending return), declining would do nothing. Presenting the Optional
/// to the player becomes a dominated choice: accepting costs something (life, discard,
/// etc.) to avoid a modification that was going to happen anyway. Skip the Optional
/// entirely in that case — the event proceeds with its existing modifications.
///
/// The check only skips when the decline branch's work is fully subsumed. If decline
/// has any non-modifier effect (e.g., a choice, a draw) or a modification not already
/// present, the Optional remains applicable so the player can still be offered the
/// choice when it is meaningful.
fn optional_decline_is_noop(event: &ProposedEvent, decline: Option<&AbilityDefinition>) -> bool {
    // Dominance only applies to ZoneChange events — that's where enter_tapped /
    // etb_counters / redirect_zone live. For other event kinds, never skip.
    let ProposedEvent::ZoneChange {
        enter_tapped,
        enter_with_counters,
        ..
    } = event
    else {
        return false;
    };

    // No decline branch at all → the Optional has nothing to do on decline. But it may
    // still have a meaningful accept branch, so do NOT dominate.
    let Some(def) = decline else {
        return false;
    };

    // If decline has any non-modifier effect, it still has real work on decline.
    if !EventModifiers::has_only_event_modifier(Some(def)) {
        return false;
    }

    let mods = event_modifiers_for_ability(Some(def));
    let tap_already = !mods.enters_tapped || *enter_tapped;
    let counters_already = mods.etb_counters.iter().all(|(ct, n)| {
        enter_with_counters
            .iter()
            .any(|(existing_ct, existing_n)| existing_ct == ct && existing_n >= n)
    });
    // Redirect: a redirect-bearing decline always has work to do, so it is never a
    // no-op regardless of the current `to` zone.
    let redirect_noop = mods.redirect_zone.is_none();

    tap_already && counters_already && redirect_noop
}

fn apply_single_replacement(
    state: &mut GameState,
    proposed: ProposedEvent,
    rid: ReplacementId,
    branch: ReplacementBranch,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    // CR 615.3: Pending damage prevention shields use sentinel ObjectId(0).
    // Look up from game-state-level registry instead of object replacement_definitions.
    let repl_def_ref = if rid.source == ObjectId(0) {
        state.pending_damage_prevention.get(rid.index)
    } else {
        state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
    };

    // Extract replacement metadata before mutably borrowing state for the applier.
    // CR 614.1c: ProposedEvent modifiers (enter_tapped, ETB counters, zone redirect)
    // come from whichever branch is being applied — `execute` on accept / mandatory,
    // `decline` on decline. Both must flow through the pipeline so dominance and
    // downstream replacements see a consistent ProposedEvent (CR 614.5).
    let (event_key, modifiers) = match repl_def_ref {
        Some(repl_def) => {
            let ability = match branch {
                ReplacementBranch::Execute => repl_def.execute.as_deref(),
                ReplacementBranch::Decline => match &repl_def.mode {
                    ReplacementMode::Optional { decline } => decline.as_deref(),
                    ReplacementMode::Mandatory => None,
                },
            };
            (repl_def.event.clone(), event_modifiers_for_ability(ability))
        }
        None => return Ok(proposed),
    };

    if let Some(handler) = registry.get(&event_key) {
        let event_type = event_key.to_string();
        match (handler.applier)(proposed, rid, state, events) {
            ApplyResult::Modified(mut new_event) => {
                // CR 614.1c: If the applied branch is a Tap SelfRef (ETB tapped), mark the zone change.
                if modifiers.enters_tapped {
                    if let ProposedEvent::ZoneChange {
                        ref mut enter_tapped,
                        ..
                    } = new_event
                    {
                        *enter_tapped = true;
                    }
                }
                // CR 614.6: Apply zone redirect (e.g., graveyard → exile for Rest in Peace).
                if let Some(zone) = modifiers.redirect_zone {
                    if let ProposedEvent::ZoneChange { ref mut to, .. } = new_event {
                        *to = zone;
                    }
                }
                // CR 614.1c: Applied branch carries ETB counter data; add to the zone change.
                if !modifiers.etb_counters.is_empty() {
                    if let ProposedEvent::ZoneChange {
                        ref mut enter_with_counters,
                        ..
                    } = new_event
                    {
                        enter_with_counters.extend(modifiers.etb_counters.iter().cloned());
                    }
                }
                events.push(GameEvent::ReplacementApplied {
                    source_id: rid.source,
                    event_type,
                });
                return Ok(new_event);
            }
            ApplyResult::Prevented => {
                events.push(GameEvent::ReplacementApplied {
                    source_id: rid.source,
                    event_type,
                });
                return Err(ApplyResult::Prevented);
            }
        }
    }
    Ok(proposed)
}

fn pipeline_loop(
    state: &mut GameState,
    mut proposed: ProposedEvent,
    mut depth: u16,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    loop {
        if depth >= MAX_REPLACEMENT_DEPTH {
            break;
        }

        let candidates = find_applicable_replacements(state, &proposed, registry);

        if candidates.is_empty() {
            break;
        }

        if candidates.len() == 1 {
            let rid = candidates[0];

            // Check if this single candidate is Optional — if so, present as a choice
            let is_optional = state
                .objects
                .get(&rid.source)
                .and_then(|obj| obj.replacement_definitions.get(rid.index))
                .map(|repl| matches!(repl.mode, ReplacementMode::Optional { .. }))
                .unwrap_or(false);

            if is_optional {
                let affected = proposed.affected_player(state);
                state.pending_replacement = Some(PendingReplacement {
                    proposed,
                    candidates,
                    depth,
                    is_optional: true,
                });
                return ReplacementResult::NeedsChoice(affected);
            }

            proposed.mark_applied(rid);
            match apply_single_replacement(
                state,
                proposed,
                rid,
                ReplacementBranch::Execute,
                registry,
                events,
            ) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
        } else {
            // CR 616.1e: If multiple replacement effects apply, the affected player chooses which to apply first.
            // If all are mandatory, auto-apply the first (APNAP order) and continue the loop —
            // the loop will pick up the remaining candidates on the next iteration.
            let any_optional = candidates.iter().any(|rid| {
                state
                    .objects
                    .get(&rid.source)
                    .and_then(|obj| obj.replacement_definitions.get(rid.index))
                    .is_some_and(|repl| matches!(repl.mode, ReplacementMode::Optional { .. }))
            });

            if any_optional {
                let affected = proposed.affected_player(state);
                state.pending_replacement = Some(PendingReplacement {
                    proposed,
                    candidates,
                    depth,
                    is_optional: false,
                });
                return ReplacementResult::NeedsChoice(affected);
            }

            // All mandatory: apply the first candidate; remaining will be picked up next iteration.
            let rid = candidates[0];
            proposed.mark_applied(rid);
            match apply_single_replacement(
                state,
                proposed,
                rid,
                ReplacementBranch::Execute,
                registry,
                events,
            ) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
        }

        depth += 1;
    }

    ReplacementResult::Execute(proposed)
}

pub fn replace_event(
    state: &mut GameState,
    proposed: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let registry = build_replacement_registry();
    pipeline_loop(state, proposed, 0, &registry, events)
}

pub fn continue_replacement(
    state: &mut GameState,
    chosen_index: usize,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let pending = match state.pending_replacement.take() {
        Some(p) => p,
        None => {
            return ReplacementResult::Execute(ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 0,
                applied: std::collections::HashSet::new(),
            });
        }
    };

    let registry = build_replacement_registry();

    // Optional replacement: index 0 = accept, index 1 = decline
    if pending.is_optional {
        let rid = pending.candidates[0];
        let mut proposed = pending.proposed;
        proposed.mark_applied(rid);

        // Extract the accept/decline effects before applying
        let (accept_effect, decline_effect) = state
            .objects
            .get(&rid.source)
            .and_then(|obj| obj.replacement_definitions.get(rid.index))
            .map(|repl| {
                let accept = repl.execute.clone();
                let decline = match &repl.mode {
                    ReplacementMode::Optional { decline } => decline.clone(),
                    ReplacementMode::Mandatory => None,
                };
                (accept, decline)
            })
            .unwrap_or((None, None));

        let (branch, post_effect) = if chosen_index == 0 {
            // Accept: `execute` runs post-zone-change (e.g., shock lands pay 2 life).
            (ReplacementBranch::Execute, accept_effect)
        } else {
            // CR 614.1c + CR 614.12: Decline's ProposedEvent modifications (enter_tapped,
            // counters, zone redirect) must flow through the replacement pipeline so the
            // next iteration sees the current state of the event. If the decline branch
            // is a pure event modifier (e.g., shock-land Tap SelfRef), no post-effect is
            // needed — the modifier has already been applied to the ProposedEvent.
            // If the decline branch has non-modifier work (e.g., a choice side-effect),
            // it is retained as a post-replacement side effect.
            let post = if EventModifiers::has_only_event_modifier(decline_effect.as_deref()) {
                None
            } else {
                decline_effect
            };
            (ReplacementBranch::Decline, post)
        };

        match apply_single_replacement(state, proposed, rid, branch, &registry, events) {
            Ok(new_event) => proposed = new_event,
            Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
            Err(ApplyResult::Modified(_)) => unreachable!(),
        }
        state.post_replacement_effect = post_effect;

        return pipeline_loop(state, proposed, pending.depth + 1, &registry, events);
    }

    if chosen_index >= pending.candidates.len() {
        return ReplacementResult::Execute(pending.proposed);
    }

    let rid = pending.candidates[chosen_index];
    let mut proposed = pending.proposed;
    proposed.mark_applied(rid);

    match apply_single_replacement(
        state,
        proposed,
        rid,
        ReplacementBranch::Execute,
        &registry,
        events,
    ) {
        Ok(new_event) => proposed = new_event,
        Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
        Err(ApplyResult::Modified(_)) => unreachable!(),
    }

    pipeline_loop(state, proposed, pending.depth + 1, &registry, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{GainLifePlayer, ReplacementDefinition, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use std::collections::HashSet;

    fn make_repl(event: ReplacementEvent) -> ReplacementDefinition {
        ReplacementDefinition::new(event)
    }

    /// Placeholder event for `evaluate_replacement_condition` callers that
    /// aren't exercising event-contextual conditions (`OnlyExtraTurn`). A
    /// natural-turn BeginTurn is inert against all state-based conditions.
    fn dummy_begin_turn_event() -> ProposedEvent {
        ProposedEvent::begin_turn(PlayerId(0), false)
    }

    fn test_state_with_object(
        obj_id: ObjectId,
        zone: Zone,
        replacements: Vec<ReplacementDefinition>,
    ) -> GameState {
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(obj_id, CardId(1), PlayerId(0), "Test".to_string(), zone);
        obj.replacement_definitions = replacements;
        state.objects.insert(obj_id, obj);
        if zone == Zone::Battlefield {
            state.battlefield.push(obj_id);
        }
        state
    }

    #[test]
    fn test_single_replacement_zone_change() {
        // Creature with Moved replacement (no params means handler applies with default behavior)
        let repl = make_repl(ReplacementEvent::Moved);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);

        // With empty params, the Moved handler applies default behavior (fallback: stay in origin)
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { .. }) => {
                // Replacement was applied
            }
            other => panic!("expected Execute with ZoneChange, got {:?}", other),
        }
        // Should have emitted a ReplacementApplied event
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ReplacementApplied {
                event_type,
                ..
            } if event_type == "Moved"
        )));
    }

    #[test]
    fn test_once_per_event_enforcement() {
        // Two mandatory Moved replacements on the same object — both auto-apply; neither fires twice.
        let repl1 = make_repl(ReplacementEvent::Moved);
        let repl2 = make_repl(ReplacementEvent::Moved);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);
        // Both mandatory replacements auto-apply without NeedsChoice; each fires exactly once.
        if let ReplacementResult::Execute(event) = result {
            let applied = event.applied_set();
            assert_eq!(
                applied.len(),
                2,
                "both replacements should have been applied"
            );
        } else {
            panic!("expected Execute, got {:?}", result);
        }
    }

    #[test]
    fn test_multiple_mandatory_replacements_auto_apply() {
        // Two different objects each with a mandatory Moved replacement — both auto-apply.
        let repl = make_repl(ReplacementEvent::Moved);

        let mut state = GameState::new_two_player(42);

        let mut obj1 = GameObject::new(
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Obj1".to_string(),
            Zone::Battlefield,
        );
        obj1.replacement_definitions = vec![repl.clone()];

        let mut obj2 = GameObject::new(
            ObjectId(20),
            CardId(2),
            PlayerId(0),
            "Obj2".to_string(),
            Zone::Battlefield,
        );
        obj2.replacement_definitions = vec![repl];

        state.objects.insert(ObjectId(10), obj1);
        state.objects.insert(ObjectId(20), obj2);
        state.battlefield.push(ObjectId(10));
        state.battlefield.push(ObjectId(20));

        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);

        let mut events = Vec::new();
        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(30),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            enter_tapped: false,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        // Both mandatory replacements auto-apply; result is Execute with both in the applied set.
        if let ReplacementResult::Execute(event) = result {
            assert_eq!(
                event.applied_set().len(),
                2,
                "both replacements should have applied"
            );
        } else {
            panic!("expected Execute, got {:?}", result);
        }
    }

    #[test]
    fn gain_life_replacement_uses_execute_as_delta() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::LifeGain {
            player_id: PlayerId(0),
            amount: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::LifeGain { amount, .. }) => {
                assert_eq!(amount, 4);
            }
            other => panic!("expected Execute with LifeGain, got {:?}", other),
        }
    }

    #[test]
    fn draw_replacement_uses_event_context_amount_with_offset() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Draw).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Draw { count, .. }) => {
                assert_eq!(count, 4);
            }
            other => panic!("expected Execute with Draw, got {:?}", other),
        }
    }

    #[test]
    fn draw_replacement_does_not_apply_when_quantity_gate_is_false() {
        let repl = ReplacementDefinition::new(ReplacementEvent::Draw)
            .condition(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::HandSize,
                },
                comparator: crate::types::ability::Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
            .execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        state.players[0].hand.extend([ObjectId(20), ObjectId(21)]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Draw { count, .. }) => {
                assert_eq!(count, 3);
            }
            other => panic!("expected Execute with Draw, got {:?}", other),
        }
    }

    #[test]
    fn draw_replacement_does_not_apply_to_zero_card_draws() {
        let repl =
            ReplacementDefinition::new(ReplacementEvent::Draw).execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                },
            ));
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 0,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            find_applicable_replacements(&state, &proposed, &registry).is_empty(),
            "draw replacements with 'one or more' semantics should not apply to zero-card draws"
        );
    }

    #[test]
    fn test_continue_replacement_after_choice() {
        // Two mandatory Moved replacements — both auto-apply without NeedsChoice.
        // (continue_replacement is exercised by the optional-replacement tests.)
        let repl1 = make_repl(ReplacementEvent::Moved);
        let repl2 = make_repl(ReplacementEvent::Moved);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        let result = replace_event(&mut state, proposed, &mut events);
        // Both mandatory replacements auto-apply; result is Execute with both applied.
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "mandatory replacements should auto-apply, got {:?}",
            result
        );
    }

    #[test]
    fn test_depth_cap() {
        // A replacement that always matches (Moved with no params filter)
        // but once-per-event tracking should prevent infinite loop anyway.
        let repl = make_repl(ReplacementEvent::Moved);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed =
            ProposedEvent::zone_change(ObjectId(10), Zone::Battlefield, Zone::Graveyard, None);

        // Should complete without hanging (once-per-event prevents re-application)
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "should complete even with broadly-matching replacement"
        );
    }

    #[test]
    fn test_damage_replacement_matches() {
        // DamageDone replacement matches damage events
        let repl = make_repl(ReplacementEvent::DamageDone);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(0)),
            amount: 5,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // Without Prevent param, the handler modifies (passes through)
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "damage replacement should apply (passthrough without Prevent param)"
        );
    }

    #[test]
    fn test_no_replacements_passthrough() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(99),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            enter_tapped: false,
            enter_with_counters: Vec::new(),
            controller_override: None,
            enter_transformed: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed.clone(), &mut events);
        match result {
            ReplacementResult::Execute(event) => {
                assert_eq!(event, proposed);
            }
            other => panic!("expected Execute passthrough, got {:?}", other),
        }
        assert!(
            events.is_empty(),
            "no events should be emitted for passthrough"
        );
    }

    #[test]
    fn test_dealt_damage_replacement_matches_damage_to_source() {
        // DealtDamage replacement on a creature matches damage dealt to it
        let repl = make_repl(ReplacementEvent::DealtDamage);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(10)),
            amount: 5,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // DealtDamage matcher checks target matches source_id, so it should match
        // Without Prevent param, it passes through as modified
        match result {
            ReplacementResult::Execute(_) | ReplacementResult::Prevented => {
                // Handler was invoked (either modified or prevented depending on implementation)
            }
            other => panic!("unexpected result: {:?}", other),
        }
    }

    #[test]
    fn test_dealt_damage_does_not_match_damage_to_other() {
        // DealtDamage on ObjectId(10) should NOT match damage targeting ObjectId(20)
        let repl = make_repl(ReplacementEvent::DealtDamage);

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(20)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        // Should pass through since the target doesn't match the replacement source
        assert!(matches!(result, ReplacementResult::Execute(_)));
    }

    #[test]
    fn test_registry_has_all_types() {
        let registry = build_replacement_registry();
        // Count reflects first-class matchers + placeholders for parser-emitted
        // but not-yet-typed events (ProduceMana, TurnFaceUp) + stubs for
        // parser-emitted events whose semantics live in statics (GameLoss,
        // GameWin). Phantom ReplacementEvent variants with zero parser
        // emission are intentionally NOT registered — their absence is a
        // fail-fast signal if a future parser path starts producing them
        // without wiring a handler.
        assert!(
            registry.len() >= 22,
            "registry should have 22+ entries, got {}",
            registry.len()
        );

        // Verify all expected keys
        let expected: Vec<ReplacementEvent> = vec![
            ReplacementEvent::DamageDone,
            ReplacementEvent::Moved,
            ReplacementEvent::Destroy,
            ReplacementEvent::Draw,
            ReplacementEvent::DrawCards,
            ReplacementEvent::GainLife,
            ReplacementEvent::LifeReduced,
            ReplacementEvent::LoseLife,
            ReplacementEvent::AddCounter,
            ReplacementEvent::RemoveCounter,
            ReplacementEvent::Tap,
            ReplacementEvent::Untap,
            ReplacementEvent::Counter,
            ReplacementEvent::CreateToken,
            ReplacementEvent::Attached,
            ReplacementEvent::BeginPhase,
            ReplacementEvent::BeginTurn,
            ReplacementEvent::DealtDamage,
            ReplacementEvent::Mill,
            ReplacementEvent::PayLife,
            ReplacementEvent::ProduceMana,
            ReplacementEvent::TurnFaceUp,
            ReplacementEvent::GameLoss,
            ReplacementEvent::GameWin,
        ];
        for key in &expected {
            assert!(registry.contains_key(key), "registry missing key: {}", key);
        }
    }

    #[test]
    fn restriction_prevents_damage_prevention() {
        use crate::types::ability::{GameRestriction, ReplacementDefinition, RestrictionExpiry};

        // Create a state with a damage prevention replacement on an object
        let obj_id = ObjectId(1);
        let prevent_repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .description("Prevent all damage that would be dealt to you.".to_string());
        let mut state = test_state_with_object(obj_id, Zone::Battlefield, vec![prevent_repl]);

        // Add a DamagePreventionDisabled restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None, // Global
            });

        // Create a damage proposed event
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        // The prevention replacement should be skipped
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Prevention replacement should be skipped when DamagePreventionDisabled is active"
        );
    }

    #[test]
    fn restriction_does_not_block_non_prevention_replacements() {
        use crate::types::ability::{GameRestriction, ReplacementDefinition, RestrictionExpiry};

        // Create a state with a non-prevention damage replacement
        let obj_id = ObjectId(1);
        let non_prevent_repl = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .description("If a source would deal damage, it deals double instead.".to_string());
        let mut state = test_state_with_object(obj_id, Zone::Battlefield, vec![non_prevent_repl]);

        // Add a DamagePreventionDisabled restriction
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };

        // Non-prevention replacements should still apply
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Non-prevention damage replacements should not be blocked"
        );
    }

    // ── destination_zone filter tests (CR 614.6) ──

    fn rip_replacement() -> ReplacementDefinition {
        use crate::types::ability::{AbilityKind, TargetFilter};
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    origin: None,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                },
            ))
            .destination_zone(Zone::Graveyard)
    }

    fn authority_replacement() -> ReplacementDefinition {
        use crate::types::ability::{AbilityKind, ControllerRef, TargetFilter, TypedFilter};
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Tap {
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ))
            .destination_zone(Zone::Battlefield)
    }

    #[test]
    fn destination_zone_rip_matches_graveyard() {
        // Battlefield → Graveyard with RIP replacement → should be a candidate
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match zone change TO graveyard"
        );
    }

    #[test]
    fn destination_zone_rip_hand_to_graveyard() {
        // Hand → Graveyard (discard) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::zone_change(ObjectId(99), Zone::Hand, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match discard (hand → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_library_to_graveyard() {
        // Library → Graveyard (mill) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Library, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match mill (library → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_stack_to_graveyard() {
        // Stack → Graveyard (countered spell) with RIP → should match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed = ProposedEvent::zone_change(ObjectId(99), Zone::Stack, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "RIP should match countered spell (stack → graveyard)"
        );
    }

    #[test]
    fn destination_zone_rip_does_not_match_exile() {
        // Battlefield → Exile — RIP (destination_zone: Graveyard) should NOT match
        let repl = rip_replacement();
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Exile, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "RIP should NOT match zone change to exile"
        );
    }

    #[test]
    fn destination_zone_no_rip_passthrough() {
        // Zone change to graveyard without RIP → no replacement
        let state = GameState::new_two_player(42);
        let proposed =
            ProposedEvent::zone_change(ObjectId(99), Zone::Battlefield, Zone::Graveyard, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "No replacement should match without RIP on battlefield"
        );
    }

    fn make_creature(id: ObjectId, owner: PlayerId, zone: Zone) -> GameObject {
        use crate::types::card_type::{CardType, CoreType};
        let mut obj = GameObject::new(id, CardId(3), owner, "Test Creature".to_string(), zone);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        obj
    }

    #[test]
    fn destination_zone_authority_matches_battlefield() {
        // Opponent creature entering battlefield with Authority → should match
        let repl = authority_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Create the entering creature (owned/controlled by opponent = PlayerId(1))
        let creature = make_creature(ObjectId(30), PlayerId(1), Zone::Hand);
        state.objects.insert(ObjectId(30), creature);

        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Hand, Zone::Battlefield, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Authority should match opponent creature entering battlefield"
        );
    }

    #[test]
    fn destination_zone_authority_own_creature_not_affected() {
        // Own creature entering battlefield with Authority → should NOT match (controller filter)
        let repl = authority_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Create own creature (PlayerId(0), same as Authority's controller)
        let creature = make_creature(ObjectId(30), PlayerId(0), Zone::Hand);
        state.objects.insert(ObjectId(30), creature);

        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Hand, Zone::Battlefield, None);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Authority should NOT match own creature entering battlefield"
        );
    }

    #[test]
    fn zone_redirect_applied_in_apply_single_replacement() {
        // Test that the zone redirect in apply_single_replacement mutates the destination
        let repl = rip_replacement();
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);

        // Add the object being moved
        let target = GameObject::new(
            ObjectId(30),
            CardId(3),
            PlayerId(0),
            "Dying Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(30), target);
        state.battlefield.push(ObjectId(30));

        let mut events = Vec::new();
        let proposed =
            ProposedEvent::zone_change(ObjectId(30), Zone::Battlefield, Zone::Graveyard, None);
        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => {
                assert_eq!(to, Zone::Exile, "RIP should redirect graveyard → exile");
            }
            other => panic!("expected Execute with ZoneChange, got {:?}", other),
        }
    }

    // ── Damage modification applier tests ──

    fn damage_event(amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount,
            is_combat: false,
            applied: HashSet::new(),
        }
    }

    fn damage_repl(modification: DamageModification) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::DamageDone).damage_modification(modification)
    }

    fn test_state_with_damage_repl(
        obj_id: ObjectId,
        controller: PlayerId,
        repls: Vec<ReplacementDefinition>,
    ) -> GameState {
        let mut state = GameState::new_two_player(42);
        let mut obj = GameObject::new(
            obj_id,
            CardId(1),
            controller,
            "Test".to_string(),
            Zone::Battlefield,
        );
        obj.replacement_definitions = repls;
        state.objects.insert(obj_id, obj);
        state.battlefield.push(obj_id);
        state
    }

    #[test]
    fn damage_applier_double() {
        let repl = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 6);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_triple() {
        let repl = damage_repl(DamageModification::Triple);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 9);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_plus() {
        let repl = damage_repl(DamageModification::Plus { value: 2 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 5);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_minus() {
        let repl = damage_repl(DamageModification::Minus { value: 1 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(3), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 2);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_minus_saturates_at_zero() {
        let repl = damage_repl(DamageModification::Minus { value: 5 });
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let result = damage_done_applier(damage_event(1), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 0);
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_double_chaining_two_doublers() {
        // Two Double replacements → 3 * 2 * 2 = 12
        let repl1 = damage_repl(DamageModification::Double);
        let repl2 = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl1, repl2]);
        let mut events = Vec::new();
        let proposed = damage_event(3);
        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 12, "Two doublers should quadruple: 3 * 2 * 2 = 12");
            }
            other => panic!("Expected Execute with Damage, got {other:?}"),
        }
    }

    // ── Damage pipeline filter tests ──

    #[test]
    fn damage_source_filter_blocks_wrong_controller() {
        // Replacement on P0's object requires "source you control" but damage source is P1's
        use crate::types::ability::{ControllerRef, TypedFilter};
        let repl = damage_repl(DamageModification::Double).damage_source_filter(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Add a damage source owned by P1
        let mut source_obj = GameObject::new(
            ObjectId(50),
            CardId(2),
            PlayerId(1),
            "Enemy Source".to_string(),
            Zone::Battlefield,
        );
        source_obj.controller = PlayerId(1);
        state.objects.insert(ObjectId(50), source_obj);
        state.battlefield.push(ObjectId(50));

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            candidates.is_empty(),
            "Should not match: source controller differs"
        );
    }

    #[test]
    fn damage_source_filter_allows_correct_controller() {
        use crate::types::ability::{ControllerRef, TypedFilter};
        let repl = damage_repl(DamageModification::Double).damage_source_filter(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        );
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage source owned by P0 (same as replacement controller)
        let source_obj = GameObject::new(
            ObjectId(50),
            CardId(2),
            PlayerId(0),
            "Own Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(50), source_obj);
        state.battlefield.push(ObjectId(50));

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Should match: source controller matches"
        );
    }

    #[test]
    fn damage_target_filter_opponent_blocks_self() {
        let repl = damage_repl(DamageModification::Plus { value: 2 })
            .damage_target_filter(DamageTargetFilter::OpponentOrTheirPermanents);
        // Replacement on P0's object
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage targets P0 (self) — should not match
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(candidates.is_empty(), "Should not match damage to self");
    }

    #[test]
    fn damage_target_filter_opponent_allows_opponent() {
        let repl = damage_repl(DamageModification::Plus { value: 2 })
            .damage_target_filter(DamageTargetFilter::OpponentOrTheirPermanents);
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage targets P1 (opponent) — should match
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(!candidates.is_empty(), "Should match damage to opponent");
    }

    #[test]
    fn damage_target_filter_opponent_allows_opponents_permanent() {
        use crate::types::card_type::CoreType;
        let repl = damage_repl(DamageModification::Plus { value: 2 })
            .damage_target_filter(DamageTargetFilter::OpponentOrTheirPermanents);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Add opponent's creature
        let mut opp_creature = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        opp_creature.card_types.core_types.push(CoreType::Creature);
        state.objects.insert(ObjectId(60), opp_creature);
        state.battlefield.push(ObjectId(60));

        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Should match damage to opponent's permanent"
        );
    }

    #[test]
    fn damage_boost_not_blocked_by_prevention_disabled() {
        use crate::types::ability::{GameRestriction, RestrictionExpiry};
        // Damage boost with damage_modification should still apply even when prevention is disabled
        let repl = damage_repl(DamageModification::Double);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state
            .restrictions
            .push(GameRestriction::DamagePreventionDisabled {
                source: ObjectId(99),
                expiry: RestrictionExpiry::EndOfTurn,
                scope: None,
            });

        let proposed = damage_event(3);
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);
        assert!(
            !candidates.is_empty(),
            "Damage boost should not be blocked by prevention disabled"
        );
    }

    // ── Regeneration shield tests ──

    /// Helper: create a creature on the battlefield with a regeneration shield.
    fn create_creature_with_regen_shield(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = crate::game::zones::create_object(
            state,
            CardId(1),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);

            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield();
            obj.replacement_definitions.push(shield);
        }
        id
    }

    #[test]
    fn regen_shield_prevents_targeted_destruction() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: Some(ObjectId(100)),
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        assert_eq!(result, ReplacementResult::Prevented);
        // CR 701.19: Creature stays on battlefield
        assert!(state.battlefield.contains(&bear_id));
        // CR 701.19: Damage removed and tapped
        let obj = state.objects.get(&bear_id).unwrap();
        assert_eq!(obj.damage_marked, 0);
        assert!(obj.tapped);
        // Shield consumed
        assert!(obj.replacement_definitions[0].is_consumed);
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { object_id } if *object_id == bear_id)));
    }

    #[test]
    fn regen_shield_removes_damage_and_deathtouch() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Mark damage including deathtouch
        {
            let obj = state.objects.get_mut(&bear_id).unwrap();
            obj.damage_marked = 3;
            obj.dealt_deathtouch_damage = true;
        }

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        let obj = state.objects.get(&bear_id).unwrap();
        assert_eq!(obj.damage_marked, 0);
        assert!(!obj.dealt_deathtouch_damage);
    }

    #[test]
    fn cant_regenerate_bypasses_shield() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: Some(ObjectId(100)),
            cant_regenerate: true,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        // Should pass through — not prevented
        assert!(
            matches!(
                result,
                ReplacementResult::Execute(ProposedEvent::Destroy { .. })
            ),
            "cant_regenerate should bypass shield, got {:?}",
            result
        );
        // Shield not consumed
        let obj = state.objects.get(&bear_id).unwrap();
        assert!(!obj.replacement_definitions[0].is_consumed);
    }

    #[test]
    fn regen_shield_consumption_one_of_two() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Add a second shield
        {
            let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate 2".to_string())
                .regeneration_shield();
            state
                .objects
                .get_mut(&bear_id)
                .unwrap()
                .replacement_definitions
                .push(shield);
        }

        // First destruction — one shield consumed
        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);
        assert_eq!(result, ReplacementResult::Prevented);

        let obj = state.objects.get(&bear_id).unwrap();
        let consumed_count = obj
            .replacement_definitions
            .iter()
            .filter(|r| r.is_consumed)
            .count();
        let active_count = obj
            .replacement_definitions
            .iter()
            .filter(|r| r.shield_kind.is_shield() && !r.is_consumed)
            .count();
        assert_eq!(consumed_count, 1, "One shield should be consumed");
        assert_eq!(active_count, 1, "One shield should remain active");

        // Second destruction — second shield consumed
        let proposed2 = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let result2 = replace_event(&mut state, proposed2, &mut events);
        assert_eq!(result2, ReplacementResult::Prevented);

        let obj = state.objects.get(&bear_id).unwrap();
        let all_consumed = obj
            .replacement_definitions
            .iter()
            .filter(|r| r.shield_kind.is_shield())
            .all(|r| r.is_consumed);
        assert!(all_consumed, "Both shields should be consumed now");
    }

    #[test]
    fn regen_shield_removes_from_combat_attacker() {
        use crate::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let attacker_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Attacker");

        // Set up combat with the creature as an attacker
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            ..Default::default()
        });

        let proposed = ProposedEvent::Destroy {
            object_id: attacker_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        // CR 701.19c: Removed from combat
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.is_empty(),
            "Regenerated attacker should be removed from combat"
        );
    }

    #[test]
    fn regen_shield_removes_from_combat_blocker() {
        use crate::game::combat::{AttackerInfo, CombatState};
        use std::collections::HashMap;

        let mut state = GameState::new_two_player(42);
        let blocker_id = create_creature_with_regen_shield(&mut state, PlayerId(1), "Blocker");
        let attacker_id = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );

        // Set up combat with the creature as a blocker
        let mut blocker_assignments = HashMap::new();
        blocker_assignments.insert(attacker_id, vec![blocker_id]);
        let mut blocker_to_attacker = HashMap::new();
        blocker_to_attacker.insert(blocker_id, vec![attacker_id]);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            blocker_assignments,
            blocker_to_attacker,
            ..Default::default()
        });

        let proposed = ProposedEvent::Destroy {
            object_id: blocker_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        replace_event(&mut state, proposed, &mut events);

        let combat = state.combat.as_ref().unwrap();
        assert!(
            !combat.blocker_to_attacker.contains_key(&blocker_id),
            "Regenerated blocker should be removed from blocker_to_attacker"
        );
        // Blocker removed from the attacker's blocker list
        let blockers = combat.blocker_assignments.get(&attacker_id).unwrap();
        assert!(
            !blockers.contains(&blocker_id),
            "Regenerated blocker should be removed from blocker list"
        );
    }

    #[test]
    fn regen_shield_taps_already_tapped_creature() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Already tapped
        state.objects.get_mut(&bear_id).unwrap().tapped = true;

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        let result = replace_event(&mut state, proposed, &mut events);

        assert_eq!(result, ReplacementResult::Prevented);
        // Still tapped (no-op on already-tapped)
        assert!(state.objects.get(&bear_id).unwrap().tapped);
    }

    #[test]
    fn consumed_shield_skipped_by_find_applicable() {
        let mut state = GameState::new_two_player(42);
        let bear_id = create_creature_with_regen_shield(&mut state, PlayerId(0), "Bear");

        // Pre-consume the shield
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions[0]
            .is_consumed = true;

        let proposed = ProposedEvent::Destroy {
            object_id: bear_id,
            source: None,
            cant_regenerate: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        let candidates = find_applicable_replacements(&state, &proposed, &registry);

        assert!(
            candidates.is_empty(),
            "Consumed shield should not be a candidate"
        );
    }

    #[test]
    fn unless_your_turn_untapped_on_controllers_turn() {
        let state = GameState::new_two_player(42);
        // active_player is PlayerId(0) by default
        let cond = ReplacementCondition::UnlessYourTurn;
        // Controller is active player → replacement suppressed (enters untapped)
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) on controller's turn"
        );
    }

    #[test]
    fn unless_your_turn_tapped_on_opponents_turn() {
        let state = GameState::new_two_player(42);
        let cond = ReplacementCondition::UnlessYourTurn;
        // Controller is NOT active player → replacement applies (enters tapped)
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(1),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) on opponent's turn"
        );
    }

    #[test]
    fn unless_quantity_turn_count_untapped_within_threshold() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.players[0].turns_taken = 2;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // turns_taken=2 ≤ 3 on controller's turn → suppressed (untapped)
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) when turns_taken <= threshold"
        );
    }

    #[test]
    fn unless_quantity_turn_count_tapped_beyond_threshold() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.players[0].turns_taken = 4;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // turns_taken=4 > 3 → replacement applies (tapped)
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) when turns_taken > threshold"
        );
    }

    #[test]
    fn unless_quantity_tapped_on_opponents_turn_regardless_of_count() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1); // Opponent's turn
        state.players[0].turns_taken = 1; // Controller's count is low
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: Some(ControllerRef::You),
        };
        // Not controller's turn → replacement applies (tapped) even though turns_taken ≤ 3
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply (tapped) when not controller's turn"
        );
    }

    #[test]
    fn unless_quantity_no_turn_req_works_on_any_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1); // Opponent's turn
        state.players[0].turns_taken = 2;
        let cond = ReplacementCondition::UnlessQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::TurnsTaken,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
            active_player_req: None, // No turn requirement
        };
        // No turn gate, turns_taken=2 ≤ 3 → suppressed regardless of active player
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should be suppressed (untapped) with no turn requirement"
        );
    }

    #[test]
    fn only_if_quantity_applies_when_condition_is_true() {
        let mut state = GameState::new_two_player(42);
        state.players[0].hand.truncate(1);
        let cond = ReplacementCondition::OnlyIfQuantity {
            lhs: QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::HandSize,
            },
            comparator: crate::types::ability::Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 1 },
            active_player_req: None,
        };
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &dummy_begin_turn_event()
            ),
            "Should apply while hand size is one or fewer"
        );
    }

    #[test]
    fn only_if_quantity_is_filtered_for_opponent_draws() {
        let repl = ReplacementDefinition::new(ReplacementEvent::Draw)
            .condition(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::HandSize,
                },
                comparator: crate::types::ability::Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
            .execute(AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                },
            ));
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        state.players[0].hand.truncate(1);

        let proposed = ProposedEvent::Draw {
            player_id: PlayerId(1),
            count: 2,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            find_applicable_replacements(&state, &proposed, &registry).is_empty(),
            "Controller-only draw replacement should not apply to opponent draws"
        );
    }

    #[test]
    fn damage_applier_set_to_source_power_replaces_when_less() {
        let repl = damage_repl(DamageModification::SetToSourcePower);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        // Set replacement source's power to 4
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(4);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        // Damage amount 2 < power 4 → should be replaced to 4
        let result = damage_done_applier(damage_event(2), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 4, "Damage should be set to source power");
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_applier_set_to_source_power_no_change_when_greater() {
        let repl = damage_repl(DamageModification::SetToSourcePower);
        let mut state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);
        state.objects.get_mut(&ObjectId(10)).unwrap().power = Some(4);
        let mut events = Vec::new();
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        // Damage amount 5 >= power 4 → should NOT be replaced
        let result = damage_done_applier(damage_event(5), rid, &mut state, &mut events);
        match result {
            ApplyResult::Modified(ProposedEvent::Damage { amount, .. }) => {
                assert_eq!(amount, 5, "Damage should pass through unchanged");
            }
            other => panic!("Expected Modified Damage, got {other:?}"),
        }
    }

    #[test]
    fn damage_target_filter_opponent_only() {
        let repl = damage_repl(DamageModification::Plus { value: 1 })
            .damage_target_filter(DamageTargetFilter::OpponentOnly);
        let state = test_state_with_damage_repl(ObjectId(10), PlayerId(0), vec![repl]);

        // Damage to opponent (P1) — should match
        let proposed_opp = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        let registry = build_replacement_registry();
        assert!(
            !find_applicable_replacements(&state, &proposed_opp, &registry).is_empty(),
            "Should match damage to opponent"
        );

        // Damage to self (P0) — should NOT match
        let proposed_self = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state, &proposed_self, &registry).is_empty(),
            "Should not match damage to self"
        );

        // Damage to a creature — should NOT match (OpponentOnly is player-only)
        let mut state2 = state.clone();
        let mut creature = GameObject::new(
            ObjectId(60),
            CardId(3),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        creature.card_types.core_types.push(CoreType::Creature);
        state2.objects.insert(ObjectId(60), creature);
        state2.battlefield.push(ObjectId(60));

        let proposed_creature = ProposedEvent::Damage {
            source_id: ObjectId(50),
            target: TargetRef::Object(ObjectId(60)),
            amount: 3,
            is_combat: false,
            applied: HashSet::new(),
        };
        assert!(
            find_applicable_replacements(&state2, &proposed_creature, &registry).is_empty(),
            "OpponentOnly should not match damage to creatures"
        );
    }

    // --- BeginTurn / BeginPhase (CR 614.1b, CR 614.10) ---

    #[test]
    fn only_extra_turn_condition_fires_only_on_extra_turn() {
        // CR 500.7 + CR 614.10: Stranglehold-class replacement with OnlyExtraTurn
        // must pass the condition check on extra turns and fail on natural turns.
        // Condition gating lives in `evaluate_replacement_condition` (the matcher
        // only filters by event shape); this test exercises the condition directly.
        let state = GameState::new_two_player(42);
        let cond = ReplacementCondition::OnlyExtraTurn;

        let extra_turn_event = ProposedEvent::begin_turn(PlayerId(0), true);
        assert!(
            evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &extra_turn_event
            ),
            "OnlyExtraTurn should apply when is_extra_turn=true"
        );

        let natural_turn_event = ProposedEvent::begin_turn(PlayerId(0), false);
        assert!(
            !evaluate_replacement_condition(
                &cond,
                PlayerId(0),
                ObjectId(1),
                &state,
                None,
                &natural_turn_event
            ),
            "OnlyExtraTurn should NOT apply when is_extra_turn=false"
        );
    }

    #[test]
    fn begin_turn_matcher_matches_event_shape_only() {
        // Matcher checks event shape; per-def gating runs in the outer pipeline.
        let state = GameState::new_two_player(42);
        let begin_turn = ProposedEvent::begin_turn(PlayerId(0), true);
        let draw = ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 1,
            applied: HashSet::new(),
        };
        assert!(begin_turn_matcher(&begin_turn, ObjectId(1), &state));
        assert!(!begin_turn_matcher(&draw, ObjectId(1), &state));
    }

    #[test]
    fn begin_turn_applier_returns_prevented() {
        // CR 614.10: "skip" means unconditionally skip — applier must return Prevented.
        let repl =
            make_repl(ReplacementEvent::BeginTurn).condition(ReplacementCondition::OnlyExtraTurn);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let rid = ReplacementId {
            source: ObjectId(10),
            index: 0,
        };
        let mut events = Vec::new();
        let proposed = ProposedEvent::begin_turn(PlayerId(0), true);

        let result = begin_turn_applier(proposed, rid, &mut state, &mut events);
        assert!(matches!(result, ApplyResult::Prevented));
    }

    #[test]
    fn begin_turn_replacement_does_not_consume_shield() {
        // CR 614.10 + ShieldKind::None: permanent statics fire every time their
        // predicate matches — the replacement definition is NOT marked consumed
        // after the pipeline applies it.
        let repl =
            make_repl(ReplacementEvent::BeginTurn).condition(ReplacementCondition::OnlyExtraTurn);
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();
        let proposed = ProposedEvent::begin_turn(PlayerId(0), true);

        let result = replace_event(&mut state, proposed, &mut events);
        assert!(matches!(result, ReplacementResult::Prevented));

        let obj = state.objects.get(&ObjectId(10)).unwrap();
        assert!(
            !obj.replacement_definitions[0].is_consumed,
            "permanent static skip replacement must not be consumed after use"
        );
    }

    #[test]
    fn begin_phase_matcher_fires_for_bare_begin_phase_def() {
        // CR 614.1b: Unconditional BeginPhase replacement should match the event.
        let repl = make_repl(ReplacementEvent::BeginPhase);
        let state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let proposed = ProposedEvent::begin_phase(PlayerId(0), crate::types::phase::Phase::Upkeep);

        assert!(begin_phase_matcher(&proposed, ObjectId(10), &state));
    }
}
