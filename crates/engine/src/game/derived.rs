use crate::game::combat::has_summoning_sickness;
use crate::game::coverage::unimplemented_mechanics;
use crate::game::devotion::count_devotion;
use crate::game::mana_abilities;
use crate::game::mana_sources::display_land_mana_colors;
use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::types::ability::StaticCondition;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::statics::StaticMode;

/// Compute display-only derived fields (CR 302.6 summoning sickness, CR 700.5 devotion).
///
/// This must be called by any consumer (WASM, Tauri, server) before
/// serializing the state to the frontend. It sets:
/// - `GameObject::unimplemented_mechanics`
/// - `GameObject::has_summoning_sickness`
/// - `GameObject::devotion` (for Theros gods pattern)
/// - `GameObject::commander_tax` (CR 903.8 commander tax)
/// - `Player::can_look_at_top_of_library`
pub fn derive_display_state(state: &mut GameState) {
    let turn = state.turn_number;
    let dirty = &state.public_state_dirty;

    let object_ids: Vec<_> = if dirty.all_objects_dirty {
        state.objects.keys().copied().collect()
    } else {
        dirty.dirty_objects.iter().copied().collect()
    };
    for id in object_ids {
        let (unimplemented, summoning_sickness, mana_idx) = {
            let Some(obj) = state.objects.get(&id) else {
                continue;
            };
            let mana_idx = obj
                .abilities
                .iter()
                .enumerate()
                .find(|(idx, ability)| {
                    mana_abilities::is_mana_ability(ability)
                        && mana_abilities::can_activate_mana_ability_now(
                            state,
                            obj.controller,
                            obj.id,
                            *idx,
                            ability,
                        )
                })
                .map(|(idx, _)| idx);
            (
                unimplemented_mechanics(obj),
                // CR 302.6: Creature must have been under controller's control since turn began to attack or {T}.
                has_summoning_sickness(obj, turn),
                mana_idx,
            )
        };

        let obj = state.objects.get_mut(&id).expect("object exists");
        obj.unimplemented_mechanics = unimplemented;
        obj.has_summoning_sickness = summoning_sickness;
        obj.has_mana_ability = mana_idx.is_some();
        obj.mana_ability_index = mana_idx;
        obj.available_mana_colors.clear();
    }

    // Compute per-card devotion for cards with DevotionGE conditions
    // (Theros gods pattern — derive colors from the card's own base_color)
    if dirty.all_objects_dirty || dirty.battlefield_display_dirty {
        let devotion_cards: Vec<_> = state
            .objects
            .iter()
            .filter_map(|(&id, obj)| {
                // Classification scan: we only need to know whether this
                // object *declares* a devotion-conditioned static so the
                // dirty-tracker can pick it up. CR 604.1 / CR 702.26b
                // gating is applied later when the static actually
                // evaluates — here we must see every declared definition,
                // so `iter_unchecked` is the correct intent.
                let has_devotion_static =
                    obj.static_definitions
                        .iter_unchecked()
                        .any(|def| match &def.condition {
                            Some(StaticCondition::DevotionGE { .. }) => true,
                            Some(StaticCondition::Not { condition })
                                if matches!(
                                    condition.as_ref(),
                                    StaticCondition::DevotionGE { .. }
                                ) =>
                            {
                                true
                            }
                            _ => false,
                        });
                if has_devotion_static && !obj.base_color.is_empty() {
                    let devotion = count_devotion(state, obj.controller, &obj.base_color);
                    Some((id, devotion))
                } else {
                    None
                }
            })
            .collect();
        for (id, devotion) in devotion_cards {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.devotion = Some(devotion);
            }
        }
    }

    // CR 903.8: Compute commander tax for display.
    if dirty.all_objects_dirty || dirty.battlefield_display_dirty {
        let commander_taxes: Vec<_> = state
            .objects
            .iter()
            .filter_map(|(&id, obj)| {
                if obj.is_commander {
                    Some((id, super::commander::commander_tax(state, id)))
                } else {
                    None
                }
            })
            .collect();
        for (id, tax) in commander_taxes {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.commander_tax = Some(tax);
            }
        }
    }

    // Compute dynamic land frame colors from currently available mana options.
    if dirty.all_objects_dirty || dirty.mana_display_dirty || dirty.battlefield_display_dirty {
        let mana_color_cards: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|&id| {
                let obj = state.objects.get(&id)?;
                if !obj.card_types.core_types.contains(&CoreType::Land) {
                    return None;
                }
                let colors = display_land_mana_colors(state, id, obj.controller);
                Some((id, colors))
            })
            .collect();
        for (id, colors) in mana_color_cards {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.available_mana_colors = colors;
            }
        }
    }

    // Compute per-player derived fields
    if dirty.all_players_dirty || dirty.battlefield_display_dirty {
        let peek_flags: Vec<bool> = state
            .players
            .iter()
            .map(|p| {
                let ctx = StaticCheckContext {
                    player_id: Some(p.id),
                    ..Default::default()
                };
                check_static_ability(state, StaticMode::MayLookAtTopOfLibrary, &ctx)
            })
            .collect();
        for (i, flag) in peek_flags.into_iter().enumerate() {
            state.players[i].can_look_at_top_of_library = flag;
        }
    } else {
        let dirty_players: Vec<_> = dirty.dirty_players.iter().copied().collect();
        for player_id in dirty_players {
            let ctx = StaticCheckContext {
                player_id: Some(player_id),
                ..Default::default()
            };
            let flag = check_static_ability(state, StaticMode::MayLookAtTopOfLibrary, &ctx);
            if let Some(player) = state
                .players
                .iter_mut()
                .find(|player| player.id == player_id)
            {
                player.can_look_at_top_of_library = flag;
            }
        }
    }

    // Derive has_pending_cast so the frontend can read it directly
    // without maintaining a parallel list of casting-flow WaitingFor states.
    state.has_pending_cast = state.waiting_for.has_pending_cast();

    // Invariant: the two storage sites for "am I mid-cast" must agree. If
    // `waiting_for` says we're mid-cast, `GameState::pending_cast` must be
    // populated (either inline via the variant's `pending_cast_ref`, or on
    // the outer state for `ManaPayment`). Drift here is the bug class that
    // caused the `ChooseXValue` omission and the `Unsummon` cast/cancel loop
    // regression — this assert makes future drift surface immediately.
    debug_assert!(
        !state.has_pending_cast
            || state.pending_cast.is_some()
            || state.waiting_for.pending_cast_ref().is_some(),
        "has_pending_cast is true but no PendingCast is reachable — drift in {:?}",
        std::mem::discriminant(&state.waiting_for)
    );
}

/// Commander damage received by `victim`, grouped by the commander's
/// controller (the attacking opponent). Each inner entry is
/// `(commander_object_id, damage)`. The frontend renders one badge per
/// entry, so this preserves the "separate commanders from the same
/// opponent" distinction (partners, backgrounds) while giving the HUD a
/// ready-to-render per-opponent summary without client-side filtering.
///
/// CR 903.10a tracks commander damage per commander; this helper adds the
/// display-oriented grouping-by-controller layer that clients need.
pub fn commander_damage_received(
    state: &GameState,
    victim: crate::types::player::PlayerId,
) -> std::collections::BTreeMap<
    crate::types::player::PlayerId,
    Vec<(crate::types::identifiers::ObjectId, u32)>,
> {
    let mut out: std::collections::BTreeMap<
        crate::types::player::PlayerId,
        Vec<(crate::types::identifiers::ObjectId, u32)>,
    > = std::collections::BTreeMap::new();
    for entry in &state.commander_damage {
        if entry.player != victim {
            continue;
        }
        // Look up the commander's controller (the attacking opponent).
        // A commander that has left the battlefield still exists in
        // state.objects — the Command zone sticks it back there — so the
        // lookup is stable across zone changes.
        let Some(commander_obj) = state.objects.get(&entry.commander) else {
            continue;
        };
        out.entry(commander_obj.controller)
            .or_default()
            .push((entry.commander, entry.damage));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn derive_sets_summoning_sickness_for_new_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(1);

        derive_display_state(&mut state);

        assert!(state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_clears_summoning_sickness_for_old_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 3;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(1);

        derive_display_state(&mut state);

        assert!(!state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_sets_unimplemented_flag() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        );

        derive_display_state(&mut state);

        // Should have set the flag (false for a card with no mechanics)
        let obj = &state.objects[&id];
        assert!(obj.unimplemented_mechanics.is_empty());
    }

    #[test]
    fn derive_sets_can_look_at_top_default_false() {
        let mut state = GameState::new_two_player(42);

        derive_display_state(&mut state);

        assert!(!state.players[0].can_look_at_top_of_library);
        assert!(!state.players[1].can_look_at_top_of_library);
    }

    #[test]
    fn derive_sets_commander_tax_for_commander() {
        use crate::game::commander::record_commander_cast;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&id).unwrap().is_commander = true;

        // No casts yet — tax should be 0
        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, Some(0));

        // After 2 casts — tax should be 4
        record_commander_cast(&mut state, id);
        record_commander_cast(&mut state, id);
        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, Some(4));
    }

    #[test]
    fn derive_does_not_set_commander_tax_for_non_commander() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        derive_display_state(&mut state);
        assert_eq!(state.objects[&id].commander_tax, None);
    }

    #[test]
    fn commander_damage_received_groups_by_controller() {
        use crate::types::game_state::CommanderDamageEntry;
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);
        // Two commanders controlled by different opponents, both hitting P0.
        let cmdr_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Atraxa".to_string(),
            Zone::Battlefield,
        );
        let cmdr_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(2),
            "Breya".to_string(),
            Zone::Battlefield,
        );
        state.commander_damage = vec![
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: cmdr_a,
                damage: 12,
            },
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: cmdr_b,
                damage: 7,
            },
            // Unrelated entry: someone else's damage — must NOT appear in P0's map.
            CommanderDamageEntry {
                player: PlayerId(1),
                commander: ObjectId(9999),
                damage: 3,
            },
        ];

        let grouped = commander_damage_received(&state, PlayerId(0));
        assert_eq!(grouped.len(), 2, "expected two attacking opponents");
        assert_eq!(grouped[&PlayerId(1)], vec![(cmdr_a, 12)]);
        assert_eq!(grouped[&PlayerId(2)], vec![(cmdr_b, 7)]);
    }

    #[test]
    fn commander_damage_received_collects_partners_under_same_controller() {
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new_two_player(42);
        // Partner commanders: both controlled by P1, both hitting P0.
        let partner_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Ravos".to_string(),
            Zone::Battlefield,
        );
        let partner_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Tymna".to_string(),
            Zone::Battlefield,
        );
        state.commander_damage = vec![
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: partner_a,
                damage: 6,
            },
            CommanderDamageEntry {
                player: PlayerId(0),
                commander: partner_b,
                damage: 4,
            },
        ];

        let grouped = commander_damage_received(&state, PlayerId(0));
        assert_eq!(grouped.len(), 1);
        let entries = &grouped[&PlayerId(1)];
        assert_eq!(
            entries.len(),
            2,
            "partners kept as distinct entries under same controller"
        );
    }
}
