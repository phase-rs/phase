use std::str::FromStr;

use crate::game::combat::AttackerInfo;
use crate::game::game_object::GameObject;
use crate::game::zones;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Check if a game object has a specific keyword, using discriminant-based matching
/// for simple keywords (ignoring associated data for parameterized variants).
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    obj.keywords
        .iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}

/// Convenience: check for Flying.
pub fn has_flying(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flying)
}

/// Convenience: check for Haste.
pub fn has_haste(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Haste)
}

/// Convenience: check for Flash.
pub fn has_flash(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flash)
}

/// Convenience: check for Hexproof.
pub fn has_hexproof(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Hexproof)
}

/// Convenience: check for Shroud.
pub fn has_shroud(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Shroud)
}

/// Convenience: check for Indestructible.
pub fn has_indestructible(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Indestructible)
}

/// Check whether `target` has protection from `source`.
/// Returns true if any protection keyword on the target matches the source.
pub fn protection_prevents_from(target: &GameObject, source: &GameObject) -> bool {
    for kw in &target.keywords {
        if let Keyword::Protection(ref pt) = kw {
            match pt {
                ProtectionTarget::Color(color) => {
                    if source.color.contains(color) {
                        return true;
                    }
                }
                ProtectionTarget::Multicolored => {
                    if source.color.len() > 1 {
                        return true;
                    }
                }
                // CR 702.16: ChosenColor resolves from the target permanent's chosen_attributes
                ProtectionTarget::ChosenColor => {
                    if let Some(color) = target.chosen_color() {
                        if source.color.contains(&color) {
                            return true;
                        }
                    }
                }
                ProtectionTarget::CardType(_) | ProtectionTarget::Quality(_) => {
                    // Not yet implemented for damage prevention
                }
            }
        }
    }
    false
}

/// Batch parse keyword strings into typed Keyword values.
/// Used when creating GameObjects from parsed card data.
pub fn parse_keywords(keyword_strings: &[String]) -> Vec<Keyword> {
    keyword_strings
        .iter()
        .map(|s| Keyword::from_str(s).unwrap())
        .collect()
}

/// CR 702.49a-c: Resolve Ninjutsu activation.
///
/// Validates the activation, returns the specified attacker to its owner's hand,
/// and puts the Ninjutsu creature onto the battlefield tapped and attacking the
/// same player/planeswalker as the returned creature.
///
/// CR 702.49c: The Ninjutsu creature is never "declared as an attacker" so it
/// does not fire "whenever ~ attacks" triggers.
pub fn activate_ninjutsu(
    state: &mut GameState,
    player: PlayerId,
    ninjutsu_card_id: CardId,
    attacker_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    // Validate: must be in combat (declare blockers or later)
    let combat = state.combat.as_ref().ok_or("No active combat")?;

    // Validate: attacker must be in combat and unblocked
    let attacker_info = combat
        .attackers
        .iter()
        .find(|a| a.object_id == attacker_to_return)
        .ok_or("Specified creature is not an attacker")?
        .clone();

    let is_blocked = combat
        .blocker_assignments
        .get(&attacker_to_return)
        .is_some_and(|blockers| !blockers.is_empty());
    if is_blocked {
        return Err("Attacker is blocked".to_string());
    }

    // Validate: attacker controlled by player
    let attacker_obj = state
        .objects
        .get(&attacker_to_return)
        .ok_or("Attacker not found")?;
    if attacker_obj.controller != player {
        return Err("You don't control that attacker".to_string());
    }

    // Find the ninjutsu card in player's hand
    let p = &state.players[player.0 as usize];
    let ninjutsu_obj_id = p
        .hand
        .iter()
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .is_some_and(|o| o.card_id == ninjutsu_card_id)
        })
        .copied()
        .ok_or("Ninjutsu card not in hand")?;

    // Validate: card has Ninjutsu keyword
    let ninjutsu_obj = state
        .objects
        .get(&ninjutsu_obj_id)
        .ok_or("Ninjutsu card object not found")?;
    if !ninjutsu_obj.has_keyword(&Keyword::Ninjutsu(Default::default())) {
        return Err("Card does not have Ninjutsu".to_string());
    }

    // Get the defending player from the attacker's combat info
    let defending_player = attacker_info.defending_player;

    // 1. Return attacker to owner's hand
    zones::move_to_zone(state, attacker_to_return, Zone::Hand, events);

    // Remove the returned attacker from combat state
    if let Some(combat) = state.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != attacker_to_return);
        combat.blocker_assignments.remove(&attacker_to_return);
    }

    // 2. Move Ninjutsu card from hand to battlefield
    zones::move_to_zone(state, ninjutsu_obj_id, Zone::Battlefield, events);

    // 3. Set tapped, entered_battlefield_turn (summoning sickness)
    if let Some(obj) = state.objects.get_mut(&ninjutsu_obj_id) {
        obj.tapped = true;
        obj.entered_battlefield_turn = Some(state.turn_number);
    }

    // 4. CR 702.49c: Add to combat.attackers directly — do NOT use declare_attackers()
    //    This ensures no AttackersDeclared event fires, so no "whenever ~ attacks" triggers.
    if let Some(combat) = state.combat.as_mut() {
        combat.attackers.push(AttackerInfo {
            object_id: ninjutsu_obj_id,
            defending_player,
        });
    }

    state.layers_dirty = true;

    Ok(())
}

/// Returns the CardIds of cards in the player's hand that have the Ninjutsu keyword.
pub fn ninjutsu_cards_in_hand(state: &GameState, player: PlayerId) -> Vec<CardId> {
    let p = &state.players[player.0 as usize];
    p.hand
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.has_keyword(&Keyword::Ninjutsu(Default::default())) {
                Some(obj.card_id)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn has_keyword_simple_match() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        assert!(has_keyword(&obj, &Keyword::Flying));
        assert!(!has_keyword(&obj, &Keyword::Haste));
    }

    #[test]
    fn has_keyword_discriminant_matching() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Kicker(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        }));
        // Discriminant match -- doesn't care about the param value
        assert!(has_keyword(
            &obj,
            &Keyword::Kicker(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            })
        ));
        assert!(!has_keyword(
            &obj,
            &Keyword::Cycling(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })
        ));
    }

    #[test]
    fn convenience_functions() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        obj.keywords.push(Keyword::Flash);
        obj.keywords.push(Keyword::Hexproof);
        obj.keywords.push(Keyword::Shroud);
        obj.keywords.push(Keyword::Indestructible);

        assert!(has_flying(&obj));
        assert!(has_haste(&obj));
        assert!(has_flash(&obj));
        assert!(has_hexproof(&obj));
        assert!(has_shroud(&obj));
        assert!(has_indestructible(&obj));
    }

    #[test]
    fn parse_keywords_known() {
        let strings = vec![
            "Flying".to_string(),
            "Haste".to_string(),
            "Deathtouch".to_string(),
        ];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed,
            vec![Keyword::Flying, Keyword::Haste, Keyword::Deathtouch]
        );
    }

    #[test]
    fn parse_keywords_parameterized() {
        let strings = vec!["Kicker:1G".to_string(), "Ward:2".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed[0],
            Keyword::Kicker(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
        );
        assert_eq!(
            parsed[1],
            Keyword::Ward(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })
        );
    }

    #[test]
    fn parse_keywords_unknown() {
        let strings = vec!["NotReal".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed[0], Keyword::Unknown("NotReal".to_string()));
    }

    #[test]
    fn has_keyword_method_on_game_object() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Indestructible);
        assert!(obj.has_keyword(&Keyword::Indestructible));
        assert!(!obj.has_keyword(&Keyword::Flying));
    }

    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;

    fn setup_ninjutsu_scenario() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareBlockers;

        // Create an attacker on battlefield
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.tapped = true;
            obj.entered_battlefield_turn = Some(1); // no summoning sickness
        }

        // Set up combat state with attacker unblocked
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(1),
            }],
            ..Default::default()
        });

        // Create Ninjutsu creature in hand
        let ninja_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ninja of the Deep Hours".to_string(),
            crate::types::zones::Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&ninja_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.keywords.push(Keyword::Ninjutsu(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            }));
            obj.base_keywords = obj.keywords.clone();
        }

        (state, attacker_id, ninja_id)
    }

    #[test]
    fn ninjutsu_returns_attacker_to_hand() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // Attacker should be in hand
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            crate::types::zones::Zone::Hand,
            "Attacker should be returned to hand"
        );
    }

    #[test]
    fn ninjutsu_creature_enters_battlefield_tapped_attacking() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // Ninjutsu creature should be on battlefield, tapped, attacking
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(ninja.zone, crate::types::zones::Zone::Battlefield);
        assert!(ninja.tapped, "Ninjutsu creature should be tapped");
        assert_eq!(
            ninja.entered_battlefield_turn,
            Some(state.turn_number),
            "Should have summoning sickness"
        );

        // Should be in combat attackers
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.iter().any(|a| a.object_id == ninja_id),
            "Ninjutsu creature should be in attackers list"
        );
        // Should be attacking same player as returned attacker
        let ninja_attacker = combat
            .attackers
            .iter()
            .find(|a| a.object_id == ninja_id)
            .unwrap();
        assert_eq!(
            ninja_attacker.defending_player,
            PlayerId(1),
            "Should attack same player"
        );
    }

    #[test]
    fn ninjutsu_creature_does_not_fire_attack_triggers() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // CR 702.49c: No AttackersDeclared event should be emitted for the Ninjutsu creature
        let has_attackers_declared = events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackersDeclared { .. }));
        assert!(
            !has_attackers_declared,
            "No AttackersDeclared event should fire for Ninjutsu creature"
        );
    }

    #[test]
    fn ninjutsu_fails_if_attacker_is_blocked() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;

        // Add a blocker assignment
        let blocker_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Wall".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .combat
            .as_mut()
            .unwrap()
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);

        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        );
        assert!(result.is_err(), "Should fail when attacker is blocked");
    }

    #[test]
    fn ninjutsu_fails_without_combat() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        state.combat = None; // Remove combat

        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        );
        assert!(result.is_err(), "Should fail without active combat");
    }

    #[test]
    fn ninjutsu_cards_in_hand_finds_ninja() {
        let (state, _attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;

        let cards = ninjutsu_cards_in_hand(&state, PlayerId(0));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0], ninja_card_id);
    }

    #[test]
    fn ninjutsu_cards_in_hand_empty_when_no_ninjas() {
        let state = GameState::new_two_player(42);
        let cards = ninjutsu_cards_in_hand(&state, PlayerId(0));
        assert!(cards.is_empty());
    }
}
