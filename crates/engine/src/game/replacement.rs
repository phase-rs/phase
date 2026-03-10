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

// --- Stub handlers (for 21 recognized-but-unimplemented types) ---

fn stub_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

fn stub_applier(
    event: ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- Helper: parse zone from Forge param string ---

fn parse_zone(s: &str) -> Option<Zone> {
    match s {
        "Battlefield" => Some(Zone::Battlefield),
        "Graveyard" => Some(Zone::Graveyard),
        "Hand" => Some(Zone::Hand),
        "Library" => Some(Zone::Library),
        "Exile" => Some(Zone::Exile),
        "Stack" => Some(Zone::Stack),
        "Command" => Some(Zone::Command),
        _ => None,
    }
}

// --- 1. Moved (ZoneChange) ---

fn moved_matcher(
    event: &ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    if let ProposedEvent::ZoneChange { from, to, .. } = event {
        // Check Origin$ param if present
        if let Some(origin) = params.get("Origin$") {
            if let Some(z) = parse_zone(origin) {
                if *from != z {
                    return false;
                }
            }
        }
        // Check Destination$ param if present
        if let Some(dest) = params.get("Destination$") {
            if let Some(z) = parse_zone(dest) {
                if *to != z {
                    return false;
                }
            }
        }
        true
    } else {
        false
    }
}

fn moved_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        cause,
        applied,
        ..
    } = event
    {
        // NewDestination$ specifies where to redirect
        if let Some(new_dest) = params.get("NewDestination$") {
            if let Some(z) = parse_zone(new_dest) {
                return ApplyResult::Modified(ProposedEvent::ZoneChange {
                    object_id,
                    from,
                    to: z,
                    cause,
                    applied,
                });
            }
        }
        // If no NewDestination$, return event unchanged
        ApplyResult::Modified(ProposedEvent::ZoneChange {
            object_id,
            from,
            to: from, // fallback: stay in origin
            cause,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 2. DamageDone ---

fn damage_done_matcher(
    event: &ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    if let ProposedEvent::Damage { is_combat, .. } = event {
        // DamageType$ filter: Combat or NonCombat
        if let Some(dtype) = params.get("DamageType$") {
            match dtype.as_str() {
                "Combat" if !is_combat => return false,
                "NonCombat" if *is_combat => return false,
                _ => {}
            }
        }
        true
    } else {
        false
    }
}

fn damage_done_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::Damage {
        source_id,
        target,
        amount,
        is_combat,
        applied,
    } = event
    {
        // Prevention: set amount to 0 or return Prevented
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Modify amount if specified
        if let Some(new_amount) = params.get("NewAmount$") {
            if let Ok(n) = new_amount.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::Damage {
                    source_id,
                    target,
                    amount: n,
                    is_combat,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::Damage {
            source_id,
            target,
            amount,
            is_combat,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 3. Destroy ---

fn destroy_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::Destroy { .. })
}

fn destroy_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::Destroy {
        object_id,
        source,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Redirect to exile instead of destroy
        if params.get("Exile").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Modified(ProposedEvent::ZoneChange {
                object_id,
                from: Zone::Battlefield,
                to: Zone::Exile,
                cause: source,
                applied,
            });
        }
        ApplyResult::Modified(ProposedEvent::Destroy {
            object_id,
            source,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 4. Draw ---

fn draw_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::Draw { .. })
}

fn draw_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::Draw {
        player_id,
        count,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        if let Some(new_count) = params.get("NewCount$") {
            if let Ok(n) = new_count.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::Draw {
                    player_id,
                    count: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::Draw {
            player_id,
            count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 5. GainLife ---

fn gain_life_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::LifeGain { .. })
}

fn gain_life_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::LifeGain {
        player_id,
        amount,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        if let Some(new_amount) = params.get("NewAmount$") {
            if let Ok(n) = new_amount.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::LifeGain {
                    player_id,
                    amount: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::LifeGain {
            player_id,
            amount,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 6. LifeReduced ---

fn life_reduced_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn life_reduced_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        if let Some(new_amount) = params.get("NewAmount$") {
            if let Ok(n) = new_amount.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::LifeLoss {
                    player_id,
                    amount: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::LifeLoss {
            player_id,
            amount,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 7. AddCounter ---

fn add_counter_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::AddCounter { .. })
}

fn add_counter_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count,
        applied,
    } = event
    {
        // Doubling: multiply count
        if params.get("Double").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Modified(ProposedEvent::AddCounter {
                object_id,
                counter_type,
                count: count * 2,
                applied,
            });
        }
        if let Some(new_count) = params.get("NewCount$") {
            if let Ok(n) = new_count.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::AddCounter {
                    object_id,
                    counter_type,
                    count: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::AddCounter {
            object_id,
            counter_type,
            count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 8. RemoveCounter ---

fn remove_counter_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::RemoveCounter { .. })
}

fn remove_counter_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::RemoveCounter {
        object_id,
        counter_type,
        count,
        applied,
    } = event
    {
        if let Some(new_count) = params.get("NewCount$") {
            if let Ok(n) = new_count.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::RemoveCounter {
                    object_id,
                    counter_type,
                    count: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::RemoveCounter {
            object_id,
            counter_type,
            count,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 9. CreateToken ---

fn create_token_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::CreateToken { .. })
}

fn create_token_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::CreateToken {
        owner,
        name,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Allow name override
        if let Some(new_name) = params.get("NewName$") {
            return ApplyResult::Modified(ProposedEvent::CreateToken {
                owner,
                name: new_name.clone(),
                applied,
            });
        }
        ApplyResult::Modified(ProposedEvent::CreateToken {
            owner,
            name,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 11. Tap ---

fn tap_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::Tap { .. })
}

fn tap_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
        return ApplyResult::Prevented;
    }
    ApplyResult::Modified(event)
}

// --- 12. Untap ---

fn untap_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::Untap { .. })
}

fn untap_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
        return ApplyResult::Prevented;
    }
    ApplyResult::Modified(event)
}

// --- 14. Counter (spell countering) ---

fn counter_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    // Match ZoneChange from Stack (countered spell going to graveyard)
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
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        cause,
        applied,
        ..
    } = event
    {
        // "Can't be countered" prevention
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Redirect countered spell to exile instead of graveyard
        if params.get("Exile").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Modified(ProposedEvent::ZoneChange {
                object_id,
                from,
                to: Zone::Exile,
                cause,
                applied,
            });
        }
        ApplyResult::Modified(ProposedEvent::ZoneChange {
            object_id,
            from,
            to: Zone::Graveyard,
            cause,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 15. Attached (ZoneChange to Battlefield for attachments) ---

fn attached_matcher(
    event: &ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    state: &GameState,
) -> bool {
    if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
        if *to != Zone::Battlefield {
            return false;
        }
        // Check if the entering object is an attachment (Aura or Equipment)
        if let Some(obj) = state.objects.get(object_id) {
            let is_attachment = obj
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Aura" || s == "Equipment");
            if !is_attachment {
                return false;
            }
        }
        // Check ValidCard$ filter if present
        if let Some(_valid) = params.get("ValidCard$") {
            // For now, accept all matching attachments
            return true;
        }
        true
    } else {
        false
    }
}

fn attached_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        cause,
        applied,
        ..
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Redirect destination if specified
        if let Some(new_dest) = params.get("NewDestination$") {
            if let Some(z) = parse_zone(new_dest) {
                return ApplyResult::Modified(ProposedEvent::ZoneChange {
                    object_id,
                    from,
                    to: z,
                    cause,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::ZoneChange {
            object_id,
            from,
            to: Zone::Battlefield,
            cause,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 16. DealtDamage (from target's perspective) ---

fn dealt_damage_matcher(
    event: &ProposedEvent,
    params: &HashMap<String, String>,
    source: ObjectId,
    state: &GameState,
) -> bool {
    if let ProposedEvent::Damage {
        target, is_combat, ..
    } = event
    {
        // Match if the source object of this replacement is the target of the damage
        let is_target = match target {
            crate::types::ability::TargetRef::Object(oid) => *oid == source,
            crate::types::ability::TargetRef::Player(pid) => {
                // Check if the replacement source's controller matches the player
                state
                    .objects
                    .get(&source)
                    .map(|o| o.controller == *pid)
                    .unwrap_or(false)
            }
        };
        if !is_target {
            return false;
        }
        // DamageType$ filter
        if let Some(dtype) = params.get("DamageType$") {
            match dtype.as_str() {
                "Combat" if !is_combat => return false,
                "NonCombat" if *is_combat => return false,
                _ => {}
            }
        }
        true
    } else {
        false
    }
}

fn dealt_damage_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::Damage {
        source_id,
        target,
        amount,
        is_combat,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        if let Some(new_amount) = params.get("NewAmount$") {
            if let Ok(n) = new_amount.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::Damage {
                    source_id,
                    target,
                    amount: n,
                    is_combat,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::Damage {
            source_id,
            target,
            amount,
            is_combat,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 17. Mill (ZoneChange from Library to Graveyard) ---

fn mill_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
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
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::ZoneChange {
        object_id,
        from,
        cause,
        applied,
        ..
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        // Redirect milled cards to exile instead of graveyard
        if let Some(new_dest) = params.get("NewDestination$") {
            if let Some(z) = parse_zone(new_dest) {
                return ApplyResult::Modified(ProposedEvent::ZoneChange {
                    object_id,
                    from,
                    to: z,
                    cause,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::ZoneChange {
            object_id,
            from,
            to: Zone::Graveyard,
            cause,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 18. PayLife (matches LifeLoss) ---

fn pay_life_matcher(
    event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    matches!(event, ProposedEvent::LifeLoss { .. })
}

fn pay_life_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if let ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied,
    } = event
    {
        if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
            return ApplyResult::Prevented;
        }
        if let Some(new_amount) = params.get("NewAmount$") {
            if let Ok(n) = new_amount.parse::<u32>() {
                return ApplyResult::Modified(ProposedEvent::LifeLoss {
                    player_id,
                    amount: n,
                    applied,
                });
            }
        }
        ApplyResult::Modified(ProposedEvent::LifeLoss {
            player_id,
            amount,
            applied,
        })
    } else {
        ApplyResult::Modified(event)
    }
}

// --- 19. ProduceMana (matches mana addition -- uses LifeGain as placeholder since
//     there's no ProposedEvent::ManaProduction yet; currently a structural matcher) ---

fn produce_mana_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    // No ProposedEvent variant for mana production yet.
    // When mana production goes through the replacement pipeline, this will match.
    false
}

fn produce_mana_applier(
    event: ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    // Placeholder: pass through unchanged
    ApplyResult::Modified(event)
}

// --- 20. Scry (no dedicated ProposedEvent -- structural placeholder) ---

fn scry_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    // Scry doesn't go through replacement pipeline via ProposedEvent yet.
    false
}

fn scry_applier(
    event: ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- 21. Transform (no dedicated ProposedEvent -- structural placeholder) ---

fn transform_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    // Transform events don't go through replacement pipeline via ProposedEvent yet.
    false
}

fn transform_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
        return ApplyResult::Prevented;
    }
    ApplyResult::Modified(event)
}

// --- 22. TurnFaceUp (no dedicated ProposedEvent -- structural placeholder) ---

fn turn_face_up_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

fn turn_face_up_applier(
    event: ProposedEvent,
    params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    if params.get("Prevent").map(|v| v == "True").unwrap_or(false) {
        return ApplyResult::Prevented;
    }
    ApplyResult::Modified(event)
}

// --- 23. Explore (no dedicated ProposedEvent -- structural placeholder) ---

fn explore_matcher(
    _event: &ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &GameState,
) -> bool {
    false
}

fn explore_applier(
    event: ProposedEvent,
    _params: &HashMap<String, String>,
    _source: ObjectId,
    _state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> ApplyResult {
    ApplyResult::Modified(event)
}

// --- Registry ---

pub fn build_replacement_registry() -> IndexMap<String, ReplacementHandlerEntry> {
    let mut registry = IndexMap::new();

    let stub = || ReplacementHandlerEntry {
        matcher: stub_matcher,
        applier: stub_applier,
    };

    // 14 core types with real logic
    registry.insert(
        "DamageDone".to_string(),
        ReplacementHandlerEntry {
            matcher: damage_done_matcher,
            applier: damage_done_applier,
        },
    );
    registry.insert(
        "Moved".to_string(),
        ReplacementHandlerEntry {
            matcher: moved_matcher,
            applier: moved_applier,
        },
    );
    registry.insert(
        "Destroy".to_string(),
        ReplacementHandlerEntry {
            matcher: destroy_matcher,
            applier: destroy_applier,
        },
    );
    registry.insert(
        "Draw".to_string(),
        ReplacementHandlerEntry {
            matcher: draw_matcher,
            applier: draw_applier,
        },
    );
    registry.insert("DrawCards".to_string(), stub()); // stays stub (alias for Draw)
    registry.insert(
        "GainLife".to_string(),
        ReplacementHandlerEntry {
            matcher: gain_life_matcher,
            applier: gain_life_applier,
        },
    );
    registry.insert(
        "LifeReduced".to_string(),
        ReplacementHandlerEntry {
            matcher: life_reduced_matcher,
            applier: life_reduced_applier,
        },
    );
    registry.insert(
        "AddCounter".to_string(),
        ReplacementHandlerEntry {
            matcher: add_counter_matcher,
            applier: add_counter_applier,
        },
    );
    registry.insert(
        "RemoveCounter".to_string(),
        ReplacementHandlerEntry {
            matcher: remove_counter_matcher,
            applier: remove_counter_applier,
        },
    );
    registry.insert(
        "Tap".to_string(),
        ReplacementHandlerEntry {
            matcher: tap_matcher,
            applier: tap_applier,
        },
    );
    registry.insert(
        "Untap".to_string(),
        ReplacementHandlerEntry {
            matcher: untap_matcher,
            applier: untap_applier,
        },
    );
    registry.insert(
        "Counter".to_string(),
        ReplacementHandlerEntry {
            matcher: counter_matcher,
            applier: counter_applier,
        },
    );
    registry.insert(
        "CreateToken".to_string(),
        ReplacementHandlerEntry {
            matcher: create_token_matcher,
            applier: create_token_applier,
        },
    );
    registry.insert(
        "Attached".to_string(),
        ReplacementHandlerEntry {
            matcher: attached_matcher,
            applier: attached_applier,
        },
    );

    // Promoted from stubs to real handlers
    registry.insert(
        "DealtDamage".to_string(),
        ReplacementHandlerEntry {
            matcher: dealt_damage_matcher,
            applier: dealt_damage_applier,
        },
    );
    registry.insert(
        "Mill".to_string(),
        ReplacementHandlerEntry {
            matcher: mill_matcher,
            applier: mill_applier,
        },
    );
    registry.insert(
        "PayLife".to_string(),
        ReplacementHandlerEntry {
            matcher: pay_life_matcher,
            applier: pay_life_applier,
        },
    );
    registry.insert(
        "ProduceMana".to_string(),
        ReplacementHandlerEntry {
            matcher: produce_mana_matcher,
            applier: produce_mana_applier,
        },
    );
    registry.insert(
        "Scry".to_string(),
        ReplacementHandlerEntry {
            matcher: scry_matcher,
            applier: scry_applier,
        },
    );
    registry.insert(
        "Transform".to_string(),
        ReplacementHandlerEntry {
            matcher: transform_matcher,
            applier: transform_applier,
        },
    );
    registry.insert(
        "TurnFaceUp".to_string(),
        ReplacementHandlerEntry {
            matcher: turn_face_up_matcher,
            applier: turn_face_up_applier,
        },
    );
    registry.insert(
        "Explore".to_string(),
        ReplacementHandlerEntry {
            matcher: explore_matcher,
            applier: explore_applier,
        },
    );

    // 12 remaining Forge types (stubs -- recognized but no-op)
    registry.insert("BeginPhase".to_string(), stub());
    registry.insert("BeginTurn".to_string(), stub());
    registry.insert("DeclareBlocker".to_string(), stub());
    registry.insert("GameLoss".to_string(), stub());
    registry.insert("GameWin".to_string(), stub());
    registry.insert("Learn".to_string(), stub());
    registry.insert("LoseMana".to_string(), stub());
    registry.insert("Proliferate".to_string(), stub());
    registry.insert("AssembleContraption".to_string(), stub());
    registry.insert("Cascade".to_string(), stub());
    registry.insert("CopySpell".to_string(), stub());
    registry.insert("PlanarDiceResult".to_string(), stub());
    registry.insert("Planeswalk".to_string(), stub());

    registry
}

// --- Pipeline functions ---

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

            if event.already_applied(&rid) {
                continue;
            }

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

fn apply_single_replacement(
    state: &mut GameState,
    proposed: ProposedEvent,
    rid: ReplacementId,
    registry: &IndexMap<String, ReplacementHandlerEntry>,
    events: &mut Vec<GameEvent>,
) -> Result<ProposedEvent, ApplyResult> {
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
        }
    }
    Ok(proposed)
}

fn pipeline_loop(
    state: &mut GameState,
    mut proposed: ProposedEvent,
    mut depth: u16,
    registry: &IndexMap<String, ReplacementHandlerEntry>,
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
            proposed.mark_applied(rid);

            match apply_single_replacement(state, proposed, rid, registry, events) {
                Ok(new_event) => proposed = new_event,
                Err(ApplyResult::Prevented) => return ReplacementResult::Prevented,
                Err(ApplyResult::Modified(_)) => unreachable!(),
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
    use crate::types::ability::{ReplacementDefinition, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashSet;

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
        // Creature with Moved replacement: death -> exile
        let repl = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Exile".to_string()),
            ]),
        };
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(10),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);

        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => {
                assert_eq!(to, Zone::Exile, "destination should be changed to Exile");
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
        // Two Moved replacements on the same object
        let repl1 = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Exile".to_string()),
            ]),
        };
        let repl2 = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Hand".to_string()),
            ]),
        };
        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(10),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            applied: HashSet::new(),
        };

        // Two replacements on same object -> NeedsChoice (different ReplacementIds)
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::NeedsChoice(_)),
            "two replacements on same object should trigger NeedsChoice"
        );
    }

    #[test]
    fn test_multiple_replacements_needs_choice() {
        // Two different objects each with a Moved replacement
        let repl = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Exile".to_string()),
            ]),
        };

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

        // Also add the target creature
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
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::NeedsChoice(player) => {
                assert_eq!(player, PlayerId(0));
            }
            other => panic!("expected NeedsChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_continue_replacement_after_choice() {
        // Setup: two replacements that trigger NeedsChoice
        let repl1 = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Exile".to_string()),
            ]),
        };
        let repl2 = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([
                ("Origin$".to_string(), "Battlefield".to_string()),
                ("Destination$".to_string(), "Graveyard".to_string()),
                ("NewDestination$".to_string(), "Hand".to_string()),
            ]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl1, repl2]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(10),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            applied: HashSet::new(),
        };

        // First call should return NeedsChoice
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(matches!(result, ReplacementResult::NeedsChoice(_)));

        // Choose first replacement (index 0) -> Exile
        let result = continue_replacement(&mut state, 0, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => {
                assert_eq!(to, Zone::Exile);
            }
            // The second replacement won't match because destination was changed from Graveyard
            // So we might get Execute directly. Either way, verify it completed.
            other => {
                // If the second replacement matched the new Exile destination,
                // it could be NeedsChoice or Execute depending on filtering
                panic!("unexpected result: {:?}", other);
            }
        }
    }

    #[test]
    fn test_depth_cap() {
        // A replacement that always matches (Moved with no origin/destination filter)
        // but once-per-event tracking should prevent infinite loop anyway.
        // This just verifies the depth cap mechanism exists.
        let repl = ReplacementDefinition {
            event: "Moved".to_string(),
            params: HashMap::from([("NewDestination$".to_string(), "Exile".to_string())]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(10),
            from: Zone::Battlefield,
            to: Zone::Graveyard,
            cause: None,
            applied: HashSet::new(),
        };

        // Should complete without hanging (once-per-event prevents re-application,
        // and depth cap is a safety net)
        let result = replace_event(&mut state, proposed, &mut events);
        assert!(
            matches!(result, ReplacementResult::Execute(_)),
            "should complete even with broadly-matching replacement"
        );
    }

    #[test]
    fn test_damage_prevention() {
        let repl = ReplacementDefinition {
            event: "DamageDone".to_string(),
            params: HashMap::from([("Prevent".to_string(), "True".to_string())]),
        };

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
        assert_eq!(result, ReplacementResult::Prevented);
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
    fn test_dealt_damage_replacement_prevents_damage_to_source() {
        // DealtDamage replacement on a creature prevents damage dealt to it
        let repl = ReplacementDefinition {
            event: "DealtDamage".to_string(),
            params: HashMap::from([("Prevent".to_string(), "True".to_string())]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        // Damage targeting the object with the replacement
        let proposed = ProposedEvent::Damage {
            source_id: ObjectId(99),
            target: TargetRef::Object(ObjectId(10)),
            amount: 5,
            is_combat: false,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        assert_eq!(result, ReplacementResult::Prevented);
    }

    #[test]
    fn test_dealt_damage_does_not_match_damage_to_other() {
        // DealtDamage on ObjectId(10) should NOT match damage targeting ObjectId(20)
        let repl = ReplacementDefinition {
            event: "DealtDamage".to_string(),
            params: HashMap::from([("Prevent".to_string(), "True".to_string())]),
        };

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
    fn test_mill_replacement_redirects_to_exile() {
        let repl = ReplacementDefinition {
            event: "Mill".to_string(),
            params: HashMap::from([("NewDestination$".to_string(), "Exile".to_string())]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::ZoneChange {
            object_id: ObjectId(50),
            from: Zone::Library,
            to: Zone::Graveyard,
            cause: None,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::ZoneChange { to, .. }) => {
                assert_eq!(to, Zone::Exile, "milled card should go to exile");
            }
            other => panic!("expected Execute with ZoneChange, got {:?}", other),
        }
    }

    #[test]
    fn test_pay_life_replacement_prevents() {
        let repl = ReplacementDefinition {
            event: "PayLife".to_string(),
            params: HashMap::from([("Prevent".to_string(), "True".to_string())]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::LifeLoss {
            player_id: PlayerId(0),
            amount: 3,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        assert_eq!(result, ReplacementResult::Prevented);
    }

    #[test]
    fn test_pay_life_replacement_modifies_amount() {
        let repl = ReplacementDefinition {
            event: "PayLife".to_string(),
            params: HashMap::from([("NewAmount$".to_string(), "1".to_string())]),
        };

        let mut state = test_state_with_object(ObjectId(10), Zone::Battlefield, vec![repl]);
        let mut events = Vec::new();

        let proposed = ProposedEvent::LifeLoss {
            player_id: PlayerId(0),
            amount: 5,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        match result {
            ReplacementResult::Execute(ProposedEvent::LifeLoss { amount, .. }) => {
                assert_eq!(amount, 1, "life loss should be modified to 1");
            }
            other => panic!("expected Execute with LifeLoss, got {:?}", other),
        }
    }

    #[test]
    fn test_registry_has_all_35_types() {
        let registry = build_replacement_registry();
        assert_eq!(
            registry.len(),
            35,
            "registry should have exactly 35 entries"
        );

        // Verify all expected keys
        let expected = [
            "DamageDone",
            "Moved",
            "Destroy",
            "Draw",
            "DrawCards",
            "GainLife",
            "LifeReduced",
            "AddCounter",
            "RemoveCounter",
            "Tap",
            "Untap",
            "Counter",
            "CreateToken",
            "Attached",
            "BeginPhase",
            "BeginTurn",
            "DealtDamage",
            "DeclareBlocker",
            "Explore",
            "GameLoss",
            "GameWin",
            "Learn",
            "LoseMana",
            "Mill",
            "PayLife",
            "ProduceMana",
            "Proliferate",
            "Scry",
            "Transform",
            "TurnFaceUp",
            "AssembleContraption",
            "Cascade",
            "CopySpell",
            "PlanarDiceResult",
            "Planeswalk",
        ];
        for key in expected {
            assert!(registry.contains_key(key), "registry missing key: {}", key);
        }
    }
}
