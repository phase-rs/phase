use std::collections::HashMap;

use indexmap::IndexMap;

use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingReplacement};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{ProposedEvent, ReplacementId};
use crate::types::zones::Zone;

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

pub type ReplacementMatcher =
    fn(&ProposedEvent, &HashMap<String, String>, ObjectId, &GameState) -> bool;
pub type ReplacementApplier = fn(
    ProposedEvent,
    &HashMap<String, String>,
    ObjectId,
    &mut GameState,
    &mut Vec<GameEvent>,
) -> ApplyResult;

pub struct ReplacementHandlerEntry {
    pub matcher: ReplacementMatcher,
    pub applier: ReplacementApplier,
}

// Stub matcher: never matches
fn stub_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

// Stub applier: passthrough
fn stub_applier(
    event: ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

pub fn build_replacement_registry() -> IndexMap<String, ReplacementHandlerEntry> {
    let mut registry = IndexMap::new();

    let stub = || ReplacementHandlerEntry {
        matcher: stub_matcher,
        applier: stub_applier,
    };

    // 14 core types (stubs for now, real logic added in Task 2)
    registry.insert("DamageDone".to_string(), stub());
    registry.insert("Moved".to_string(), stub());
    registry.insert("Destroy".to_string(), stub());
    registry.insert("Draw".to_string(), stub());
    registry.insert("DrawCards".to_string(), stub());
    registry.insert("GainLife".to_string(), stub());
    registry.insert("LifeReduced".to_string(), stub());
    registry.insert("AddCounter".to_string(), stub());
    registry.insert("RemoveCounter".to_string(), stub());
    registry.insert("Tap".to_string(), stub());
    registry.insert("Untap".to_string(), stub());
    registry.insert("Counter".to_string(), stub());
    registry.insert("CreateToken".to_string(), stub());
    registry.insert("Attached".to_string(), stub());

    // 21 remaining Forge types (stubs -- recognized but no-op)
    registry.insert("BeginPhase".to_string(), stub());
    registry.insert("BeginTurn".to_string(), stub());
    registry.insert("DealtDamage".to_string(), stub());
    registry.insert("DeclareBlocker".to_string(), stub());
    registry.insert("Explore".to_string(), stub());
    registry.insert("GameLoss".to_string(), stub());
    registry.insert("GameWin".to_string(), stub());
    registry.insert("Learn".to_string(), stub());
    registry.insert("LoseMana".to_string(), stub());
    registry.insert("Mill".to_string(), stub());
    registry.insert("PayLife".to_string(), stub());
    registry.insert("ProduceMana".to_string(), stub());
    registry.insert("Proliferate".to_string(), stub());
    registry.insert("Scry".to_string(), stub());
    registry.insert("Transform".to_string(), stub());
    registry.insert("TurnFaceUp".to_string(), stub());
    registry.insert("AssembleContraption".to_string(), stub());
    registry.insert("Cascade".to_string(), stub());
    registry.insert("CopySpell".to_string(), stub());
    registry.insert("PlanarDiceResult".to_string(), stub());
    registry.insert("Planeswalk".to_string(), stub());

    registry
}

pub fn find_applicable_replacements(
    state: &GameState,
    event: &ProposedEvent,
    registry: &IndexMap<String, ReplacementHandlerEntry>,
) -> Vec<ReplacementId> {
    let mut candidates = Vec::new();

    // Scan battlefield + command zone objects for replacement_definitions
    let zones_to_scan = [Zone::Battlefield, Zone::Command];
    for obj in state.objects.values() {
        if !zones_to_scan.contains(&obj.zone) {
            continue;
        }
        for (index, repl_def) in obj.replacement_definitions.iter().enumerate() {
            let rid = ReplacementId {
                source: obj.id,
                index,
            };

            // Skip if already applied to this event
            if event.already_applied(&rid) {
                continue;
            }

            // Check if the registry has a handler for this event type
            if let Some(handler) = registry.get(&repl_def.event) {
                if (handler.matcher)(event, &repl_def.params, obj.id, state) {
                    candidates.push(rid);
                }
            }
        }
    }

    candidates
}

const MAX_REPLACEMENT_DEPTH: u16 = 16;

pub fn replace_event(
    state: &mut GameState,
    mut proposed: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let registry = build_replacement_registry();
    let mut depth: u16 = 0;

    loop {
        if depth >= MAX_REPLACEMENT_DEPTH {
            break;
        }

        let candidates = find_applicable_replacements(state, &proposed, &registry);

        if candidates.is_empty() {
            break;
        }

        if candidates.len() == 1 {
            let rid = candidates[0];
            proposed.mark_applied(rid);

            // Find the replacement definition and apply it
            if let Some(obj) = state.objects.get(&rid.source) {
                if let Some(repl_def) = obj.replacement_definitions.get(rid.index) {
                    let event_type = repl_def.event.clone();
                    let params = repl_def.params.clone();
                    if let Some(handler) = registry.get(&event_type) {
                        match (handler.applier)(proposed, &params, rid.source, state, events) {
                            ApplyResult::Modified(new_event) => {
                                events.push(GameEvent::ReplacementApplied {
                                    source_id: rid.source,
                                    event_type,
                                });
                                proposed = new_event;
                            }
                            ApplyResult::Prevented => {
                                events.push(GameEvent::ReplacementApplied {
                                    source_id: rid.source,
                                    event_type,
                                });
                                return ReplacementResult::Prevented;
                            }
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            // Multiple candidates -- need player choice
            let affected = proposed.affected_player(state);
            state.pending_replacement = Some(PendingReplacement {
                proposed,
                candidates,
                depth,
            });
            return ReplacementResult::NeedsChoice(affected);
        }

        depth += 1;
    }

    ReplacementResult::Execute(proposed)
}

pub fn continue_replacement(
    state: &mut GameState,
    chosen_index: usize,
    events: &mut Vec<GameEvent>,
) -> ReplacementResult {
    let pending = match state.pending_replacement.take() {
        Some(p) => p,
        None => return ReplacementResult::Execute(ProposedEvent::Draw {
            player_id: PlayerId(0),
            count: 0,
            applied: std::collections::HashSet::new(),
        }),
    };

    let registry = build_replacement_registry();

    if chosen_index >= pending.candidates.len() {
        // Invalid choice, just execute the event as-is
        return ReplacementResult::Execute(pending.proposed);
    }

    let rid = pending.candidates[chosen_index];
    let mut proposed = pending.proposed;
    proposed.mark_applied(rid);

    // Apply the chosen replacement
    if let Some(obj) = state.objects.get(&rid.source) {
        if let Some(repl_def) = obj.replacement_definitions.get(rid.index) {
            let event_type = repl_def.event.clone();
            let params = repl_def.params.clone();
            if let Some(handler) = registry.get(&event_type) {
                match (handler.applier)(proposed, &params, rid.source, state, events) {
                    ApplyResult::Modified(new_event) => {
                        events.push(GameEvent::ReplacementApplied {
                            source_id: rid.source,
                            event_type,
                        });
                        proposed = new_event;
                    }
                    ApplyResult::Prevented => {
                        events.push(GameEvent::ReplacementApplied {
                            source_id: rid.source,
                            event_type,
                        });
                        return ReplacementResult::Prevented;
                    }
                }
            } else {
                return ReplacementResult::Execute(proposed);
            }
        } else {
            return ReplacementResult::Execute(proposed);
        }
    } else {
        return ReplacementResult::Execute(proposed);
    }

    // Re-enter the pipeline loop for remaining candidates
    let mut depth = pending.depth + 1;

    loop {
        if depth >= MAX_REPLACEMENT_DEPTH {
            break;
        }

        let candidates = find_applicable_replacements(state, &proposed, &registry);

        if candidates.is_empty() {
            break;
        }

        if candidates.len() == 1 {
            let rid = candidates[0];
            proposed.mark_applied(rid);

            if let Some(obj) = state.objects.get(&rid.source) {
                if let Some(repl_def) = obj.replacement_definitions.get(rid.index) {
                    let event_type = repl_def.event.clone();
                    let params = repl_def.params.clone();
                    if let Some(handler) = registry.get(&event_type) {
                        match (handler.applier)(proposed, &params, rid.source, state, events) {
                            ApplyResult::Modified(new_event) => {
                                events.push(GameEvent::ReplacementApplied {
                                    source_id: rid.source,
                                    event_type,
                                });
                                proposed = new_event;
                            }
                            ApplyResult::Prevented => {
                                events.push(GameEvent::ReplacementApplied {
                                    source_id: rid.source,
                                    event_type,
                                });
                                return ReplacementResult::Prevented;
                            }
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            let affected = proposed.affected_player(state);
            state.pending_replacement = Some(PendingReplacement {
                proposed,
                candidates,
                depth,
            });
            return ReplacementResult::NeedsChoice(affected);
        }

        depth += 1;
    }

    ReplacementResult::Execute(proposed)
}
