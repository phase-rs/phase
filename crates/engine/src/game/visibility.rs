use std::collections::HashSet;
use std::sync::Arc;

use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::{ExileCostSourceZone, Zone};

use super::players;
use super::turn_control;

/// Returns a filtered copy of the game state for the given viewer.
/// Hides all opponents' hand contents and all library contents except where the
/// viewer is explicitly allowed to see them.
pub fn filter_state_for_viewer(state: &GameState, viewer: PlayerId) -> GameState {
    let mut filtered = state.clone();
    filtered.pending_begin_game_abilities.clear();
    filtered.resolving_begin_game_abilities = false;
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
    let drawn_choice_hand_cards: HashSet<ObjectId> =
        if let WaitingFor::DrawnThisTurnTopdeckChoice { ref cards, .. } = filtered.waiting_for {
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
        if !visible
            && !effect_zone_hand_cards.contains(&obj_id)
            && !drawn_choice_hand_cards.contains(&obj_id)
        {
            hide_card(&mut filtered, obj_id);
        }
    }

    let hidden_foretold_exile_ids: Vec<ObjectId> = filtered
        .exile
        .iter()
        .copied()
        .filter(|obj_id| {
            state.objects.get(obj_id).is_some_and(|obj| {
                obj.foretold && obj.face_down && !can_view_private_for_player(obj.owner)
            })
        })
        .collect();
    for obj_id in hidden_foretold_exile_ids {
        hide_card(&mut filtered, obj_id);
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
        ref constraint,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::SearchChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                reveal,
                up_to,
                constraint: constraint.clone(),
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

    // CR 400.2: Library and hand are hidden zones — opponents cannot see the
    // identities of cards there. The eligible-cards list for an alternative or
    // additional exile-from-hand cost (Force of Will and the rest of the
    // pitch-spell family) would leak hand contents to opponents (e.g.
    // `cards.len()` reveals the count of blue cards in the caster's hand minus
    // one). Redact `cards` to opaque placeholders for viewers who cannot see
    // the caster's hand. `count` and `pending_cast` are public (CR 601.2 +
    // CR 408 — the spell on the stack is public information).
    // The graveyard variant of `ExileForCost` is intentionally NOT redacted
    // because the graveyard is a public zone (CR 400.2).
    if let WaitingFor::ExileForCost {
        player,
        zone,
        count,
        ref cards,
        ref pending_cast,
    } = state.waiting_for
    {
        if zone == ExileCostSourceZone::Hand && !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::ExileForCost {
                player,
                zone,
                count,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                pending_cast: pending_cast.clone(),
            };
        }
    }

    if let WaitingFor::EffectZoneChoice {
        player,
        ref cards,
        count,
        min_count,
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
                min_count,
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
    if let WaitingFor::DrawnThisTurnTopdeckChoice {
        player,
        ref cards,
        count,
        min_count,
        life_payment,
        source_id,
    } = state.waiting_for
    {
        if !can_view_private_for_player(player) {
            filtered.waiting_for = WaitingFor::DrawnThisTurnTopdeckChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                count,
                min_count,
                life_payment,
                source_id,
            };
        }
    }

    filtered.auto_pass.retain(|pid, _| *pid == viewer);
    filtered.phase_stops.retain(|pid, _| *pid == viewer);
    filtered
        .may_trigger_auto_choices
        .retain(|record| record.key.player == viewer);
    filtered
        .lands_tapped_for_mana
        .retain(|pid, _| *pid == viewer);
    filtered
        .cards_drawn_this_turn
        .retain(|pid, _| can_view_private_for_player(*pid));

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
            // Per-seat redaction: replace the Arc'd decks with fresh empties.
            // Cheaper than `make_mut + clear` because we discard the contents;
            // the original Arcs remain shared by the unfiltered state and any
            // other viewer's filter.
            pool.registered_main = Arc::new(Vec::new());
            pool.registered_sideboard = Arc::new(Vec::new());
            pool.current_main = Arc::new(Vec::new());
            pool.current_sideboard = Arc::new(Vec::new());
        }
    }

    filtered
}

fn hide_card(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.face_down = true;
        obj.name = "Hidden Card".to_string();
        Arc::make_mut(&mut obj.abilities).clear();
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
        obj.foretold = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        AutoMayChoice, CastingVariant, MayTriggerAutoChoiceKey, MayTriggerOrigin,
        PendingBeginGameAbility, PendingCast,
    };
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::zones::{ExileCostSourceZone, Zone};

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
            cast_timing_permission: None,
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
            additional_cost_flow: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
        })
    }

    #[test]
    fn filters_other_players_may_trigger_auto_choices() {
        let mut state = GameState::new_two_player(42);
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id: ObjectId(10),
                origin: MayTriggerOrigin::Printed { trigger_index: 0 },
            },
            AutoMayChoice::Accept,
        );
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(1),
                source_id: ObjectId(11),
                origin: MayTriggerOrigin::Printed { trigger_index: 0 },
            },
            AutoMayChoice::Decline,
        );

        let filtered = filter_state_for_viewer(&state, PlayerId(0));

        assert_eq!(filtered.may_trigger_auto_choices.len(), 1);
        assert_eq!(filtered.may_trigger_auto_choices[0].key.player, PlayerId(0));
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
            constraint: crate::types::ability::SearchSelectionConstraint::None,
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
    fn filtered_state_hides_pending_begin_game_queue() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Opening Hand Card".to_string(),
            Zone::Hand,
        );
        state
            .pending_begin_game_abilities
            .push(PendingBeginGameAbility {
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Hidden Begin Game Ability".to_string(),
                        description: None,
                    },
                    vec![],
                    source,
                    PlayerId(0),
                ),
            });
        state.resolving_begin_game_abilities = true;

        let filtered = filter_state_for_viewer(&state, PlayerId(1));

        assert!(filtered.pending_begin_game_abilities.is_empty());
        assert!(!filtered.resolving_begin_game_abilities);
        assert_eq!(state.pending_begin_game_abilities.len(), 1);
        assert!(state.resolving_begin_game_abilities);
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
            constraint: crate::types::ability::SearchSelectionConstraint::None,
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

    /// CR 400.2: hand is a hidden zone. The eligible-cards list for an
    /// exile-from-hand cost reveals "blue cards in caster's hand − 1" to
    /// opponents and must be redacted, while the caster's own view is
    /// preserved.
    #[test]
    fn exile_from_hand_for_cost_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Blue Pitch Card".to_string(),
            Zone::Hand,
        );
        let pending = dummy_pending_cast(ObjectId(50), CardId(99), PlayerId(1));
        state.waiting_for = WaitingFor::ExileForCost {
            player: PlayerId(1),
            zone: ExileCostSourceZone::Hand,
            count: 1,
            cards: vec![card_id],
            pending_cast: pending,
        };

        // Caster sees the real ID.
        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        match filtered_self.waiting_for {
            WaitingFor::ExileForCost {
                cards,
                zone,
                count,
                player,
                ..
            } => {
                assert_eq!(zone, ExileCostSourceZone::Hand);
                assert_eq!(cards, vec![card_id]);
                assert_eq!(count, 1);
                assert_eq!(player, PlayerId(1));
            }
            other => panic!("expected ExileForCost, got {other:?}"),
        }

        // Opponent sees a placeholder, but `count` and `pending_cast` survive.
        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::ExileForCost {
                cards,
                zone,
                count,
                player,
                pending_cast,
            } => {
                assert_eq!(zone, ExileCostSourceZone::Hand);
                assert_eq!(cards, vec![ObjectId(0)]);
                assert_eq!(count, 1);
                assert_eq!(player, PlayerId(1));
                assert_eq!(pending_cast.object_id, ObjectId(50));
            }
            other => panic!("expected ExileForCost, got {other:?}"),
        }
    }

    #[test]
    fn drawn_this_turn_choice_private_tracking_is_hidden_from_non_controller() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Drawn Secret".to_string(),
            Zone::Hand,
        );
        state
            .cards_drawn_this_turn
            .insert(PlayerId(1), vec![card_id]);
        state.waiting_for = WaitingFor::DrawnThisTurnTopdeckChoice {
            player: PlayerId(1),
            cards: vec![card_id],
            count: 1,
            min_count: 0,
            life_payment: 4,
            source_id: ObjectId(99),
        };

        let filtered_self = filter_state_for_viewer(&state, PlayerId(1));
        assert_eq!(
            filtered_self.cards_drawn_this_turn.get(&PlayerId(1)),
            Some(&vec![card_id])
        );
        match filtered_self.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. } => {
                assert_eq!(cards, vec![card_id]);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        assert!(
            !filtered_opp
                .cards_drawn_this_turn
                .contains_key(&PlayerId(1)),
            "opponents must not learn which hidden hand cards were drawn this turn"
        );
        match filtered_opp.waiting_for {
            WaitingFor::DrawnThisTurnTopdeckChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0)]);
            }
            other => panic!("expected DrawnThisTurnTopdeckChoice, got {other:?}"),
        }
    }

    /// CR 400.2: Graveyard is a public zone. The escape eligibility list
    /// (`ExileForCost { zone: Graveyard, .. }`) must NOT be redacted for
    /// non-controller viewers.
    #[test]
    fn exile_for_cost_graveyard_is_not_redacted() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Escape Filler".to_string(),
            Zone::Graveyard,
        );
        let pending = dummy_pending_cast(ObjectId(50), CardId(99), PlayerId(1));
        state.waiting_for = WaitingFor::ExileForCost {
            player: PlayerId(1),
            zone: ExileCostSourceZone::Graveyard,
            count: 1,
            cards: vec![card_id],
            pending_cast: pending,
        };

        let filtered_opp = filter_state_for_viewer(&state, PlayerId(2));
        match filtered_opp.waiting_for {
            WaitingFor::ExileForCost { zone, cards, .. } => {
                assert_eq!(zone, ExileCostSourceZone::Graveyard);
                assert_eq!(
                    cards,
                    vec![card_id],
                    "graveyard variant must NOT be redacted"
                );
            }
            other => panic!("expected ExileForCost, got {other:?}"),
        }
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

    /// CR 903.10a: commander damage is public game state — every viewer
    /// (the dealing player, the receiving player, and every spectator) must
    /// see how much damage each commander has dealt to each player. The
    /// visibility filter must therefore preserve `commander_damage` verbatim
    /// for every viewer, and `derive_views` must populate the
    /// per-victim grouping irrespective of who is viewing.
    #[test]
    fn commander_damage_is_visible_to_every_viewer() {
        use crate::game::derived_views::derive_views;
        use crate::types::game_state::CommanderDamageEntry;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        let cmd = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Public Commander".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&cmd).unwrap().is_commander = true;
        state.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(1),
            commander: cmd,
            damage: 7,
        });

        for viewer in [PlayerId(0), PlayerId(1)] {
            let filtered = filter_state_for_viewer(&state, viewer);
            assert_eq!(
                filtered.commander_damage.len(),
                1,
                "viewer {viewer:?} must see the commander-damage entry",
            );
            let views = derive_views(&filtered);
            let from_p0 = views
                .commander_damage_by_attacker
                .get(&PlayerId(0))
                .unwrap_or_else(|| {
                    panic!("viewer {viewer:?} must see P0's attacker entry");
                });
            assert_eq!(from_p0.len(), 1);
            assert_eq!(from_p0[0].victim, PlayerId(1));
            assert_eq!(from_p0[0].damage, 7);
            assert_eq!(from_p0[0].commander, cmd);
        }
    }

    #[test]
    fn foretold_exile_card_identity_visible_only_to_owner() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let card_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Foretold Test".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&card_id).unwrap();
            obj.foretold = true;
            obj.face_down = true;
        }

        let owner_view = filter_state_for_viewer(&state, PlayerId(0));
        let owner_obj = owner_view.objects.get(&card_id).unwrap();
        assert_eq!(owner_obj.name, "Foretold Test");
        assert!(owner_obj.foretold);
        assert!(owner_obj.face_down);

        let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
        let opponent_obj = opponent_view.objects.get(&card_id).unwrap();
        assert_eq!(opponent_obj.name, "Hidden Card");
        assert!(!opponent_obj.foretold);
        assert!(opponent_obj.face_down);
        assert!(opponent_obj.casting_permissions.is_empty());
    }
}
