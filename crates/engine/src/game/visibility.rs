use std::collections::HashSet;

use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::players;
use super::turn_control;

/// Returns a filtered copy of the game state for the given viewer.
/// Hides all opponents' hand contents and all library contents except where the
/// viewer is explicitly allowed to see them.
pub fn filter_state_for_viewer(state: &GameState, viewer: PlayerId) -> GameState {
    let mut filtered = state.clone();
    let can_view_private_for_player = |player: PlayerId| {
        player == viewer
            || (player == state.active_player
                && turn_control::viewer_controls_active_turn(state, viewer))
    };

    let opponents = players::opponents(state, viewer);
    let opp_hand_ids: Vec<ObjectId> = opponents
        .iter()
        .copied()
        .filter(|&opp| !can_view_private_for_player(opp))
        .flat_map(|opp| filtered.players[opp.0 as usize].hand.iter().copied())
        .collect();
    for obj_id in opp_hand_ids {
        if !state.revealed_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
        }
    }

    let (manifest_dread_visible, manifest_dread_cards): (HashSet<ObjectId>, HashSet<ObjectId>) =
        if let WaitingFor::ManifestDreadChoice { player, ref cards } = filtered.waiting_for {
            let all_cards: HashSet<ObjectId> = cards.iter().copied().collect();
            if can_view_private_for_player(player) {
                (all_cards.clone(), all_cards)
            } else {
                (HashSet::new(), all_cards)
            }
        } else {
            (HashSet::new(), HashSet::new())
        };

    let dig_visible: HashSet<ObjectId> = if let WaitingFor::DigChoice {
        player, ref cards, ..
    } = filtered.waiting_for
    {
        if can_view_private_for_player(player) {
            cards.iter().copied().collect()
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    let search_visible: HashSet<ObjectId> =
        if let WaitingFor::SearchChoice {
            player, ref cards, ..
        } = filtered.waiting_for
        {
            if can_view_private_for_player(player) {
                cards.iter().copied().collect()
            } else {
                HashSet::new()
            }
        } else {
            HashSet::new()
        };

    let effect_zone_hand_cards: HashSet<ObjectId> = if let WaitingFor::EffectZoneChoice {
        zone: Zone::Hand,
        ref cards,
        ..
    } = filtered.waiting_for
    {
        cards.iter().copied().collect()
    } else {
        HashSet::new()
    };

    let all_library_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.library.iter().copied())
        .collect();
    for obj_id in all_library_ids {
        let visible = manifest_dread_visible.contains(&obj_id)
            || dig_visible.contains(&obj_id)
            || search_visible.contains(&obj_id)
            // CR 701.20b: Revealed cards are visible to all players. For reveal-digs
            // ("reveal the top N"), dig cards are also in revealed_cards and must remain
            // public during DigChoice. For private digs ("look at"), revealed_cards won't
            // contain dig cards, so the exclusion still applies.
            || (state.revealed_cards.contains(&obj_id)
                && !manifest_dread_cards.contains(&obj_id));
        if !visible && !effect_zone_hand_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
        }
    }

    if let WaitingFor::ManifestDreadChoice { player, ref cards } = state.waiting_for {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::ManifestDreadChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    if let WaitingFor::DigChoice {
        player,
        ref cards,
        keep_count,
        up_to,
        ref selectable_cards,
        kept_destination,
        rest_destination,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::DigChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                keep_count,
                up_to,
                selectable_cards: selectable_cards.iter().map(|_| ObjectId(0)).collect(),
                kept_destination,
                rest_destination,
                source_id,
            };
        }
    }

    if let WaitingFor::LearnChoice {
        player,
        ref hand_cards,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::LearnChoice {
                player,
                hand_cards: hand_cards.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    if let WaitingFor::SearchChoice {
        player,
        ref cards,
        count,
        reveal,
        up_to,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::SearchChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                reveal,
                up_to,
            };
        }
    }

    if let WaitingFor::ChooseFromZoneChoice {
        player,
        ref cards,
        count,
        up_to,
        ref constraint,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::ChooseFromZoneChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                up_to,
                constraint: constraint.clone(),
                source_id,
            };
        }
    }

    if let WaitingFor::EffectZoneChoice {
        player,
        ref cards,
        count,
        up_to,
        source_id,
        effect_kind,
        zone,
        destination,
        enter_tapped,
        enter_transformed,
        under_your_control,
        enters_attacking,
        owner_library,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) && zone == Zone::Hand {
            filtered.waiting_for = WaitingFor::EffectZoneChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                up_to,
                source_id,
                effect_kind,
                zone,
                destination,
                enter_tapped,
                enter_transformed,
                under_your_control,
                enters_attacking,
                owner_library,
            };
        }
    }

    filtered.auto_pass.retain(|pid, _| *pid == viewer);
    filtered.phase_stops.retain(|pid, _| *pid == viewer);
    filtered
        .lands_tapped_for_mana
        .retain(|pid, _| *pid == viewer);

    // CR 601.2 + CR 408: A spell being cast is on the stack and is public information —
    // caster, targets, chosen X values, and pending mana payment are all visible to
    // opponents. The old behavior of clearing `pending_cast` for non-casters was both
    // rules-incorrect and inconsistent with the inline `pending_cast` fields embedded in
    // `WaitingFor` variants (ChooseXValue, TargetSelection, etc.), which were already
    // leaking through unfiltered. `PendingCast` itself carries only public data
    // (object_id, card_id, ability, cost) — the card's identity is already visible via
    // the stack object.

    for pool in &mut filtered.deck_pools {
        if pool.player != viewer {
            pool.registered_main.clear();
            pool.registered_sideboard.clear();
            pool.current_main.clear();
            pool.current_sideboard.clear();
        }
    }

    filtered
}

fn hide_card(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.face_down = true;
        obj.name = "Hidden Card".to_string();
        obj.abilities.clear();
        obj.keywords.clear();
        obj.base_keywords.clear();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = None;
        obj.color.clear();
        obj.base_color.clear();
        obj.trigger_definitions.clear();
        obj.replacement_definitions.clear();
        obj.static_definitions.clear();
        obj.casting_permissions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{CastingVariant, PendingCast};
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::zones::Zone;

    fn dummy_pending_cast(
        object_id: ObjectId,
        card_id: CardId,
        caster: PlayerId,
    ) -> Box<PendingCast> {
        Box::new(PendingCast {
            object_id,
            card_id,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Dummy".to_string(),
                    description: None,
                },
                vec![],
                object_id,
                caster,
            ),
            cost: ManaCost::NoCost,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: vec![],
            casting_variant: CastingVariant::Normal,
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
        })
    }

    #[test]
    fn search_choice_is_visible_to_turn_controller() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden Tutor Target".to_string(),
            Zone::Library,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            reveal: false,
            up_to: false,
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        match filtered.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => assert_eq!(cards, vec![card_id]),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
        assert_eq!(
            filtered.objects.get(&card_id).map(|obj| obj.name.as_str()),
            Some("Hidden Tutor Target")
        );
    }

    #[test]
    fn search_choice_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden Tutor Target".to_string(),
            Zone::Library,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            reveal: false,
            up_to: false,
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(2));

        match filtered.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => assert_eq!(cards, vec![ObjectId(0)]),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn opponent_commander_in_command_zone_remains_visible() {
        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        let commander_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&commander_id).unwrap().is_commander = true;

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(filtered.command_zone, im::vector![commander_id]);
        let commander = filtered.objects.get(&commander_id).unwrap();
        assert_eq!(commander.name, "Opponent Commander");
        assert!(!commander.face_down);
        assert_eq!(commander.zone, Zone::Command);
        assert!(commander.is_commander);
    }

    // CR 601.2 + CR 408: A spell being cast is on the stack and is public information —
    // opponents see the caster, the spell, chosen targets, and mana payment progress
    // as it happens (the MTGA "Opponent is casting X" experience). The tests below guard
    // against regression of the pre-correction behavior that cleared `pending_cast` for
    // non-caster viewers, which was both rules-incorrect and inconsistent with the
    // inline `pending_cast` fields on `WaitingFor::{ChooseXValue, TargetSelection,
    // ModeChoice, ...}` that always leaked through unfiltered.

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_mana_payment() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        state.pending_cast = Some(dummy_pending_cast(ObjectId(10), CardId(1), PlayerId(0)));

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ManaPayment (CR 601.2 + CR 408)"
        );
        let pc = filtered.pending_cast.as_ref().unwrap();
        assert_eq!(pc.object_id, ObjectId(10));
        assert_eq!(pc.card_id, CardId(1));
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_choose_x_value() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(20), CardId(2), PlayerId(0));
        state.waiting_for = WaitingFor::ChooseXValue {
            player: PlayerId(0),
            max: 5,
            pending_cast: pending.clone(),
            convoke_mode: None,
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ChooseXValue (CR 601.2 + CR 408)"
        );
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_target_selection() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(30), CardId(3), PlayerId(0));
        state.waiting_for = WaitingFor::TargetSelection {
            player: PlayerId(0),
            pending_cast: pending.clone(),
            target_slots: vec![],
            selection: Default::default(),
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during TargetSelection (CR 601.2 + CR 408)"
        );
    }

    #[test]
    fn pending_cast_remains_visible_to_non_caster_during_mode_choice() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let pending = dummy_pending_cast(ObjectId(40), CardId(4), PlayerId(0));
        state.waiting_for = WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: crate::types::ability::ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                ..Default::default()
            },
            pending_cast: pending.clone(),
        };
        state.pending_cast = Some(pending);

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(
            filtered.pending_cast.is_some(),
            "non-caster must see opponent's pending cast during ModeChoice (CR 601.2 + CR 408)"
        );
    }

    #[test]
    fn choose_from_zone_choice_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Tracked Card".to_string(),
            Zone::Exile,
        );
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            up_to: false,
            constraint: None,
            source_id: ObjectId(99),
        };

        let filtered = filter_state_for_viewer(&state, PlayerId(2));

        match filtered.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0)])
            }
            other => panic!("expected ChooseFromZoneChoice, got {other:?}"),
        }
    }
}
