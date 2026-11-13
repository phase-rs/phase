use crate::game::game_object::GameObject;
use crate::types::events::GameEvent;
use crate::types::game_state::{DayNight, GameState};
use crate::types::keywords::Keyword;
use crate::types::proposed_event::CopiedFaceUp;

use super::transform;

/// CR 702.145b + CR 702.145c + CR 702.145e + CR 702.145f: whether a permanent turns over for
/// `designation`: daybound and front face up at night, nightbound and back face up during the
/// day. "Front face up" is the face that's up (`transform::face_up`), not whether it last
/// transformed.
pub(crate) fn face_disagrees_with(obj: &GameObject, designation: DayNight) -> bool {
    let front_face_up = match transform::face_up(obj) {
        CopiedFaceUp::Front => true,
        CopiedFaceUp::TransformedBack | CopiedFaceUp::ModalBack => false,
    };
    match designation {
        DayNight::Night => obj.has_keyword(&Keyword::Daybound) && front_face_up,
        DayNight::Day => obj.has_keyword(&Keyword::Nightbound) && !front_face_up,
    }
}

/// CR 702.145b + CR 702.145c + CR 702.145e + CR 702.145f: transforms every permanent whose face
/// disagrees with `designation` (`face_disagrees_with`). CR 701.27c + CR 712.9: a permanent that
/// isn't a double-faced token or card is left as it is (`transform_permanent` refuses it).
/// CR 702.26b + CR 702.26d: phased-out permanents neither exist nor are controlled.
/// CR 702.145b + CR 702.145e: its own designation ability causes the transform.
fn transform_disagreeing_permanents(
    state: &mut GameState,
    designation: DayNight,
    events: &mut Vec<GameEvent>,
) {
    let to_transform: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_phased_in_permanent() && face_disagrees_with(obj, designation)
            })
        })
        .collect();
    for id in to_transform {
        let _ = transform::transform_permanent_with_cause(
            state,
            id,
            transform::TransformCause::DayNightDesignation,
            events,
        );
    }
}

/// CR 702.145c + CR 702.145f: "Any time a player controls a permanent that is front face up with
/// daybound and it's night, that player transforms that permanent. This happens immediately and
/// isn't a state-based action." (and the nightbound/day mirror). CR 111.1 + CR 702.145b: a token
/// isn't represented by a card, so a token copy of a daybound card made at night enters front
/// face up (CR 707.8a). CR 603.10 + CR 603.6a: its entry's triggers are checked against the
/// objects as they exist immediately after that event, so its own enters triggers see the face it
/// entered with; the transform is the next event. The engine collects an action's triggers from
/// the live object in the post-action pipeline, so this pass runs at the first checkpoint after
/// that collection, not inside the resolution; no printed "create a token that's a copy of … card"
/// producer observes the token between the two. Its Transformed events are matched before the
/// next SBA check (CR 603.10; `engine_priority`).
/// The untap step also calls it right after phasing (`turns::execute_untap_with_choices`),
/// so a permanent that phases in agrees before CR 502.3's untap decision.
pub(crate) fn keep_faces_in_agreement(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let Some(designation) = state.day_night else {
        return;
    };
    // CR 613.1: a copy effect's daybound/nightbound is a layer-applied keyword.
    if state.layers_dirty.is_dirty() {
        super::layers::flush_layers(state);
    }
    transform_disagreeing_permanents(state, designation, events);
}

/// Check and apply day/night transition at end of turn.
///
/// CR 502.2 (see also CR 731, "Day and Night"):
/// - If it's currently Day and the active player cast no spells this turn, it becomes Night.
/// - If it's currently Night and the active player cast 2+ spells this turn, it becomes Day.
/// - On transition, all Daybound/Nightbound permanents transform accordingly.
pub fn check_day_night_transition(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let current = match state.day_night {
        Some(dn) => dn,
        None => return, // Day/night not yet initialized
    };

    // CR 502.2: the transition keys on the spells the (previous turn's) ACTIVE
    // player cast that turn — not the table-wide total. `spells_cast_this_turn`
    // counts every player's casts, so an opponent's instant-speed spell on the
    // active player's turn would wrongly drive or suppress the transition (e.g.
    // keep it Day when the active player cast nothing). Read the active player's
    // own per-turn cast count. This runs at cleanup, where `active_player` is
    // still the player whose turn is ending and the per-player tally is intact.
    let active_spells = state
        .spells_cast_this_turn_by_player
        .get(&state.active_player)
        .map_or(0, |records| records.len());

    let new_state = match current {
        DayNight::Day if active_spells == 0 => DayNight::Night,
        DayNight::Night if active_spells >= 2 => DayNight::Day,
        _ => return, // No transition
    };

    state.day_night = Some(new_state);

    events.push(GameEvent::DayNightChanged {
        new_state: match new_state {
            DayNight::Day => "Day".to_string(),
            DayNight::Night => "Night".to_string(),
        },
    });

    // CR 702.145b + CR 702.145e: as it becomes day or night, each disagreeing permanent
    // transforms. A card entering at night enters transformed (zones::move_to_zone,
    // CR 702.145b); any other disagreement, a token's included, is corrected by
    // keep_faces_in_agreement.
    transform_disagreeing_permanents(state, new_state, events);
}

/// CR 731.1: Set the game's day/night designation to a specific value.
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
    transform_disagreeing_permanents(state, to, events);
}

/// Initialize day/night to Day when a daybound/nightbound card first enters the game.
///
/// CR 702.145d / CR 731.1: The game starts with no day/night designation. Once a permanent
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
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
            name: "Nightbound Werewolf".to_string(),
            power: Some(4),
            toughness: Some(4),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            mana_cost: crate::types::mana::ManaCost::default(),
            keywords: vec![Keyword::Nightbound],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::Green, ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        });
        id
    }

    /// Record `n` spells cast this turn by the active player in the per-player
    /// tally that CR 502.2 keys the day/night transition on.
    fn set_active_spells(state: &mut GameState, n: usize) {
        let player = state.active_player;
        state.spells_cast_this_turn_by_player.insert(
            player,
            (0..n)
                .map(|_| crate::types::game_state::SpellCastRecord::default())
                .collect(),
        );
    }

    #[test]
    fn test_day_to_night_on_zero_spells() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Day);
        set_active_spells(&mut state, 0);

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
        set_active_spells(&mut state, 2);

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
        set_active_spells(&mut state, 1);

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Day));
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_transition_night_with_one_spell() {
        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Night);
        set_active_spells(&mut state, 1);

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(events.is_empty());
    }

    /// CR 502.2: only the (previous turn's) ACTIVE player's spells drive the
    /// transition. An opponent casting spells on the active player's turn must not
    /// affect it. Here the active player casts 0 but the opponent casts 2; the
    /// table-wide total is 2, yet Day must still become Night (active player cast
    /// nothing). Using the global `spells_cast_this_turn` would wrongly stay Day.
    #[test]
    fn day_night_ignores_non_active_player_spells() {
        use crate::types::game_state::SpellCastRecord;

        let mut state = GameState::new_two_player(42);
        state.day_night = Some(DayNight::Day);
        let active = state.active_player;
        let opponent = if active == PlayerId(0) {
            PlayerId(1)
        } else {
            PlayerId(0)
        };

        // Active player cast nothing; opponent cast 2 (e.g. instants on this turn).
        set_active_spells(&mut state, 0);
        state.spells_cast_this_turn_by_player.insert(
            opponent,
            (0..2).map(|_| SpellCastRecord::default()).collect(),
        );
        // The global tally reflects the table total but must be irrelevant.
        state.spells_cast_this_turn = 2;

        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);

        assert_eq!(
            state.day_night,
            Some(DayNight::Night),
            "CR 502.2: active player cast 0 spells, so Day becomes Night regardless of the opponent's casts"
        );
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
        set_active_spells(&mut state, 0);

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
        set_active_spells(&mut state, 0);

        // First transition to night (transforms daybound -> nightbound)
        let mut events = Vec::new();
        check_day_night_transition(&mut state, &mut events);
        assert_eq!(state.day_night, Some(DayNight::Night));
        assert!(state.objects[&id].transformed);

        // Now transition back to day
        set_active_spells(&mut state, 2);
        let mut events2 = Vec::new();
        check_day_night_transition(&mut state, &mut events2);

        assert_eq!(state.day_night, Some(DayNight::Day));
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Daybound Werewolf");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
    }

    fn transformed_events(events: &[GameEvent], id: crate::types::identifiers::ObjectId) -> usize {
        events
            .iter()
            .filter(
                |event| matches!(event, GameEvent::Transformed { object_id } if *object_id == id),
            )
            .count()
    }

    /// CR 702.145c: a daybound token front face up at night transforms at the next agreement
    /// pass, and an agreeing permanent is left alone by the next one. DN1 isolates the shape D1
    /// runs on a printed card: Feldon of the Third Path's copy of Baneblade Scoundrel at night.
    #[test]
    fn a_daybound_token_front_face_up_at_night_transforms() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        state.objects.get_mut(&id).unwrap().is_token = true;
        state.day_night = Some(DayNight::Night);

        let mut events = Vec::new();
        keep_faces_in_agreement(&mut state, &mut events);

        assert_eq!(transformed_events(&events, id), 1);
        let obj = &state.objects[&id];
        assert!(obj.transformed);
        assert_eq!(obj.name, "Nightbound Werewolf");

        let mut again = Vec::new();
        keep_faces_in_agreement(&mut state, &mut again);
        assert!(
            again.is_empty(),
            "an agreeing face is not transformed again"
        );
    }

    /// CR 702.145f: a nightbound permanent back face up during the day transforms. DN2's printed
    /// shape is Baneblade Scoundrel turned to Baneclaw Marauder during the day by Moonmist
    /// ("Transform all Humans. Prevent all combat damage that would be dealt this turn by
    /// creatures other than Werewolves and Wolves."). Moonmist can't transform it (CR 702.145b),
    /// so the setup turns it through its own ability; the synthetic object isolates CR 702.145f.
    #[test]
    fn a_nightbound_permanent_back_face_up_during_the_day_transforms() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        let mut setup_events = Vec::new();
        transform::transform_permanent_with_cause(
            &mut state,
            id,
            transform::TransformCause::DayNightDesignation,
            &mut setup_events,
        )
        .unwrap();
        assert!(state.objects[&id].transformed, "precondition: back face up");
        assert!(state.objects[&id].has_keyword(&Keyword::Nightbound));
        state.day_night = Some(DayNight::Day);

        let mut events = Vec::new();
        keep_faces_in_agreement(&mut state, &mut events);

        assert_eq!(transformed_events(&events, id), 1);
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Daybound Werewolf");
    }

    /// CR 701.27c + CR 712.9: a permanent with daybound that isn't double-faced can't transform,
    /// so nothing happens. DN3's printed shape is Clone ("You may have this creature enter as a
    /// copy of any creature on the battlefield.") copying Baneblade Scoundrel during the day,
    /// followed by Into the Night: that permanent has daybound and is front face up, but it isn't
    /// a double-faced card.
    #[test]
    fn a_permanent_that_isnt_double_faced_is_left_alone() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        state.objects.get_mut(&id).unwrap().back_face = None;
        state.day_night = Some(DayNight::Night);
        assert!(
            face_disagrees_with(&state.objects[&id], DayNight::Night),
            "reach: the pass considers it"
        );

        let mut events = Vec::new();
        keep_faces_in_agreement(&mut state, &mut events);

        assert!(events.is_empty());
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.name, "Daybound Werewolf");
    }

    /// CR 701.27c: a token copy of a Room keeps the Room's other half in `back_face` as a
    /// copiable value with no layout (CR 709.5b). It isn't represented by a double-faced token or
    /// card, so the designation pass leaves it alone even with daybound and front face up at
    /// night. R3's printed shape: Cackling Counterpart's copy of Mirror Room // Fractured Realm,
    /// made a copy of Baneblade Scoundrel by Mirrorweave, at night. DN3 is the sibling with no
    /// stored slot; DN1 is the positive.
    #[test]
    fn a_room_token_with_its_other_half_stored_is_left_alone() {
        use crate::types::mana::{ManaCost, ManaCostShard};

        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.is_token = true;
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Room".to_string()],
            };
            obj.back_face = Some(BackFaceData {
                name: "Fractured Realm".to_string(),
                mana_cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                    generic: 5,
                },
                ..Default::default()
            });
        }
        state.day_night = Some(DayNight::Night);
        // The pass flushes layers first; flush them here so the reach guard reads the object the
        // pass reads (a Room with both doors locked has no name, CR 709.5).
        super::super::layers::flush_layers(&mut state);
        assert!(
            face_disagrees_with(&state.objects[&id], DayNight::Night),
            "reach: the pass considers it"
        );
        let name_before = state.objects[&id].name.clone();

        let mut events = Vec::new();
        keep_faces_in_agreement(&mut state, &mut events);

        assert!(events.is_empty());
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.transformation_count, 0);
        assert_eq!(
            obj.back_face.as_ref().map(|face| face.name.as_str()),
            Some("Fractured Realm")
        );
        assert_eq!(obj.name, name_before);
    }

    /// CR 731.1: with neither day nor night, no face disagrees, so nothing transforms. The row
    /// asserts only that: CR 702.145d ("Any time a player controls a permanent with daybound, if
    /// it's neither day nor night, it becomes day.") is not applied by this pass, which is
    /// pre-existing (`initialize_day_night` has no production caller).
    #[test]
    fn no_designation_transforms_nothing() {
        let mut state = GameState::new_two_player(42);
        let id = setup_daybound_creature(&mut state);
        state.objects.get_mut(&id).unwrap().is_token = true;
        assert_eq!(state.day_night, None);

        let mut events = Vec::new();
        keep_faces_in_agreement(&mut state, &mut events);

        assert_eq!(transformed_events(&events, id), 0);
        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert_eq!(obj.transformation_count, 0);
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
