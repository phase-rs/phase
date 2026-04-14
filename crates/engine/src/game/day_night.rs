use crate::types::events::GameEvent;
use crate::types::game_state::{DayNight, GameState};
use crate::types::keywords::Keyword;

use super::transform;

/// Check and apply day/night transition at end of turn.
///
/// CR 730.2a/b:
/// - If it's currently Day and the active player cast no spells this turn, it becomes Night.
/// - If it's currently Night and the active player cast 2+ spells this turn, it becomes Day.
/// - On transition, all Daybound/Nightbound permanents transform accordingly.
pub fn check_day_night_transition(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let current = match state.day_night {
        Some(dn) => dn,
        None => return, // Day/night not yet initialized
    };

    let new_state = match current {
        DayNight::Day if state.spells_cast_this_turn == 0 => DayNight::Night,
        DayNight::Night if state.spells_cast_this_turn >= 2 => DayNight::Day,
        _ => return, // No transition
    };

    state.day_night = Some(new_state);

    events.push(GameEvent::DayNightChanged {
        new_state: match new_state {
            DayNight::Day => "Day".to_string(),
            DayNight::Night => "Night".to_string(),
        },
    });

    // Transform all Daybound/Nightbound permanents
    let to_transform: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| match new_state {
                // Becoming Night: transform Daybound creatures (front face -> night face)
                DayNight::Night => obj.has_keyword(&Keyword::Daybound) && !obj.transformed,
                // Becoming Day: transform Nightbound creatures (back face -> day face)
                DayNight::Day => obj.has_keyword(&Keyword::Nightbound) && obj.transformed,
            })
        })
        .collect();

    // CR 702.145c + CR 702.145f: Daybound/Nightbound permanents entering under
    // the opposite designation are handled at ETB time in zones::move_to_zone,
    // not here. This loop only handles on-transition transforms.
    for id in to_transform {
        let _ = transform::transform_permanent(state, id, events);
    }
}

/// CR 730.1: Set the game's day/night designation to a specific value.
/// Triggered by Effect::SetDayNight. Transforms all Daybound/Nightbound permanents
/// as appropriate for the new designation.
pub fn resolve_set_day_night(state: &mut GameState, to: DayNight, events: &mut Vec<GameEvent>) {
    let current = state.day_night;
    state.day_night = Some(to);

    events.push(GameEvent::DayNightChanged {
        new_state: match to {
            DayNight::Day => "Day".to_string(),
            DayNight::Night => "Night".to_string(),
        },
    });

    // Only transform permanents if the designation actually changed
    if current == Some(to) {
        return;
    }

    // Transform all Daybound/Nightbound permanents
    let to_transform: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| match to {
                DayNight::Night => obj.has_keyword(&Keyword::Daybound) && !obj.transformed,
                DayNight::Day => obj.has_keyword(&Keyword::Nightbound) && obj.transformed,
            })
        })
        .collect();

    for id in to_transform {
        let _ = transform::transform_permanent(state, id, events);
    }
}

/// Initialize day/night to Day when a daybound/nightbound card first enters the game.
///
/// CR 702.145d / CR 730.1: The game starts with no day/night designation. Once a permanent
/// with daybound or nightbound enters the battlefield, it becomes day (if not already set).
pub fn initialize_day_night(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if state.day_night.is_some() {
        return; // Already initialized
    }

    state.day_night = Some(DayNight::Day);
    events.push(GameEvent::DayNightChanged {
        new_state: "Day".to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_daybound_creature(state: &mut GameState) -> crate::types::identifiers::ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Daybound Werewolf".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Werewolf".to_string()],
        };
        obj.keywords = vec![Keyword::Daybound];
        obj.base_keywords = vec![Keyword::Daybound];
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.back_face = Some(BackFaceData {
            name: "Nightbound Werewolf".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            mana_cost: crate::types::mana::ManaCost::default(),
            keywords: vec![Keyword::Nightbound],
            abilities: vec![],
            trigger_definitions: vec![],
            replacement_definitions: vec![],
            static_definitions: vec![],
            color: vec![ManaColor::Green, ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
        });
        id
    }

    #[test]
    fn test_day_to_night_on_zero_spells() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Day);
        state.spells_cast_this_turn = 0;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::DayNightChanged { new_state } if new_state == "Night"
        )));
    }

    #[test]
    fn test_night_to_day_on_two_spells() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Night);
        state.spells_cast_this_turn = 2;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Day));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::DayNightChanged { new_state } if new_state == "Day"
        )));
    }

    #[test]
    fn test_no_transition_day_with_spells() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Day);
        state.spells_cast_this_turn = 1;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Day));
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_transition_night_with_one_spell() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Night);
        state.spells_cast_this_turn = 1;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_transition_when_none() {
        let mut state = GameState::new_two_player(42);
        assert_eq!(state.day_night, None);

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, None);
        assert!(events.is_empty());
    }

    #[test]
    fn test_daybound_transforms_to_night() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        state.day_night = Some(DayNight::Day);
        state.spells_cast_this_turn = 0;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Night));
        let obj = &state.objects[&id];
        assert!(obj.transformed);
        assert_eq!(obj.name, "Nightbound Werewolf");
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
    }

    #[test]
    fn test_nightbound_transforms_to_day() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        state.day_night = Some(DayNight::Day);
        state.spells_cast_this_turn = 0;

        // First transition to night (transforms daybound -> nightbound)
        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);
        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(state.objects[&id].transformed);

        // Now transition back to day
        state.spells_cast_this_turn = 2;
        let mut events2 = Vec::new();
        check_day_night_transition(&mut state, &mut events2);

        assert_eq!(state.day_night, Some(DayNight::Day));
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Daybound Werewolf");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
    }

    #[test]
    fn test_initialize_day_night() {
        let mut state = GameState::new_two_player(42);
        assert_eq!(state.day_night, None);

        let mut events = Vec::new();
        initialize_day_night(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Day));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::DayNightChanged { new_state } if new_state == "Day"
        )));
    }

    #[test]
    fn test_initialize_day_night_no_op_if_already_set() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Night);

        let mut events = Vec::new();
        initialize_day_night(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(events.is_empty());
    }
}
