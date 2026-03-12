use crate::types::ability::TargetRef;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

/// Parse ValidTgts$ filter string and return all legal targets.
pub fn find_legal_targets(
    state: &GameState,
    filter: &str,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    let mut targets = Vec::new();

    match filter {
        "Any" => {
            // All creatures on battlefield + all players
            add_creatures(state, &mut targets, None, source_controller, source_id);
            add_players(state, &mut targets);
        }
        "Creature" => {
            add_creatures(state, &mut targets, None, source_controller, source_id);
        }
        "Creature.YouCtrl" => {
            add_creatures(
                state,
                &mut targets,
                Some(ControlFilter::YouCtrl(source_controller)),
                source_controller,
                source_id,
            );
        }
        "Creature.OppCtrl" => {
            add_creatures(
                state,
                &mut targets,
                Some(ControlFilter::OppCtrl(source_controller)),
                source_controller,
                source_id,
            );
        }
        "Player" => {
            add_players(state, &mut targets);
        }
        "Card" => {
            // Target spells on the stack (for Counterspell etc.)
            add_stack_spells(state, &mut targets);
        }
        _ if filter.starts_with("Creature.non") => {
            let color_str = &filter["Creature.non".len()..];
            let excluded = parse_color(color_str);
            add_creatures_color_filter(state, &mut targets, excluded, source_controller, source_id);
        }
        _ => {
            // Unknown filter -- return empty (no legal targets)
        }
    }

    targets
}

/// Find legal targets using a typed TargetFilter.
///
/// Evaluates battlefield objects against the filter using the typed filter system,
/// and includes players/stack spells where appropriate.
pub fn find_legal_targets_typed(
    state: &GameState,
    filter: &crate::types::ability::TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    use crate::types::ability::{TargetFilter, TypeFilter};
    let mut targets = Vec::new();

    // Check if filter could match players
    if matches!(filter, TargetFilter::Any | TargetFilter::Player) {
        add_players(state, &mut targets);
    }

    // Check if filter could match stack spells (Card type or Any)
    let matches_stack = matches!(
        filter,
        TargetFilter::Any
            | TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                ..
            }
    );
    if matches_stack {
        add_stack_spells(state, &mut targets);
    }

    // Check battlefield objects using typed filter
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

    targets
}

/// Recheck targets on resolution, returns only still-legal targets.
pub fn validate_targets(
    state: &GameState,
    targets: &[TargetRef],
    filter: &str,
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

/// Returns true if ALL original targets are now illegal (spell fizzles per rule 608.2b).
pub fn check_fizzle(original_targets: &[TargetRef], legal_targets: &[TargetRef]) -> bool {
    if original_targets.is_empty() {
        return false; // Spells with no targets never fizzle
    }
    legal_targets.is_empty()
}

// --- Internal helpers ---

enum ControlFilter {
    YouCtrl(PlayerId),
    OppCtrl(PlayerId),
}

fn add_creatures(
    state: &GameState,
    targets: &mut Vec<TargetRef>,
    control: Option<ControlFilter>,
    source_controller: PlayerId,
    source_id: ObjectId,
) {
    for &obj_id in &state.battlefield {
        let obj = match state.objects.get(&obj_id) {
            Some(o) => o,
            None => continue,
        };
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        if let Some(ref cf) = control {
            match cf {
                ControlFilter::YouCtrl(controller) => {
                    if obj.controller != *controller {
                        continue;
                    }
                }
                ControlFilter::OppCtrl(controller) => {
                    if obj.controller == *controller {
                        continue;
                    }
                }
            }
        }
        if !can_target(obj, source_controller, source_id, state) {
            continue;
        }
        targets.push(TargetRef::Object(obj_id));
    }
}

fn add_creatures_color_filter(
    state: &GameState,
    targets: &mut Vec<TargetRef>,
    excluded_color: Option<ManaColor>,
    source_controller: PlayerId,
    source_id: ObjectId,
) {
    for &obj_id in &state.battlefield {
        let obj = match state.objects.get(&obj_id) {
            Some(o) => o,
            None => continue,
        };
        if !obj.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        if let Some(color) = excluded_color {
            if obj.color.contains(&color) {
                continue;
            }
        }
        if !can_target(obj, source_controller, source_id, state) {
            continue;
        }
        targets.push(TargetRef::Object(obj_id));
    }
}

fn add_stack_spells(state: &GameState, targets: &mut Vec<TargetRef>) {
    for entry in &state.stack {
        if matches!(
            entry.kind,
            crate::types::game_state::StackEntryKind::Spell { .. }
        ) {
            targets.push(TargetRef::Object(entry.id));
        }
    }
}

fn add_players(state: &GameState, targets: &mut Vec<TargetRef>) {
    for player in &state.players {
        targets.push(TargetRef::Player(player.id));
    }
}

fn can_target(
    obj: &crate::game::game_object::GameObject,
    source_controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // Shroud: can't be targeted by anyone
    if obj.has_keyword(&Keyword::Shroud) {
        return false;
    }
    // Hexproof: can't be targeted by opponents
    if obj.has_keyword(&Keyword::Hexproof) && obj.controller != source_controller {
        return false;
    }
    // Protection: can't be targeted by sources with the protected quality
    for kw in &obj.keywords {
        match kw {
            Keyword::Protection(ProtectionTarget::Color(color)) => {
                if let Some(source_obj) = state.objects.get(&source_id) {
                    if source_obj.color.contains(color) {
                        return false;
                    }
                }
            }
            Keyword::Protection(ProtectionTarget::Multicolored) => {
                if let Some(source_obj) = state.objects.get(&source_id) {
                    if source_obj.color.len() > 1 {
                        return false;
                    }
                }
            }
            _ => {}
        }
    }
    // Ward: targeting is legal, cost enforcement deferred to mana payment UI
    true
}

fn parse_color(s: &str) -> Option<ManaColor> {
    match s {
        "White" => Some(ManaColor::White),
        "Blue" => Some(ManaColor::Blue),
        "Black" => Some(ManaColor::Black),
        "Red" => Some(ManaColor::Red),
        "Green" => Some(ManaColor::Green),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
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

    #[test]
    fn find_legal_targets_any_returns_creatures_and_players() {
        let (state, c0, c1) = setup_with_creatures();
        let targets = find_legal_targets(&state, "Any", PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
        assert_eq!(targets.len(), 4);
    }

    #[test]
    fn find_legal_targets_creature_returns_only_creatures() {
        let (state, c0, c1) = setup_with_creatures();
        let targets = find_legal_targets(&state, "Creature", PlayerId(0), ObjectId(99));
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

        // Player 0 tries to target player 1's hexproof creature
        let targets = find_legal_targets(&state, "Creature", PlayerId(0), ObjectId(99));
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

        // Player 1 (controller) can target their own hexproof creature
        let targets = find_legal_targets(&state, "Creature", PlayerId(1), ObjectId(99));
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

        // Neither player can target a shroud creature
        let targets_p0 = find_legal_targets(&state, "Creature", PlayerId(0), ObjectId(99));
        let targets_p1 = find_legal_targets(&state, "Creature", PlayerId(1), ObjectId(99));
        assert!(!targets_p0.contains(&TargetRef::Object(c1)));
        assert!(!targets_p1.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn validate_targets_filters_out_removed_objects() {
        let (mut state, c0, c1) = setup_with_creatures();
        let original = vec![TargetRef::Object(c0), TargetRef::Object(c1)];

        // Remove c1 from battlefield
        state.battlefield.retain(|id| *id != c1);

        let legal = validate_targets(&state, &original, "Creature", PlayerId(0), ObjectId(99));
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
        let targets = find_legal_targets(&state, "Creature", PlayerId(0), red_source);
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
        let targets = find_legal_targets(&state, "Creature", PlayerId(0), blue_source);
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
            .push(Keyword::Ward(crate::types::mana::ManaCost::Cost {
                generic: 2,
                shards: vec![],
            }));

        // Ward creature can still be targeted (cost enforcement is separate)
        let targets = find_legal_targets(&state, "Creature", PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    // ---- find_legal_targets_typed tests ----

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
    fn find_legal_targets_typed_creature_filter() {
        let (state, c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed {
            card_type: Some(TypeFilter::Creature),
            subtype: None,
            controller: None,
            properties: vec![],
        };
        let targets = find_legal_targets_typed(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn find_legal_targets_typed_permanent_opponent_nonland() {
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed {
            card_type: Some(TypeFilter::Permanent),
            subtype: None,
            controller: Some(ControllerRef::Opponent),
            properties: vec![FilterProp::NonType {
                value: "Land".to_string(),
            }],
        };
        let targets = find_legal_targets_typed(&state, &filter, PlayerId(0), ObjectId(99));
        // Should find opponent's creature but not their land
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_typed_any_returns_creatures_and_players() {
        let (state, c0, c1, land) = setup_with_typed_creatures();
        let targets =
            find_legal_targets_typed(&state, &TargetFilter::Any, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert!(targets.contains(&TargetRef::Object(land)));
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_typed_player_returns_only_players() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let targets =
            find_legal_targets_typed(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_typed_respects_hexproof() {
        let (mut state, _c0, c1, _land) = setup_with_typed_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let filter = TargetFilter::Typed {
            card_type: Some(TypeFilter::Creature),
            subtype: None,
            controller: None,
            properties: vec![],
        };
        // Player 0 can't target hexproof creature controlled by player 1
        let targets = find_legal_targets_typed(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn find_legal_targets_typed_card_returns_stack_spells() {
        let (mut state, _c0, _c1, _land) = setup_with_typed_creatures();
        // Add a spell to the stack
        use crate::types::ability::{Effect, ResolvedAbility};
        let spell_id = ObjectId(100);
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
            },
        });
        let filter = TargetFilter::Typed {
            card_type: Some(TypeFilter::Card),
            subtype: None,
            controller: None,
            properties: vec![],
        };
        let targets = find_legal_targets_typed(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
    }
}
