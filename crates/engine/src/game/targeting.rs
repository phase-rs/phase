use crate::types::ability::{FilterProp, TargetFilter, TargetRef, TypedFilter};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::{HexproofFilter, Keyword};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::collections::HashSet;

/// Find legal targets using a typed TargetFilter (CR 115.2 + CR 702.16b).
///
/// Evaluates battlefield objects against the filter using the typed filter system,
/// and includes players/stack spells where appropriate.
pub fn find_legal_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    use crate::types::ability::ControllerRef;
    let mut targets = Vec::new();

    // StackAbility: only match non-mana activated/triggered abilities on the stack
    if matches!(filter, TargetFilter::StackAbility) {
        add_stack_abilities(state, source_id, &mut targets);
        return targets;
    }

    // SpecificObject is runtime-bound (not used for target selection)
    if matches!(filter, TargetFilter::SpecificObject { .. }) {
        return targets;
    }

    // ParentTarget inherits targets from the parent ability at resolution time.
    // No new targeting needed — the sub_ability chain copies parent targets automatically.
    if matches!(filter, TargetFilter::ParentTarget) {
        return targets;
    }

    // Check if filter could match players
    if matches!(filter, TargetFilter::Any | TargetFilter::Player) {
        add_players(state, &mut targets);
    }

    // Typed filter with no type_filters targets players, not permanents.
    // e.g. "target opponent" → Typed { type_filters: [], controller: Opponent }
    if let TargetFilter::Typed(ref tf) = filter {
        if tf.type_filters.is_empty() {
            let controller = &tf.controller;
            for player in &state.players {
                // Player-phasing exclusion (mirrors CR 702.26b for permanents).
                if player.is_phased_out() {
                    continue;
                }
                // CR 702.16b + CR 702.16j: A player with protection from
                // everything can't be targeted by spells or abilities.
                if super::static_abilities::player_has_protection_from_everything(state, player.id)
                {
                    continue;
                }
                let is_opponent = player.id != source_controller;
                let include = match controller {
                    Some(ControllerRef::Opponent) => is_opponent,
                    Some(ControllerRef::You) => !is_opponent,
                    // CR 109.4: TargetPlayer is nonsensical when enumerating target
                    // candidates (the "target player" is what's being chosen here).
                    // Fail closed.
                    Some(ControllerRef::TargetPlayer) => false,
                    None => true,
                };
                if include {
                    targets.push(TargetRef::Player(player.id));
                }
            }
            return targets;
        }
    }

    let explicit_zones = extract_explicit_zones(filter);

    let target_ctx =
        super::filter::FilterContext::from_source_with_controller(source_id, source_controller);
    if !explicit_zones.is_empty() {
        // Explicit zone search: ONLY search the specified zones
        for zone in &explicit_zones {
            match zone {
                Zone::Battlefield => {
                    for &obj_id in &state.battlefield {
                        if super::filter::matches_target_filter(state, obj_id, filter, &target_ctx)
                        {
                            let obj = match state.objects.get(&obj_id) {
                                Some(o) => o,
                                None => continue,
                            };
                            if can_target(obj, source_controller, source_id, state) {
                                targets.push(TargetRef::Object(obj_id));
                            }
                        }
                    }
                }
                Zone::Exile => add_zone_targets(
                    state,
                    state.exile.iter().copied(),
                    filter,
                    source_controller,
                    source_id,
                    false,
                    &mut targets,
                ),
                Zone::Graveyard => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.graveyard.iter().copied(),
                            filter,
                            source_controller,
                            source_id,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Hand => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.hand.iter().copied(),
                            filter,
                            source_controller,
                            source_id,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Library => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.library.iter().copied(),
                            filter,
                            source_controller,
                            source_id,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Stack => {
                    for entry in &state.stack {
                        let obj_id = entry.id;
                        if super::filter::matches_target_filter(state, obj_id, filter, &target_ctx)
                        {
                            let obj = match state.objects.get(&obj_id) {
                                Some(o) => o,
                                None => continue,
                            };
                            if !is_protected_from(obj, source_id, state) {
                                targets.push(TargetRef::Object(obj_id));
                            }
                        }
                    }
                }
                Zone::Command => {}
            }
        }
    } else {
        // No explicit zone: default behavior (battlefield + stack for Card type)
        if filter_targets_stack_spells(filter) {
            add_stack_spells(state, filter, source_controller, source_id, &mut targets);
        }

        for &obj_id in &state.battlefield {
            if super::filter::matches_target_filter(state, obj_id, filter, &target_ctx) {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                if can_target(obj, source_controller, source_id, state) {
                    targets.push(TargetRef::Object(obj_id));
                }
            }
        }
    }

    targets
}

/// Recheck targets on resolution using typed filter, returns only still-legal targets.
pub fn validate_targets(
    state: &GameState,
    targets: &[TargetRef],
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    let legal = find_legal_targets(state, filter, source_controller, source_id);
    if legal.len() <= 8 {
        targets
            .iter()
            .filter(|t| legal.contains(t))
            .cloned()
            .collect()
    } else {
        let legal_set: HashSet<TargetRef> = legal.into_iter().collect();
        targets
            .iter()
            .filter(|t| legal_set.contains(*t))
            .cloned()
            .collect()
    }
}

/// Returns true if ALL original targets are now illegal (spell fizzles per CR 608.2b).
pub fn check_fizzle(original_targets: &[TargetRef], legal_targets: &[TargetRef]) -> bool {
    if original_targets.is_empty() {
        return false; // Spells with no targets never fizzle
    }
    legal_targets.is_empty()
}

/// Resolve event-context TargetFilter variants using the current trigger event.
/// These variants auto-resolve at effect resolution time from `state.current_trigger_event`
/// without requiring player selection (CR 603.7c).
///
/// Returns `Some(TargetRef)` if the event context can provide a target,
/// `None` if the filter is not an event-context variant or no event is available.
pub fn resolve_event_context_target(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> Option<TargetRef> {
    match filter {
        TargetFilter::TriggeringSpellController => {
            let event = state.current_trigger_event.as_ref()?;
            let source_obj_id = extract_source_from_event(event)?;
            let controller = state.objects.get(&source_obj_id)?.controller;
            Some(TargetRef::Player(controller))
        }
        TargetFilter::TriggeringSpellOwner => {
            let event = state.current_trigger_event.as_ref()?;
            let source_obj_id = extract_source_from_event(event)?;
            let owner = state.objects.get(&source_obj_id)?.owner;
            Some(TargetRef::Player(owner))
        }
        TargetFilter::TriggeringPlayer => {
            let event = state.current_trigger_event.as_ref()?;
            let player = extract_player_from_event(event, state)?;
            Some(TargetRef::Player(player))
        }
        TargetFilter::TriggeringSource => {
            let event = state.current_trigger_event.as_ref()?;
            let obj_id = extract_source_from_event(event)?;
            Some(TargetRef::Object(obj_id))
        }
        // CR 506.3d: "defending player" — look up from combat state using the source creature.
        TargetFilter::DefendingPlayer => {
            let combat = state.combat.as_ref()?;
            let attacker_info = combat.attackers.iter().find(|a| a.object_id == source_id)?;
            Some(TargetRef::Player(attacker_info.defending_player))
        }
        TargetFilter::ParentTargetController => {
            let event = state.current_trigger_event.as_ref()?;
            let source_obj_id = extract_source_from_event(event)?;
            let controller = state.objects.get(&source_obj_id)?.controller;
            Some(TargetRef::Player(controller))
        }
        _ => None,
    }
}

/// Extract the source object ID from a trigger event.
pub(crate) fn extract_source_from_event(
    event: &crate::types::events::GameEvent,
) -> Option<ObjectId> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::BecomesTarget { source_id, .. } => Some(*source_id),
        GameEvent::SpellCast { object_id, .. } => Some(*object_id),
        GameEvent::DamageDealt { source_id, .. } => Some(*source_id),
        GameEvent::AbilityActivated { source_id } => Some(*source_id),
        GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
        GameEvent::PermanentTapped { object_id, .. } => Some(*object_id),
        GameEvent::PermanentUntapped { object_id } => Some(*object_id),
        GameEvent::CounterAdded { object_id, .. } => Some(*object_id),
        GameEvent::CounterRemoved { object_id, .. } => Some(*object_id),
        GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
        GameEvent::CreatureDestroyed { object_id } => Some(*object_id),
        GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
        GameEvent::Transformed { object_id } => Some(*object_id),
        GameEvent::TurnedFaceUp { object_id } => Some(*object_id),
        GameEvent::Cycled { object_id, .. } => Some(*object_id),
        GameEvent::CreatureSuspected { object_id } => Some(*object_id),
        GameEvent::CaseSolved { object_id } => Some(*object_id),
        GameEvent::AttackersDeclared { attacker_ids, .. } if attacker_ids.len() == 1 => {
            attacker_ids.first().copied()
        }
        _ => None,
    }
}

/// Extract the relevant player from a trigger event.
pub(crate) fn extract_player_from_event(
    event: &crate::types::events::GameEvent,
    state: &GameState,
) -> Option<PlayerId> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::LifeChanged { player_id, .. } => Some(*player_id),
        GameEvent::CardsDrawn { player_id, .. } => Some(*player_id),
        GameEvent::CardDrawn { player_id, .. } => Some(*player_id),
        GameEvent::Discarded { player_id, .. } => Some(*player_id),
        GameEvent::LandPlayed { player_id, .. } => Some(*player_id),
        GameEvent::SpellCast { controller, .. } => Some(*controller),
        GameEvent::PermanentSacrificed { player_id, .. } => Some(*player_id),
        GameEvent::Cycled { player_id, .. } => Some(*player_id),
        GameEvent::PlayerPerformedAction { player_id, .. } => Some(*player_id),
        GameEvent::CrimeCommitted { player_id, .. } => Some(*player_id),
        GameEvent::PlayerEliminated { player_id, .. } => Some(*player_id),
        // CR 506.2 + CR 508.1: The attacking player is the common controller of the
        // declared attackers in this batch. All attackers in one AttackersDeclared
        // batch share the active player as their controller.
        GameEvent::AttackersDeclared { attacker_ids, .. } => attacker_ids
            .iter()
            .find_map(|id| state.objects.get(id).map(|obj| obj.controller)),
        // For object-centric events, extract the controller
        GameEvent::BecomesTarget { source_id, .. } => {
            state.objects.get(source_id).map(|obj| obj.controller)
        }
        // CR 603.7c: "that player" for DamageDone triggers refers to the damaged player.
        GameEvent::DamageDealt { target, .. } => match target {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(oid) => state.objects.get(oid).map(|obj| obj.controller),
        },
        // CR 500.2 + CR 603.7c: Phase-change triggers like "at the beginning of
        // each player's upkeep" bind "that player" / `TriggeringPlayer` to the
        // active player — the player whose phase is currently beginning.
        // Without this, Ruthless Winnower ("that player sacrifices a non-Elf
        // creature") would have no player anchor and the sacrifice filter
        // would match across all players.
        GameEvent::PhaseChanged { .. } => Some(state.active_player),
        _ => None,
    }
}

/// CR 603.7c: Extract a numeric amount from a trigger event.
/// Returns the quantity relevant to the event type (damage dealt, life changed, etc.).
pub(crate) fn extract_amount_from_event(event: &crate::types::events::GameEvent) -> Option<i32> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::DamageDealt { amount, .. } => Some(*amount as i32),
        GameEvent::LifeChanged { amount, .. } => Some(amount.abs()),
        GameEvent::CardsDrawn { count, .. } => Some(*count as i32),
        GameEvent::CounterAdded { count, .. } => Some(*count as i32),
        GameEvent::CounterRemoved { count, .. } => Some(*count as i32),
        GameEvent::Discarded { .. } => Some(1),
        _ => None,
    }
}

// --- Internal helpers ---

/// Find activated/triggered (non-mana) abilities on the stack as legal targets.
/// Mana abilities never go on the stack, so all ActivatedAbility/TriggeredAbility
/// entries are valid. Excludes the source ability itself.
fn add_stack_abilities(state: &GameState, source_id: ObjectId, targets: &mut Vec<TargetRef>) {
    use crate::types::game_state::StackEntryKind;
    for entry in &state.stack {
        if entry.id == source_id {
            continue; // Don't target yourself
        }
        match &entry.kind {
            // CR 113.3b: Activated keyword abilities (Crew / Station / Equip / Saddle)
            // are activated abilities and are targetable by counterspell-class effects
            // that filter "activated or triggered ability on the stack".
            StackEntryKind::ActivatedAbility { .. }
            | StackEntryKind::TriggeredAbility { .. }
            | StackEntryKind::KeywordAction { .. } => {
                targets.push(TargetRef::Object(entry.id));
            }
            StackEntryKind::Spell { .. } => {}
        }
    }
}

fn add_zone_targets(
    state: &GameState,
    object_ids: impl IntoIterator<Item = ObjectId>,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    require_full_targeting: bool,
    targets: &mut Vec<TargetRef>,
) {
    let ctx =
        super::filter::FilterContext::from_source_with_controller(source_id, source_controller);
    for obj_id in object_ids {
        if super::filter::matches_target_filter(state, obj_id, filter, &ctx) {
            let obj = match state.objects.get(&obj_id) {
                Some(o) => o,
                None => continue,
            };
            if require_full_targeting {
                if can_target(obj, source_controller, source_id, state) {
                    targets.push(TargetRef::Object(obj_id));
                }
            } else if !is_protected_from(obj, source_id, state) {
                targets.push(TargetRef::Object(obj_id));
            }
        }
    }
}

fn add_stack_spells(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    targets: &mut Vec<TargetRef>,
) {
    let requires_single_target = filter_requires_single_target(filter);
    let targets_only_constraint = super::filter::extract_targets_only(filter);
    let targets_constraint = super::filter::extract_targets(filter);
    let source_controller_opt = state.objects.get(&source_id).map(|o| o.controller);

    for entry in &state.stack {
        if !matches!(
            entry.kind,
            crate::types::game_state::StackEntryKind::Spell { .. }
        ) {
            continue;
        }
        // CR 115.7: "with a single target" — only match stack entries with exactly one target.
        if requires_single_target {
            let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
            if targets.len() != 1 {
                continue;
            }
        }
        let bare_ctx = super::filter::FilterContext::from_source(state, source_id);
        // CR 115.9c: "that targets only [X]" — all targets must match the constraint filter.
        if let Some(ref constraint) = targets_only_constraint {
            let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
            if targets.is_empty()
                || !targets.iter().all(|t| match t {
                    TargetRef::Object(id) => {
                        super::filter::matches_target_filter(state, *id, constraint, &bare_ctx)
                    }
                    TargetRef::Player(pid) => super::filter::player_matches_target_filter(
                        constraint,
                        *pid,
                        source_controller_opt,
                    ),
                })
            {
                continue;
            }
        }
        // CR 115.9b: "that targets [X]" — at least one target must match (.any() semantics).
        if let Some(ref constraint) = targets_constraint {
            let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
            if targets.is_empty()
                || !targets.iter().any(|t| match t {
                    TargetRef::Object(id) => {
                        super::filter::matches_target_filter(state, *id, constraint, &bare_ctx)
                    }
                    TargetRef::Player(pid) => super::filter::player_matches_target_filter(
                        constraint,
                        *pid,
                        source_controller_opt,
                    ),
                })
            {
                continue;
            }
        }
        let controlled_ctx =
            super::filter::FilterContext::from_source_with_controller(source_id, source_controller);
        if super::filter::matches_target_filter(state, entry.id, filter, &controlled_ctx) {
            let obj = match state.objects.get(&entry.id) {
                Some(o) => o,
                None => continue,
            };
            if can_target(obj, source_controller, source_id, state) {
                targets.push(TargetRef::Object(entry.id));
            }
        }
    }
}

/// Check if a filter contains a `HasSingleTarget` property anywhere in its tree.
fn filter_requires_single_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::HasSingleTarget)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_requires_single_target)
        }
        _ => false,
    }
}

fn filter_targets_stack_spells(filter: &TargetFilter) -> bool {
    use crate::types::ability::TypeFilter;
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) => {
            let in_stack = properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Stack));
            in_stack || type_filters.contains(&TypeFilter::Card)
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_targets_stack_spells)
        }
        TargetFilter::Not { filter } => filter_targets_stack_spells(filter),
        _ => false,
    }
}

fn add_players(state: &GameState, targets: &mut Vec<TargetRef>) {
    // Player-phasing exclusion: a phased-out player is treated as though they
    // don't exist for targeting purposes (mirrors CR 702.26b for permanents,
    // applied to players via card Oracle text like "you phase out").
    for player in &state.players {
        if player.is_phased_out() {
            continue;
        }
        // CR 702.16b + CR 702.16j: A player with protection from everything
        // can't be targeted by spells or abilities — any source, any quality.
        if super::static_abilities::player_has_protection_from_everything(state, player.id) {
            continue;
        }
        targets.push(TargetRef::Player(player.id));
    }
}

/// CR 702.16b: Protection prevents targeting from sources with the relevant quality.
fn is_protected_from(
    obj: &crate::game::game_object::GameObject,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let Some(source_obj) = state.objects.get(&source_id) else {
        return false;
    };

    for kw in &obj.keywords {
        if let Keyword::Protection(protection) = kw {
            if crate::game::keywords::source_matches_protection_target(protection, obj, source_obj)
            {
                return true;
            }
        }
    }
    false
}

/// CR 702.11d: Check if a source matches a HexproofFilter.
fn hexproof_filter_matches(
    filter: &HexproofFilter,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let source_obj = match state.objects.get(&source_id) {
        Some(o) => o,
        None => return false,
    };
    match filter {
        HexproofFilter::Color(color) => source_obj.color.contains(color),
        HexproofFilter::CardType(type_name) => {
            crate::game::keywords::source_matches_card_type(source_obj, type_name)
        }
        HexproofFilter::Quality(quality) => {
            crate::game::keywords::source_matches_quality(source_obj, quality)
        }
    }
}

/// Full battlefield targeting check: shroud + hexproof + protection (CR 702.16b).
fn can_target(
    obj: &crate::game::game_object::GameObject,
    source_controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // CR 702.18a: Shroud prevents targeting by any player.
    if obj.has_keyword(&Keyword::Shroud) {
        return false;
    }
    // CR 702.11a: Hexproof prevents targeting by opponents.
    if obj.has_keyword(&Keyword::Hexproof) && obj.controller != source_controller {
        return false;
    }
    // CR 702.11d: "Hexproof from [quality]" prevents targeting by opponents' sources
    // with the matching quality. CR 702.11e: IgnoreHexproof bypasses this.
    if obj.controller != source_controller {
        for kw in &obj.keywords {
            if let Keyword::HexproofFrom(ref filter) = kw {
                if hexproof_filter_matches(filter, source_id, state) {
                    return false;
                }
            }
        }
    }
    if is_protected_from(obj, source_id, state) {
        return false;
    }
    // CR 702.21a: Ward is a triggered ability, not a targeting restriction.
    // Targeting is legal; the ward trigger fires via process_triggers() and
    // counters the spell/ability unless the opponent pays the ward cost.
    // TODO(CR 115.7): Retargeting (Willbender-style) not implemented.
    true
}

/// CR 400.1: Returns all object IDs in the given zone.
///
/// Per-player zones (Hand, Library, Graveyard) are aggregated across all players.
/// Shared zones (Battlefield, Exile, Stack, Command) return the global list.
///
/// CR 702.26b: Phased-out battlefield permanents are treated as though they
/// don't exist — excluded from the `Zone::Battlefield` listing. Zones other
/// than battlefield can't contain phased-out permanents (phasing is a
/// battlefield-only status, CR 702.26d).
pub(crate) fn zone_object_ids(state: &GameState, zone: Zone) -> Vec<ObjectId> {
    match zone {
        Zone::Battlefield => state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.is_phased_in()))
            .collect(),
        Zone::Stack => state.stack.iter().map(|e| e.id).collect(),
        Zone::Exile => state.exile.iter().copied().collect(),
        Zone::Graveyard => state
            .players
            .iter()
            .flat_map(|p| p.graveyard.iter().copied())
            .collect(),
        Zone::Hand => state
            .players
            .iter()
            .flat_map(|p| p.hand.iter().copied())
            .collect(),
        Zone::Library => state
            .players
            .iter()
            .flat_map(|p| p.library.iter().copied())
            .collect(),
        Zone::Command => vec![],
    }
}

/// Extract all explicit `InZone` zones from a target filter, recursing through combinators.
fn extract_explicit_zones(filter: &TargetFilter) -> Vec<Zone> {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties
            .iter()
            .filter_map(|p| match p {
                FilterProp::InZone { zone } => Some(*zone),
                _ => None,
            })
            .collect(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().flat_map(extract_explicit_zones).collect()
        }
        TargetFilter::Not { filter } => extract_explicit_zones(filter),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::CastingVariant;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::ProtectionTarget;
    use crate::types::zones::Zone;

    fn setup_with_creatures() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);

        // Creature controlled by player 0
        let c0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c0).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Creature controlled by player 1
        let c1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        (state, c0, c1)
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature())
    }

    #[test]
    fn find_legal_targets_creature_returns_only_creatures() {
        let (state, c0, c1) = setup_with_creatures();
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn hexproof_creature_not_targetable_by_opponent() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn hexproof_creature_targetable_by_controller() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(1), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn shroud_creature_not_targetable_by_anyone() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Shroud);

        let targets_p0 = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        let targets_p1 = find_legal_targets(&state, &creature_filter(), PlayerId(1), ObjectId(99));
        assert!(!targets_p0.contains(&TargetRef::Object(c1)));
        assert!(!targets_p1.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn validate_targets_filters_out_removed_objects() {
        let (mut state, c0, c1) = setup_with_creatures();
        let original = vec![TargetRef::Object(c0), TargetRef::Object(c1)];

        state.battlefield.retain(|id| *id != c1);

        let legal = validate_targets(
            &state,
            &original,
            &creature_filter(),
            PlayerId(0),
            ObjectId(99),
        );
        assert!(legal.contains(&TargetRef::Object(c0)));
        assert!(!legal.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn check_fizzle_all_targets_illegal() {
        let original = vec![
            TargetRef::Object(ObjectId(1)),
            TargetRef::Object(ObjectId(2)),
        ];
        let legal: Vec<TargetRef> = vec![];
        assert!(check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_some_targets_legal() {
        let original = vec![
            TargetRef::Object(ObjectId(1)),
            TargetRef::Object(ObjectId(2)),
        ];
        let legal = vec![TargetRef::Object(ObjectId(1))];
        assert!(!check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_no_targets_never_fizzles() {
        let original: Vec<TargetRef> = vec![];
        let legal: Vec<TargetRef> = vec![];
        assert!(!check_fizzle(&original, &legal));
    }

    #[test]
    fn protection_from_red_prevents_red_source_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();

        // Give c1 protection from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        // Create a red source spell
        let red_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&red_source)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Red source cannot target creature with protection from red
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), red_source);
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn protection_from_red_allows_blue_source_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();

        // Give c1 protection from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        // Create a blue source spell
        let blue_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Unsummon".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&blue_source)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Blue source CAN target creature with protection from red
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), blue_source);
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn ward_does_not_prevent_targeting() {
        // Ward should be recognized but not block targeting (cost enforcement deferred)
        let (mut state, _c0, c1) = setup_with_creatures();

        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Ward(crate::types::keywords::WardCost::Mana(
                crate::types::mana::ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            )));

        // Ward creature can still be targeted (cost enforcement is separate)
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    // ---- find_legal_targets tests ----

    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter};

    fn setup_with_typed_creatures() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);

        // Creature controlled by player 0
        let c0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c0).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Creature controlled by player 1
        let c1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Land controlled by player 1
        let land = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
        }

        (state, c0, c1, land)
    }

    #[test]
    fn find_legal_targets_creature_filter() {
        let (state, c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(TypedFilter::creature());
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn find_legal_targets_permanent_opponent_nonland() {
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::Opponent)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        // Should find opponent's creature but not their land
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_permanent_opponent_nonland_via_type_filter() {
        // TypeFilter::Non is case-insensitive via type_filter_matches, so a single test suffices
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::Opponent)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_any_returns_creatures_and_players() {
        let (state, c0, c1, land) = setup_with_typed_creatures();
        let targets = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert!(targets.contains(&TargetRef::Object(land)));
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_player_returns_only_players() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let targets = find_legal_targets(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_opponent_as_player() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_respects_hexproof() {
        let (mut state, _c0, c1, _land) = setup_with_typed_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let filter = TargetFilter::Typed(TypedFilter::creature());
        // Player 0 can't target hexproof creature controlled by player 1
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn find_legal_targets_card_returns_stack_spells() {
        let (mut state, _c0, _c1, _land) = setup_with_typed_creatures();
        // Add a spell to the stack
        let spell_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(100),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let filter = TargetFilter::Typed(TypedFilter::card());
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
    }

    #[test]
    fn find_legal_targets_stack_restriction_excludes_battlefield() {
        use crate::types::ability::FilterProp;
        let (mut state, c0, _c1, _land) = setup_with_typed_creatures();

        // Make c0 an artifact permanent on the battlefield.
        state
            .objects
            .get_mut(&c0)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        // Add an artifact spell to the stack.
        let spell_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Artifact Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(200),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let spell_obj = state.objects.get_mut(&spell_id).unwrap();
        spell_obj
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        spell_obj.zone = crate::types::zones::Zone::Stack;

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
        assert!(!targets.contains(&TargetRef::Object(c0)));
    }

    #[test]
    fn aang_airbend_filter_targets_stack_spells_and_other_creatures() {
        use crate::types::ability::Effect;

        let (mut state, source_id, other_creature, _land) = setup_with_typed_creatures();
        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(1),
            "Mightform Harmonizer".to_string(),
            Zone::Stack,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
        }
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(300),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let effect = crate::parser::oracle_effect::parse_effect(
            "airbend up to one other target creature or spell",
        );
        let filter = match effect {
            Effect::ChangeZone { target, .. } => target,
            other => panic!("expected ChangeZone target, got {other:?}"),
        };

        let targets = find_legal_targets(&state, &filter, PlayerId(0), source_id);
        assert!(targets.contains(&TargetRef::Object(other_creature)));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
        assert!(!targets.contains(&TargetRef::Object(source_id)));
    }

    #[test]
    fn find_legal_targets_graveyard_finds_graveyard_cards() {
        let mut state = GameState::new_two_player(42);

        // Card in player 0's graveyard
        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Card on battlefield (should NOT be found)
        let bf_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Live Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bf_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(gy_card)));
        assert!(!targets.contains(&TargetRef::Object(bf_card)));
    }

    #[test]
    fn find_legal_targets_graveyard_excludes_battlefield() {
        let mut state = GameState::new_two_player(42);

        let bf_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bf_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.is_empty());
    }

    #[test]
    fn protection_blocks_graveyard_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);

        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Protected Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords
                .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));
        }

        // Red source trying to target graveyard card
        let red_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Red Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&red_source)
            .unwrap()
            .color
            .push(ManaColor::Red);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), red_source);
        assert!(!targets.contains(&TargetRef::Object(gy_card)));
    }

    #[test]
    fn hexproof_does_not_block_graveyard_targeting() {
        let mut state = GameState::new_two_player(42);

        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hexproof Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Hexproof);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        // Opponent (player 0) CAN target hexproof card in graveyard
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(gy_card)));
    }

    #[test]
    fn extract_player_from_damage_dealt_returns_damaged_player() {
        // CR 603.7c: "that player" for DamageDone triggers refers to the damaged player.
        let state = GameState::new_two_player(42);
        let event = crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(10),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        let result = extract_player_from_event(&event, &state);
        // Should return the damaged player (PlayerId(1)), not the source's controller.
        assert_eq!(result, Some(PlayerId(1)));
    }

    #[test]
    fn extract_player_from_damage_dealt_to_creature_returns_controller() {
        // When damage targets a creature, "that player" resolves to the creature's controller.
        let mut state = GameState::new_two_player(42);
        let creature_id = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let event = crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(10),
            target: TargetRef::Object(creature_id),
            amount: 2,
            is_combat: false,
            excess: 0,
        };
        let result = extract_player_from_event(&event, &state);
        assert_eq!(result, Some(PlayerId(1)));
    }

    #[test]
    fn extract_player_from_player_action_returns_acting_player() {
        let state = GameState::new_two_player(42);
        let event = crate::types::events::GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: crate::types::events::PlayerActionKind::Scry,
        };
        let result = extract_player_from_event(&event, &state);
        assert_eq!(result, Some(PlayerId(1)));
    }

    // --- CR 702.11d: HexproofFrom targeting tests ---

    #[test]
    fn hexproof_from_color_prevents_opponent_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        // Give c1 (player 1's creature) hexproof from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a red source spell on the stack controlled by player 0
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Player 0 (opponent) targeting c1 with a red source — should fail
        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_from_color_allows_non_matching_opponent_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a blue source
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Player 0 targeting c1 with a blue source — should succeed
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_from_color_allows_controller_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a red source controlled by the same player (player 1)
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Own Red Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Controller targeting own creature — should succeed regardless
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(1), source_id, &state));
    }

    #[test]
    fn hexproof_filter_matches_card_type() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::CardType(
                "artifacts".to_string(),
            )));

        // Create an artifact source
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Artifact Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_filter_matches_monocolored() {
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Quality(
                "monocolored".to_string(),
            )));

        // Monocolored source (exactly 1 color)
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Mono Red".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));

        // Multicolored source — NOT blocked by "hexproof from monocolored"
        let multi_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Multi Source".to_string(),
            Zone::Battlefield,
        );
        {
            let multi = state.objects.get_mut(&multi_id).unwrap();
            multi.color.push(ManaColor::Red);
            multi.color.push(ManaColor::Blue);
        }
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(0), multi_id, &state));
    }

    #[test]
    fn protection_from_instants_blocks_targeting() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "instants".to_string(),
            )));

        let source_id = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    /// CR 702.16b + CR 702.16j: A player with protection from everything
    /// cannot be a legal target of any spell or ability from any source.
    /// `find_legal_targets` must exclude that player from the "any target"
    /// scan.
    #[test]
    fn find_legal_targets_excludes_player_protection_from_everything() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source Spell".to_string(),
            Zone::Battlefield,
        );
        // Protect PlayerId(1) via a transient continuous effect.
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        // "any target" should list PlayerId(0) (unprotected) but not PlayerId(1).
        let targets = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), source);
        assert!(
            targets.contains(&TargetRef::Player(PlayerId(0))),
            "PlayerId(0) should be a legal target, got {:?}",
            targets
        );
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "PlayerId(1) has protection from everything — must NOT be targetable, got {:?}",
            targets
        );
    }

    /// CR 702.16b + CR 702.16j: "target opponent" (Typed filter with no
    /// type_filters and ControllerRef::Opponent) must also exclude a protected
    /// opponent — verifies the typed-player-target branch was updated.
    #[test]
    fn find_legal_targets_typed_opponent_excludes_protected_player() {
        use crate::types::ability::{ContinuousModification, ControllerRef, Duration, TypedFilter};
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source Spell".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        let filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), source);
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "protected opponent must not be a legal target, got {:?}",
            targets
        );
    }
}
