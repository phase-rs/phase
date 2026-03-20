use crate::types::ability::{FilterProp, TargetFilter, TargetRef, TypedFilter};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Find legal targets using a typed TargetFilter (CR 115.3).
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

    // Typed filter with no card_type targets players, not permanents.
    // e.g. "target opponent" → Typed { card_type: None, controller: Opponent }
    if let TargetFilter::Typed(TypedFilter {
        card_type: None,
        controller,
        ..
    }) = filter
    {
        for player in &state.players {
            let is_opponent = player.id != source_controller;
            let include = match controller {
                Some(ControllerRef::Opponent) => is_opponent,
                Some(ControllerRef::You) => !is_opponent,
                None => true,
            };
            if include {
                targets.push(TargetRef::Player(player.id));
            }
        }
        return targets;
    }

    let explicit_zones = extract_explicit_zones(filter);

    if !explicit_zones.is_empty() {
        // Explicit zone search: ONLY search the specified zones
        for zone in &explicit_zones {
            for obj_id in zone_object_ids(state, *zone) {
                if super::filter::matches_target_filter_controlled(
                    state,
                    obj_id,
                    filter,
                    source_id,
                    source_controller,
                ) {
                    let obj = match state.objects.get(&obj_id) {
                        Some(o) => o,
                        None => continue,
                    };
                    if *zone == Zone::Battlefield {
                        // Full targeting rules on battlefield (hexproof, shroud, protection)
                        if can_target(obj, source_controller, source_id, state) {
                            targets.push(TargetRef::Object(obj_id));
                        }
                    } else {
                        // Non-battlefield: only protection applies (702.16a)
                        if !is_protected_from(obj, source_id, state) {
                            targets.push(TargetRef::Object(obj_id));
                        }
                    }
                }
            }
        }
    } else {
        // No explicit zone: default behavior (battlefield + stack for Card type)
        if filter_targets_stack_spells(filter) {
            add_stack_spells(state, filter, source_controller, source_id, &mut targets);
        }

        for &obj_id in &state.battlefield {
            if super::filter::matches_target_filter_controlled(
                state,
                obj_id,
                filter,
                source_id,
                source_controller,
            ) {
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
    targets
        .iter()
        .filter(|t| legal.contains(t))
        .cloned()
        .collect()
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
        GameEvent::PermanentTapped { object_id } => Some(*object_id),
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
        GameEvent::CrimeCommitted { player_id, .. } => Some(*player_id),
        GameEvent::PlayerEliminated { player_id, .. } => Some(*player_id),
        // For object-centric events, extract the controller
        GameEvent::BecomesTarget { source_id, .. } => {
            state.objects.get(source_id).map(|obj| obj.controller)
        }
        GameEvent::DamageDealt { source_id, .. } => {
            state.objects.get(source_id).map(|obj| obj.controller)
        }
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
            StackEntryKind::ActivatedAbility { .. } | StackEntryKind::TriggeredAbility { .. } => {
                targets.push(TargetRef::Object(entry.id));
            }
            StackEntryKind::Spell { .. } => {}
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
    for entry in &state.stack {
        if !matches!(
            entry.kind,
            crate::types::game_state::StackEntryKind::Spell { .. }
        ) {
            continue;
        }
        if super::filter::matches_target_filter_controlled(
            state,
            entry.id,
            filter,
            source_id,
            source_controller,
        ) {
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

fn filter_targets_stack_spells(filter: &TargetFilter) -> bool {
    use crate::types::ability::TypeFilter;
    match filter {
        TargetFilter::Typed(TypedFilter {
            card_type,
            properties,
            ..
        }) => {
            let in_stack = properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Stack));
            in_stack || matches!(card_type, Some(TypeFilter::Card))
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_targets_stack_spells)
        }
        TargetFilter::Not { filter } => filter_targets_stack_spells(filter),
        _ => false,
    }
}

fn add_players(state: &GameState, targets: &mut Vec<TargetRef>) {
    for player in &state.players {
        targets.push(TargetRef::Player(player.id));
    }
}

/// Check if an object has protection from the given source (CR 702.16a: works in any zone).
fn is_protected_from(
    obj: &crate::game::game_object::GameObject,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    for kw in &obj.keywords {
        match kw {
            Keyword::Protection(ProtectionTarget::Color(color)) => {
                if let Some(source_obj) = state.objects.get(&source_id) {
                    if source_obj.color.contains(color) {
                        return true;
                    }
                }
            }
            Keyword::Protection(ProtectionTarget::Multicolored) => {
                if let Some(source_obj) = state.objects.get(&source_id) {
                    if source_obj.color.len() > 1 {
                        return true;
                    }
                }
            }
            // CR 702.16: ChosenColor resolves from the protected permanent's chosen_attributes
            Keyword::Protection(ProtectionTarget::ChosenColor) => {
                if let Some(color) = obj.chosen_color() {
                    if let Some(source_obj) = state.objects.get(&source_id) {
                        if source_obj.color.contains(&color) {
                            return true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

/// Full battlefield targeting check: shroud + hexproof + protection (CR 702.16a).
fn can_target(
    obj: &crate::game::game_object::GameObject,
    source_controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // CR 702.18a: Shroud — can't be the target of spells or abilities.
    if obj.has_keyword(&Keyword::Shroud) {
        return false;
    }
    // CR 702.11b: Hexproof — can't be targeted by opponents.
    if obj.has_keyword(&Keyword::Hexproof) && obj.controller != source_controller {
        return false;
    }
    if is_protected_from(obj, source_id, state) {
        return false;
    }
    // Ward: targeting is legal, cost enforcement deferred to mana payment UI
    true
}

/// Returns all object IDs in the given zone.
fn zone_object_ids(state: &GameState, zone: Zone) -> Vec<ObjectId> {
    match zone {
        Zone::Battlefield => state.battlefield.clone(),
        Zone::Stack => state.stack.iter().map(|e| e.id).collect(),
        Zone::Exile => state.exile.clone(),
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
                .properties(vec![FilterProp::NonType {
                    value: "Land".to_string(),
                }]),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        // Should find opponent's creature but not their land
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_permanent_opponent_nonland_lowercase() {
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::Opponent)
                .properties(vec![FilterProp::NonType {
                    value: "land".to_string(),
                }]),
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
        use crate::types::ability::{Effect, ResolvedAbility};
        let spell_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(100),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "test".to_string(),
                        description: None,
                    },
                    vec![],
                    spell_id,
                    PlayerId(0),
                ),
                casting_variant: CastingVariant::Normal,
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
        use crate::types::ability::{Effect, ResolvedAbility};
        let spell_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Artifact Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(200),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "test".to_string(),
                        description: None,
                    },
                    vec![],
                    spell_id,
                    PlayerId(1),
                ),
                casting_variant: CastingVariant::Normal,
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
}
