use crate::game::layers;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::zones;

const MAX_SBA_ITERATIONS: u32 = 9;

/// CR 704.3: Run state-based actions in a fixpoint loop until no more actions are performed,
/// capped at MAX_SBA_ITERATIONS.
pub fn check_state_based_actions(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // Evaluate layers before SBA checks so computed P/T is current
    if state.layers_dirty {
        layers::evaluate_layers(state);
    }

    for _ in 0..MAX_SBA_ITERATIONS {
        let mut any_performed = false;

        // CR 704.5a: A player with 0 or less life loses the game.
        check_player_life(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.5c: A player with ten or more poison counters loses the game.
        check_poison_counters(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.6d: A player who has been dealt 21 or more combat damage by the same
        // commander loses the game.
        check_commander_damage(state, events, &mut any_performed);

        // If game is over, stop immediately
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }

        // CR 704.5f: A creature with toughness 0 or less is put into its owner's graveyard.
        check_zero_toughness(state, events, &mut any_performed);

        // CR 704.5g: A creature with lethal damage marked on it is destroyed.
        check_lethal_damage(state, events, &mut any_performed);

        // CR 704.5j: If a player controls two or more legendary permanents with the same name,
        // that player chooses one and the rest are put into their owners' graveyards.
        check_legend_rule(state, events, &mut any_performed);

        // CR 704.5n: If an Aura is attached to an illegal object or player, it is put into
        // its owner's graveyard.
        check_unattached_auras(state, events, &mut any_performed);

        // CR 704.5p: If an Equipment is attached to an illegal permanent, it becomes unattached.
        check_unattached_equipment(state, &mut any_performed);

        // CR 704.5i: If a planeswalker has loyalty 0, it is put into its owner's graveyard.
        check_zero_loyalty(state, events, &mut any_performed);

        // CR 704.5s + CR 714.4: If a Saga has lore counters >= its final chapter number,
        // and no chapter ability has triggered but not yet left the stack, sacrifice it.
        check_saga_sacrifice(state, events, &mut any_performed);

        if !any_performed {
            break;
        }
    }
}

fn check_player_life(state: &mut GameState, events: &mut Vec<GameEvent>, any_performed: &mut bool) {
    // Collect all players who should be eliminated (check all, not just first)
    let to_eliminate: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.life <= 0)
        .map(|p| p.id)
        .collect();

    for loser in to_eliminate {
        events.push(GameEvent::PlayerLost { player_id: loser });
        super::elimination::eliminate_player(state, loser, events);
        *any_performed = true;
    }
}

fn check_poison_counters(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_eliminate: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.poison_counters >= 10)
        .map(|p| p.id)
        .collect();

    for loser in to_eliminate {
        events.push(GameEvent::PlayerLost { player_id: loser });
        super::elimination::eliminate_player(state, loser, events);
        *any_performed = true;
    }
}

fn check_commander_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let threshold = match state.format_config.commander_damage_threshold {
        Some(t) => t as u32,
        None => return, // Not a Commander format
    };

    // Collect players who should be eliminated
    let to_eliminate: Vec<PlayerId> = state
        .commander_damage
        .iter()
        .filter(|entry| entry.damage >= threshold)
        .map(|entry| entry.player)
        .filter(|pid| !state.eliminated_players.contains(pid))
        .collect();

    for player_id in to_eliminate {
        super::elimination::eliminate_player(state, player_id, events);
        *any_performed = true;
    }
}

fn check_zero_toughness(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Creature)
                        && obj.toughness.is_some_and(|t| t <= 0)
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

fn check_lethal_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Creature)
                        && (
                            // Normal lethal damage: damage >= toughness
                            obj.toughness.is_some_and(|t| obj.damage_marked >= t as u32 && t > 0)
                            // Deathtouch: any amount of damage from a deathtouch source is lethal
                            || (obj.dealt_deathtouch_damage && obj.damage_marked > 0)
                        )
                        // Indestructible creatures are not destroyed by lethal damage
                        && !obj.has_keyword(&crate::types::keywords::Keyword::Indestructible)
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

fn check_legend_rule(state: &mut GameState, events: &mut Vec<GameEvent>, any_performed: &mut bool) {
    for player_idx in 0..state.players.len() {
        let player_id = state.players[player_idx].id;

        // Group legendaries by name
        let legendaries: Vec<_> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| {
                        obj.controller == player_id
                            && obj.card_types.supertypes.contains(&Supertype::Legendary)
                    })
                    .unwrap_or(false)
            })
            .collect();

        // Group by name
        let mut by_name: std::collections::HashMap<String, Vec<_>> =
            std::collections::HashMap::new();
        for id in legendaries {
            if let Some(obj) = state.objects.get(&id) {
                by_name.entry(obj.name.clone()).or_default().push(id);
            }
        }

        // For names with 2+, keep newest (highest entered_battlefield_turn), remove rest
        for (_name, mut ids) in by_name {
            if ids.len() < 2 {
                continue;
            }

            // Sort by entered_battlefield_turn descending (newest first)
            ids.sort_by(|a, b| {
                let turn_a = state
                    .objects
                    .get(a)
                    .and_then(|o| o.entered_battlefield_turn)
                    .unwrap_or(0);
                let turn_b = state
                    .objects
                    .get(b)
                    .and_then(|o| o.entered_battlefield_turn)
                    .unwrap_or(0);
                turn_b.cmp(&turn_a)
            });

            // Skip the first (newest), remove the rest
            for &id in &ids[1..] {
                zones::move_to_zone(state, id, Zone::Graveyard, events);
                *any_performed = true;
            }
        }
    }
}

fn check_unattached_auras(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_remove: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    // Check if it's an aura (Enchantment with attached_to)
                    obj.card_types.core_types.contains(&CoreType::Enchantment)
                        && obj.attached_to.is_some()
                        && !is_valid_attachment_target(state, obj.attached_to.unwrap())
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_remove {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

fn check_unattached_equipment(state: &mut GameState, any_performed: &mut bool) {
    let to_unattach: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.subtypes.contains(&"Equipment".to_string())
                        && obj.attached_to.is_some()
                        && !is_valid_attachment_target(state, obj.attached_to.unwrap())
                })
                .unwrap_or(false)
        })
        .collect();

    for equipment_id in to_unattach {
        // Clear the attachment reference on the equipment
        if let Some(old_target_id) = state
            .objects
            .get(&equipment_id)
            .and_then(|obj| obj.attached_to)
        {
            // Remove from old target's attachments if it still exists
            if let Some(old_target) = state.objects.get_mut(&old_target_id) {
                old_target.attachments.retain(|&id| id != equipment_id);
            }
        }
        if let Some(equipment) = state.objects.get_mut(&equipment_id) {
            equipment.attached_to = None;
        }
        *any_performed = true;
    }
}

fn check_zero_loyalty(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    let to_destroy: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| {
                    obj.card_types.core_types.contains(&CoreType::Planeswalker)
                        && obj.loyalty.is_some_and(|l| l == 0)
                })
                .unwrap_or(false)
        })
        .collect();

    for id in to_destroy {
        zones::move_to_zone(state, id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

/// CR 704.5s + CR 714.4: Sacrifice Sagas that have reached their final chapter,
/// unless a chapter ability from that Saga is still on the stack or a lore counter
/// was just added (meaning process_triggers hasn't placed the chapter trigger yet).
fn check_saga_sacrifice(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    any_performed: &mut bool,
) {
    use crate::game::game_object::CounterType;
    use crate::types::game_state::StackEntryKind;

    let to_sacrifice: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = match state.objects.get(id) {
                Some(o) => o,
                None => return false,
            };
            let final_ch = match obj.final_chapter_number() {
                Some(n) => n,
                None => return false,
            };
            let lore_count = obj.counters.get(&CounterType::Lore).copied().unwrap_or(0);
            if lore_count < final_ch {
                return false;
            }

            // CR 714.4: Don't sacrifice while a chapter trigger from this Saga is on the stack.
            let chapter_on_stack = state.stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == *id
                )
            });
            if chapter_on_stack {
                return false;
            }

            // CR 714.4 deferral: A lore counter was just added in this SBA batch —
            // process_triggers hasn't run yet, so defer sacrifice for one pass.
            let pending_lore_event = events.iter().any(|e| {
                matches!(
                    e,
                    GameEvent::CounterAdded {
                        object_id,
                        counter_type: CounterType::Lore,
                        ..
                    } if *object_id == *id
                )
            });
            if pending_lore_event {
                return false;
            }

            true
        })
        .collect();

    for saga_id in to_sacrifice {
        let owner = state
            .objects
            .get(&saga_id)
            .map(|obj| obj.owner)
            .unwrap_or(crate::types::player::PlayerId(0));
        events.push(GameEvent::PermanentSacrificed {
            object_id: saga_id,
            player_id: owner,
        });
        zones::move_to_zone(state, saga_id, Zone::Graveyard, events);
        *any_performed = true;
    }
}

fn is_valid_attachment_target(
    state: &GameState,
    target_id: crate::types::identifiers::ObjectId,
) -> bool {
    state
        .objects
        .get(&target_id)
        .map(|obj| obj.zone == Zone::Battlefield)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn create_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    // --- 2-player SBA tests (backward compatible) ---

    #[test]
    fn sba_zero_life_player_loses() {
        let mut state = setup();
        state.players[0].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn sba_negative_life_player_loses() {
        let mut state = setup();
        state.players[1].life = -5;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn sba_zero_toughness_creature_dies() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Weakling", 1, 0);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
    }

    #[test]
    fn sba_lethal_damage_creature_dies() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 2;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
    }

    #[test]
    fn sba_healthy_creature_survives() {
        let mut state = setup();
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 1;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn sba_legend_rule_keeps_newest() {
        let mut state = setup();
        state.turn_number = 1;
        let id1 = create_creature(&mut state, CardId(1), PlayerId(0), "Thalia", 2, 1);
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .entered_battlefield_turn = Some(1);

        state.turn_number = 2;
        let id2 = create_creature(&mut state, CardId(2), PlayerId(0), "Thalia", 2, 1);
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .entered_battlefield_turn = Some(2);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Newest (id2, turn 2) should survive, oldest (id1, turn 1) should die
        assert!(state.battlefield.contains(&id2));
        assert!(!state.battlefield.contains(&id1));
        assert!(state.players[0].graveyard.contains(&id1));
    }

    #[test]
    fn sba_unattached_aura_goes_to_graveyard() {
        let mut state = setup();
        // Create an enchantment attached to a nonexistent object
        let aura_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.attached_to = Some(ObjectId(999)); // nonexistent target

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&aura_id));
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    #[test]
    fn sba_fixpoint_handles_cascading_deaths() {
        let mut state = setup();
        // Create a creature that will die from lethal damage
        let id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&id).unwrap().damage_marked = 3;

        // Create an aura attached to that creature
        let aura_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Aura".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.attached_to = Some(id);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Both should be in graveyard (creature dies, then aura detaches and dies)
        assert!(!state.battlefield.contains(&id));
        assert!(!state.battlefield.contains(&aura_id));
    }

    #[test]
    fn sba_poison_10_player_loses() {
        let mut state = setup();
        state.players[0].poison_counters = 10;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(0)
            }
        )));
    }

    #[test]
    fn sba_poison_9_player_survives() {
        let mut state = setup();
        state.players[0].poison_counters = 9;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_no_actions_when_nothing_to_do() {
        let mut state = setup();
        create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // No zone change events should have been generated
        assert!(events.is_empty());
    }

    #[test]
    fn sba_equipment_unattaches_when_creature_dies() {
        let mut state = setup();
        // Create a creature that will die
        let creature_id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&creature_id).unwrap().damage_marked = 3; // lethal

        // Create equipment attached to that creature
        let equip_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&equip_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.attached_to = Some(creature_id);

        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .attachments
            .push(equip_id);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Creature should be dead
        assert!(!state.battlefield.contains(&creature_id));
        // Equipment should still be on battlefield but unattached
        assert!(state.battlefield.contains(&equip_id));
        assert_eq!(state.objects.get(&equip_id).unwrap().attached_to, None);
    }

    #[test]
    fn sba_equipment_on_battlefield_without_attachment_stays() {
        let mut state = setup();
        // Equipment on battlefield with no attached_to is a valid state
        let equip_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&equip_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Equipment should stay on battlefield, no events generated
        assert!(state.battlefield.contains(&equip_id));
        assert!(events.is_empty());
    }

    #[test]
    fn sba_aura_still_goes_to_graveyard_when_target_leaves() {
        let mut state = setup();
        // Create a creature that will die
        let creature_id = create_creature(&mut state, CardId(1), PlayerId(0), "Bear", 2, 2);
        state.objects.get_mut(&creature_id).unwrap().damage_marked = 3;

        // Create an aura attached to the creature
        let aura_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.attached_to = Some(creature_id);

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // Both should be gone from battlefield
        assert!(!state.battlefield.contains(&creature_id));
        assert!(!state.battlefield.contains(&aura_id));
        // Aura goes to graveyard (not stays on battlefield like equipment)
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    fn create_planeswalker(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(loyalty);
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    #[test]
    fn sba_zero_loyalty_planeswalker_dies() {
        let mut state = setup();
        let pw = create_planeswalker(&mut state, CardId(1), PlayerId(0), "Jace", 0);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&pw));
        assert!(state.players[0].graveyard.contains(&pw));
    }

    #[test]
    fn sba_positive_loyalty_planeswalker_survives() {
        let mut state = setup();
        let pw = create_planeswalker(&mut state, CardId(1), PlayerId(0), "Jace", 3);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&pw));
    }

    // --- N-player SBA tests ---

    #[test]
    fn sba_three_player_one_dies_game_continues() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.players[1].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 eliminated but game continues
        assert!(state.players[1].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_three_player_two_die_simultaneously_ends_game() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.players[1].life = 0;
        state.players[2].life = -3;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Both eliminated, P0 wins
        assert!(state.players[1].is_eliminated);
        assert!(state.players[2].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn sba_eliminated_player_not_re_checked() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        // P1 already eliminated with 0 life
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));
        state.players[1].life = 0;
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // No new events for already-eliminated player
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerLost {
                player_id: PlayerId(1)
            }
        )));
    }

    #[test]
    fn sba_commander_damage_21_eliminates_player() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let cmd_id = ObjectId(999);
        // Player 1 has taken 21 commander damage from cmd_id
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 21,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 should be eliminated
        assert!(state.players[1].is_eliminated);
        assert!(state.eliminated_players.contains(&PlayerId(1)));
        // Game should NOT be over (3 remaining players)
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn sba_commander_damage_20_does_not_eliminate() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let cmd_id = ObjectId(999);
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 20,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // P1 should NOT be eliminated (threshold is 21)
        assert!(!state.players[1].is_eliminated);
    }

    #[test]
    fn sba_commander_damage_skipped_in_non_commander_format() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let cmd_id = ObjectId(999);
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd_id,
            damage: 100,
        });
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Not a commander format -> threshold is None -> no elimination
        assert!(!state.players[1].is_eliminated);
    }

    #[test]
    fn sba_2hg_team_dies_together() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.players[0].life = 0; // Team A player dies
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        // Both team A members eliminated
        assert!(state.players[0].is_eliminated);
        assert!(state.players[1].is_eliminated);
        // Team B wins
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ));
    }

    // --- Saga SBA tests ---

    fn create_saga(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        final_chapter: u32,
    ) -> ObjectId {
        use crate::game::game_object::CounterType;
        use crate::types::ability::{CounterTriggerFilter, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Saga".to_string());
        obj.entered_battlefield_turn = Some(state.turn_number);
        // Add chapter triggers so final_chapter_number() works
        for ch in 1..=final_chapter {
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::CounterAdded).counter_filter(
                    CounterTriggerFilter {
                        counter_type: CounterType::Lore,
                        threshold: Some(ch),
                    },
                ),
            );
        }
        id
    }

    #[test]
    fn saga_sacrificed_at_final_chapter() {
        use crate::game::game_object::CounterType;

        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(!state.battlefield.contains(&id));
        assert!(state.players[0].graveyard.contains(&id));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == id)
        ));
    }

    #[test]
    fn saga_not_sacrificed_below_final() {
        use crate::game::game_object::CounterType;

        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 2);
        let mut events = Vec::new();

        check_state_based_actions(&mut state, &mut events);

        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn saga_not_sacrificed_with_chapter_on_stack() {
        use crate::game::game_object::CounterType;
        use crate::types::ability::{Effect, ResolvedAbility};
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);

        // Put a chapter trigger from this saga on the stack
        state.stack.push(StackEntry {
            id: ObjectId(999),
            source_id: id,
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: id,
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "chapter".into(),
                        description: None,
                    },
                    vec![],
                    id,
                    PlayerId(0),
                ),
                condition: None,
                trigger_event: None,
            },
        });

        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);

        // CR 714.4: Saga survives while chapter trigger is on the stack
        assert!(state.battlefield.contains(&id));
    }

    #[test]
    fn saga_not_sacrificed_with_pending_lore_event() {
        use crate::game::game_object::CounterType;

        let mut state = setup();
        let id = create_saga(&mut state, CardId(1), PlayerId(0), "Saga", 3);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Lore, 3);

        // Simulate a lore counter having just been added in this batch
        let mut events = vec![GameEvent::CounterAdded {
            object_id: id,
            counter_type: CounterType::Lore,
            count: 1,
        }];

        check_state_based_actions(&mut state, &mut events);

        // CR 714.4 deferral: triggers haven't been placed yet
        assert!(state.battlefield.contains(&id));
    }
}
