use crate::types::ability::TargetRef;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

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
            add_creatures_color_filter(
                state,
                &mut targets,
                excluded,
                source_controller,
                source_id,
            );
        }
        _ => {
            // Unknown filter -- return empty (no legal targets)
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
        if !can_target(obj, source_controller, source_id) {
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
        if !can_target(obj, source_controller, source_id) {
            continue;
        }
        targets.push(TargetRef::Object(obj_id));
    }
}

fn add_stack_spells(state: &GameState, targets: &mut Vec<TargetRef>) {
    for entry in &state.stack {
        if matches!(entry.kind, crate::types::game_state::StackEntryKind::Spell { .. }) {
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
    _source_id: ObjectId,
) -> bool {
    // Shroud: can't be targeted by anyone
    if obj.has_keyword(&crate::types::keywords::Keyword::Shroud) {
        return false;
    }
    // Hexproof: can't be targeted by opponents
    if obj.has_keyword(&crate::types::keywords::Keyword::Hexproof) {
        if obj.controller != source_controller {
            return false;
        }
    }
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
    use crate::types::keywords::Keyword;

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
        let original = vec![TargetRef::Object(ObjectId(1)), TargetRef::Object(ObjectId(2))];
        let legal: Vec<TargetRef> = vec![];
        assert!(check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_some_targets_legal() {
        let original = vec![TargetRef::Object(ObjectId(1)), TargetRef::Object(ObjectId(2))];
        let legal = vec![TargetRef::Object(ObjectId(1))];
        assert!(!check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_no_targets_never_fizzles() {
        let original: Vec<TargetRef> = vec![];
        let legal: Vec<TargetRef> = vec![];
        assert!(!check_fizzle(&original, &legal));
    }
}
