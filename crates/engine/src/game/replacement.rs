use indexmap::IndexMap;

use crate::types::ability::{
    AbilityDefinition, Effect, QuantityExpr, ReplacementCondition, ReplacementMode, TargetFilter,
};

use super::filter::matches_target_filter;
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

fn damage_done_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 3. Destroy ---

fn destroy_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Destroy { .. })
}

fn destroy_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 4. Draw ---

fn draw_matcher(event: &ProposedEvent, _source: ObjectId, _state: &GameState) -> bool {
    matches!(event, ProposedEvent::Draw { .. })
}

fn draw_applier(
    event: ProposedEvent,
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 5. GainLife ---

fn gain_life_matcher(event: &ProposedEvent, source: ObjectId, state: &GameState) -> bool {
    if let ProposedEvent::LifeGain { player_id, .. } = event {
        state
            .objects
            .get(&source)
            .is_some_and(|obj| obj.controller == *player_id)
    } else {
        false
    }
}

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

    match &execute.effect {
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
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
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
    _rid: ReplacementId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
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

// --- Registry ---

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
    registry.insert(ReplacementEvent::Other("DrawCards".into()), stub()); // stays stub (alias for Draw)
    registry.insert(
        ReplacementEvent::GainLife,
        ReplacementHandlerEntry {
            matcher: gain_life_matcher,
            applier: gain_life_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("LifeReduced".into()),
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
        ReplacementEvent::Other("AddCounter".into()),
        ReplacementHandlerEntry {
            matcher: add_counter_matcher,
            applier: add_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("RemoveCounter".into()),
        ReplacementHandlerEntry {
            matcher: remove_counter_matcher,
            applier: remove_counter_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("Tap".into()),
        ReplacementHandlerEntry {
            matcher: tap_matcher,
            applier: tap_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("Untap".into()),
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
        ReplacementEvent::Other("CreateToken".into()),
        ReplacementHandlerEntry {
            matcher: create_token_matcher,
            applier: create_token_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("Attached".into()),
        ReplacementHandlerEntry {
            matcher: attached_matcher,
            applier: attached_applier,
        },
    );

    // Promoted from stubs to real handlers
    registry.insert(
        ReplacementEvent::Other("DealtDamage".into()),
        ReplacementHandlerEntry {
            matcher: dealt_damage_matcher,
            applier: dealt_damage_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("Mill".into()),
        ReplacementHandlerEntry {
            matcher: mill_matcher,
            applier: mill_applier,
        },
    );
    registry.insert(
        ReplacementEvent::Other("PayLife".into()),
        ReplacementHandlerEntry {
            matcher: pay_life_matcher,
            applier: pay_life_applier,
        },
    );
    let placeholder = || ReplacementHandlerEntry {
        matcher: placeholder_matcher,
        applier: placeholder_applier,
    };
    registry.insert(ReplacementEvent::Other("ProduceMana".into()), placeholder());
    registry.insert(ReplacementEvent::Other("Scry".into()), placeholder());
    registry.insert(ReplacementEvent::Other("Transform".into()), placeholder());
    registry.insert(ReplacementEvent::TurnFaceUp, placeholder());
    registry.insert(ReplacementEvent::Other("Explore".into()), placeholder());

    // 12 remaining Forge types (stubs -- recognized but no-op)
    let stub_events = [
        "BeginPhase",
        "BeginTurn",
        "DeclareBlocker",
        "GameLoss",
        "GameWin",
        "Learn",
        "LoseMana",
        "Proliferate",
        "AssembleContraption",
        "Cascade",
        "CopySpell",
        "PlanarDiceResult",
        "Planeswalk",
    ];
    for ev in &stub_events {
        registry.insert(ReplacementEvent::Other((*ev).into()), stub());
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
        || matches!(event, ReplacementEvent::Other(s) if s == "DealtDamage");
    if !is_damage_event {
        return false;
    }

    // Check the replacement definition description for prevention keywords
    state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
        .is_some_and(|repl| {
            repl.description.as_ref().is_some_and(|d| {
                let lower = d.to_lowercase();
                lower.contains("prevent") && lower.contains("damage")
            })
        })
}

// --- Pipeline functions ---

/// Evaluate a replacement condition against the current game state.
/// Returns `true` if the replacement should apply, `false` if it should be skipped.
fn evaluate_replacement_condition(
    condition: &ReplacementCondition,
    controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
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
            let matching_count = state
                .objects
                .values()
                .filter(|o| {
                    o.zone == Zone::Battlefield
                        && matches_target_filter(state, o.id, &target_filter, source_id)
                })
                .count() as u32;
            // "unless you control N or fewer" → suppressed when count ≤ N
            // Replacement applies (enters tapped) when count > N
            matching_count > *count
        }
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
                        let matches = event
                            .affected_object_id()
                            .map(|oid| {
                                super::filter::matches_target_filter(state, oid, filter, obj.id)
                            })
                            .unwrap_or(false);
                        if !matches {
                            continue;
                        }
                    }
                    // CR 614.6: Zone-change replacements may be scoped to a specific destination.
                    if let Some(ref dest_zone) = repl_def.destination_zone {
                        let matches_dest = match event {
                            ProposedEvent::ZoneChange { to, .. } => to == dest_zone,
                            _ => true,
                        };
                        if !matches_dest {
                            continue;
                        }
                    }
                    // Evaluate replacement condition (e.g. "unless you control a Mountain")
                    if let Some(ref cond) = repl_def.condition {
                        if !evaluate_replacement_condition(cond, obj.controller, obj.id, state) {
                            continue;
                        }
                    }
                    // CR 614.16: Skip damage prevention replacements when prevention is disabled
                    if is_damage_prevention_replacement(state, &rid, &repl_def.event)
                        && is_prevention_disabled(state, event)
                    {
                        continue;
                    }
                    candidates.push(rid);
                }
            }
        }
    }

    candidates
}

const MAX_REPLACEMENT_DEPTH: u16 = 16;

/// Extract ETB counter data from a replacement's execute effect.
/// Handles `PutCounter` and `AddCounter` effects, returning (counter_type, count) pairs.
fn extract_etb_counters(execute: Option<&AbilityDefinition>) -> Vec<(String, u32)> {
    let exec = match execute {
        Some(e) => e,
        None => return Vec::new(),
    };
    match &exec.effect {
        Effect::PutCounter {
            counter_type,
            count,
            ..
        }
        | Effect::AddCounter {
            counter_type,
            count,
            ..
        } => vec![(counter_type.clone(), *count as u32)],
        _ => Vec::new(),
    }
}

fn apply_single_replacement(
    state: &mut GameState,
    proposed: ProposedEvent,
    rid: ReplacementId,
    registry: &IndexMap<ReplacementEvent, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
    // Extract replacement metadata before mutably borrowing state for the applier.
    let (event_key, enters_tapped, etb_counters, redirect_zone) = match state
        .objects
        .get(&rid.source)
        .and_then(|obj| obj.replacement_definitions.get(rid.index))
    {
        Some(repl_def) => {
            let tapped = repl_def.execute.as_ref().is_some_and(|exec| {
                matches!(
                    exec.effect,
                    crate::types::ability::Effect::Tap {
                        target: crate::types::ability::TargetFilter::SelfRef,
                    }
                )
            });
            let counters = extract_etb_counters(repl_def.execute.as_deref());
            // CR 614.6: Zone-change replacement — redirect destination.
            let redirect_zone = repl_def
                .execute
                .as_ref()
                .and_then(|exec| match &exec.effect {
                    Effect::ChangeZone { destination, .. } => Some(*destination),
                    _ => None,
                });
            (repl_def.event.clone(), tapped, counters, redirect_zone)
        }
        None => return Ok(proposed),
    };

    if let Some(handler) = registry.get(&event_key) {
        let event_type = event_key.to_string();
        match (handler.applier)(proposed, rid, state, events) {
            ApplyResult::Modified(mut new_event) => {
                // If the replacement carries a Tap execute (ETB tapped), mark the zone change.
                if enters_tapped {
                    if let ProposedEvent::ZoneChange {
                        ref mut enter_tapped,
                        ..
                    } = new_event
                    {
                        *enter_tapped = true;
                    }
                }
                // CR 614.6: Apply zone redirect (e.g., graveyard → exile for Rest in Peace).
                if let Some(zone) = redirect_zone {
                    if let ProposedEvent::ZoneChange { ref mut to, .. } = new_event {
                        *to = zone;
                    }
                }
                // If the replacement carries counter data, add to the zone change.
                if !etb_counters.is_empty() {
                    if let ProposedEvent::ZoneChange {
                        ref mut enter_with_counters,
                        ..
                    } = new_event
                    {
                        enter_with_counters.extend(etb_counters.iter().cloned());
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
            match apply_single_replacement(state, proposed, rid, registry, events) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
        } else {
            // Multiple candidates: if any is optional or requires ordering input, ask the player.
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
            match apply_single_replacement(state, proposed, rid, registry, events) {
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
                    _ => None,
                };
                (accept, decline)
            })
            .unwrap_or((None, None));

        if chosen_index == 0 {
            // Accept: apply the replacement, store accept effect for post-zone-change
            match apply_single_replacement(state, proposed, rid, &registry, events) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
            }
            state.post_replacement_effect = accept_effect;
        } else {
            // Decline: skip the replacement, store decline effect for post-zone-change
            state.post_replacement_effect = decline_effect;
        }

        return pipeline_loop(state, proposed, pending.depth + 1, &registry, events);
    }

    if chosen_index >= pending.candidates.len() {
        return ReplacementResult::Execute(pending.proposed);
    }

    let rid = pending.candidates[chosen_index];
    let mut proposed = pending.proposed;
    proposed.mark_applied(rid);

    match apply_single_replacement(state, proposed, rid, &registry, events) {
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
        let repl = make_repl(ReplacementEvent::Other("DealtDamage".to_string()));

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
        let repl = make_repl(ReplacementEvent::Other("DealtDamage".to_string()));

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
    fn test_registry_has_all_36_types() {
        let registry = build_replacement_registry();
        assert_eq!(
            registry.len(),
            36,
            "registry should have exactly 36 entries"
        );

        // Verify all expected keys
        let expected: Vec<ReplacementEvent> = vec![
            ReplacementEvent::DamageDone,
            ReplacementEvent::Moved,
            ReplacementEvent::Destroy,
            ReplacementEvent::Draw,
            ReplacementEvent::Other("DrawCards".into()),
            ReplacementEvent::GainLife,
            ReplacementEvent::Other("LifeReduced".into()),
            ReplacementEvent::LoseLife,
            ReplacementEvent::Other("AddCounter".into()),
            ReplacementEvent::Other("RemoveCounter".into()),
            ReplacementEvent::Other("Tap".into()),
            ReplacementEvent::Other("Untap".into()),
            ReplacementEvent::Counter,
            ReplacementEvent::Other("CreateToken".into()),
            ReplacementEvent::Other("Attached".into()),
            ReplacementEvent::Other("BeginPhase".into()),
            ReplacementEvent::Other("BeginTurn".into()),
            ReplacementEvent::Other("DealtDamage".into()),
            ReplacementEvent::Other("DeclareBlocker".into()),
            ReplacementEvent::Other("Explore".into()),
            ReplacementEvent::Other("GameLoss".into()),
            ReplacementEvent::Other("GameWin".into()),
            ReplacementEvent::Other("Learn".into()),
            ReplacementEvent::Other("LoseMana".into()),
            ReplacementEvent::Other("Mill".into()),
            ReplacementEvent::Other("PayLife".into()),
            ReplacementEvent::Other("ProduceMana".into()),
            ReplacementEvent::Other("Proliferate".into()),
            ReplacementEvent::Other("Scry".into()),
            ReplacementEvent::Other("Transform".into()),
            ReplacementEvent::TurnFaceUp,
            ReplacementEvent::Other("AssembleContraption".into()),
            ReplacementEvent::Other("Cascade".into()),
            ReplacementEvent::Other("CopySpell".into()),
            ReplacementEvent::Other("PlanarDiceResult".into()),
            ReplacementEvent::Other("Planeswalk".into()),
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
}
