use engine::game::filter_events_for_viewer;
use engine::game::filter_state_for_viewer;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Returns a filtered copy of the game state for the given player.
/// Hides ALL opponents' hand contents and ALL players' library contents.
pub fn filter_state_for_player(state: &GameState, viewer: PlayerId) -> GameState {
    filter_state_for_viewer(state, viewer)
}

/// Returns viewer-safe game events for wire broadcast (library draws, etc.).
pub fn filter_events_for_player(
    events: &[GameEvent],
    state: &GameState,
    viewer: PlayerId,
) -> Vec<GameEvent> {
    filter_events_for_viewer(events, state, viewer)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::deck_loading::DeckEntry;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::{ActiveLibrarySearch, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;
    use proptest::prelude::*;

    fn setup_state() -> GameState {
        let mut state = GameState::new_two_player(42);

        // Add cards to player 0's hand
        let id0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id0).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        )]);

        // Add cards to player 1's hand
        let id1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id1).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
        )]);

        // Add cards to libraries
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Island".to_string(),
            Zone::Library,
        );

        state
    }

    #[test]
    fn own_hand_is_fully_visible() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        let hand = &filtered.players[0].hand;
        assert_eq!(hand.len(), 1);
        let obj = filtered.objects.get(&hand[0]).unwrap();
        assert_eq!(obj.name, "Lightning Bolt");
        assert!(!obj.face_down);
    }

    #[test]
    fn opponent_hand_cards_are_hidden() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        let opp_hand = &filtered.players[1].hand;
        assert_eq!(opp_hand.len(), 1, "hand size preserved");
        let obj = filtered.objects.get(&opp_hand[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");
        assert!(obj.face_down);
        assert!(obj.abilities.is_empty());
    }

    #[test]
    fn non_seat_spectator_sees_no_player_hands() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(u8::MAX));

        for player in &filtered.players {
            let hand = &player.hand;
            assert_eq!(hand.len(), 1, "hand size remains public");
            let obj = filtered.objects.get(&hand[0]).unwrap();
            assert_eq!(obj.name, "Hidden Card");
            assert!(obj.face_down);
            assert!(obj.abilities.is_empty());
        }
    }

    #[test]
    fn library_contents_hidden_for_both() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        // Own library hidden
        let own_lib = &filtered.players[0].library;
        assert_eq!(own_lib.len(), 1);
        let obj = filtered.objects.get(&own_lib[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");

        // Opponent library hidden
        let opp_lib = &filtered.players[1].library;
        assert_eq!(opp_lib.len(), 1);
        let obj = filtered.objects.get(&opp_lib[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");
    }

    #[test]
    fn filter_preserves_hand_size() {
        let state = setup_state();
        let original_opp_hand_size = state.players[1].hand.len();
        let filtered = filter_state_for_player(&state, PlayerId(0));
        assert_eq!(filtered.players[1].hand.len(), original_opp_hand_size);
    }

    #[test]
    fn revealed_cards_remain_visible_in_opponent_hand() {
        let mut state = setup_state();
        let opp_hand = &state.players[1].hand;
        let revealed_id = opp_hand[0];

        // Mark the card as revealed
        state.revealed_cards.insert(revealed_id);

        let filtered = filter_state_for_player(&state, PlayerId(0));

        let obj = filtered.objects.get(&revealed_id).unwrap();
        assert_ne!(
            obj.name, "Hidden Card",
            "Revealed card should not be hidden"
        );
        assert!(!obj.face_down, "Revealed card should not be face_down");
    }

    #[test]
    fn redacts_opponent_deck_pool_details() {
        let mut state = setup_state();
        let entry = DeckEntry {
            card: CardFace {
                name: "Forest".to_string(),
                mana_cost: ManaCost::NoCost,
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![engine::types::card_type::CoreType::Land],
                    subtypes: vec!["Forest".to_string()],
                },
                power: None,
                toughness: None,
                loyalty: None,
                defense: None,
                oracle_text: None,
                non_ability_text: None,
                flavor_name: None,
                keywords: vec![],
                abilities: vec![],
                triggers: vec![],
                static_abilities: vec![],
                replacements: vec![],
                cleave_variant: None,
                color_override: None,
                color_identity: vec![],
                scryfall_oracle_id: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                solve_condition: None,
                parse_warnings: vec![],
                brawl_commander: false,
                is_commander: false,
                is_oathbreaker: false,
                deck_copy_limit: None,
                metadata: Default::default(),
                rarities: Default::default(),
                attraction_lights: vec![],
            },
            count: 4,
        };
        state.deck_pools = vec![
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(0),
                registered_main: Arc::new(vec![entry.clone()]),
                registered_sideboard: Arc::new(vec![entry.clone()]),
                current_main: Arc::new(vec![entry.clone()]),
                current_sideboard: Arc::new(vec![entry.clone()]),
                ..Default::default()
            },
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(1),
                registered_main: Arc::new(vec![entry.clone()]),
                registered_sideboard: Arc::new(vec![entry.clone()]),
                current_main: Arc::new(vec![entry.clone()]),
                current_sideboard: Arc::new(vec![entry]),
                ..Default::default()
            },
        ];

        let filtered = filter_state_for_player(&state, PlayerId(0));
        let own = filtered
            .deck_pools
            .iter()
            .find(|pool| pool.player == PlayerId(0))
            .unwrap();
        let opp = filtered
            .deck_pools
            .iter()
            .find(|pool| pool.player == PlayerId(1))
            .unwrap();
        assert!(!own.registered_main.is_empty());
        assert!(opp.registered_main.is_empty());
        assert!(opp.registered_sideboard.is_empty());
        assert!(opp.current_main.is_empty());
        assert!(opp.current_sideboard.is_empty());
    }

    #[test]
    fn manifest_dread_hides_card_ids_from_opponent() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        // Add 2 cards to library
        let card_a = create_object(
            &mut state,
            CardId(10),
            p0,
            "Creature A".to_string(),
            Zone::Library,
        );
        let card_b = create_object(
            &mut state,
            CardId(11),
            p0,
            "Creature B".to_string(),
            Zone::Library,
        );

        // Set up ManifestDreadChoice state
        state.waiting_for = WaitingFor::ManifestDreadChoice {
            player: p0,
            cards: vec![card_a, card_b],
            source_id: ObjectId(99),
        };
        state.revealed_cards.insert(card_a);
        state.revealed_cards.insert(card_b);

        // Player 0 (manifesting player) should see the cards
        let filtered_p0 = filter_state_for_player(&state, p0);
        match &filtered_p0.waiting_for {
            WaitingFor::ManifestDreadChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                assert_eq!(cards[0], card_a);
                assert_eq!(cards[1], card_b);
            }
            other => panic!("Expected ManifestDreadChoice, got {:?}", other),
        }
        // Cards should not be hidden for the manifesting player
        let obj_a = &filtered_p0.objects[&card_a];
        assert_eq!(obj_a.name, "Creature A");

        // Player 1 (opponent) should see redacted card IDs
        let filtered_p1 = filter_state_for_player(&state, PlayerId(1));
        match &filtered_p1.waiting_for {
            WaitingFor::ManifestDreadChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                // Card IDs should be zeroed out for opponents
                assert_eq!(cards[0], engine::types::identifiers::ObjectId(0));
                assert_eq!(cards[1], engine::types::identifiers::ObjectId(0));
            }
            other => panic!("Expected ManifestDreadChoice, got {:?}", other),
        }
        // Library cards should be hidden for opponent
        let obj_a_opp = &filtered_p1.objects[&card_a];
        assert_eq!(obj_a_opp.name, "Hidden Card");
    }

    #[test]
    fn effect_zone_choice_from_hand_redacts_cards_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let card_a = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        let card_b = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Island".to_string(),
            Zone::Hand,
        );

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![card_a, card_b],
            count: 1,
            min_count: 0,
            up_to: true,
            source_id: ObjectId(100),
            effect_kind: engine::types::ability::EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            is_cost_payment: false,
            library_position: None,
            enters_modified_if: None,
        };

        let filtered = filter_state_for_player(&state, PlayerId(1));

        match filtered.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0), ObjectId(0)]);
            }
            other => panic!("Expected EffectZoneChoice, got {:?}", other),
        }

        assert_eq!(filtered.objects[&card_a].name, "Hidden Card");
        assert_eq!(filtered.objects[&card_b].name, "Hidden Card");
    }

    /// CR 603.3b: `WaitingFor::OrderTriggers` carries only public information
    /// (triggers on their way to the shared stack). The variant must survive
    /// `filter_state_for_player` unchanged for both the prompted player and
    /// the opponent — no hiding, no redaction. The parallel
    /// `pending_trigger_order` state, by contrast, IS per-controller redacted:
    /// its placement spine (groups, controllers, ordered flags, group sizes)
    /// stays visible to everyone, but each group's private trigger payload
    /// (firing event, modal choice, distribution, mode descriptions) is
    /// stripped for viewers who don't control that group. That redaction is
    /// covered by `pending_trigger_order_redacts_private_payload_for_opponent`
    /// and `pending_trigger_order_redacts_per_group_in_multi_group_order`
    /// below.
    #[test]
    fn order_triggers_waiting_for_passes_through_to_both_players() {
        use engine::types::game_state::PendingTriggerSummary;

        let mut state = GameState::new_two_player(42);
        let summaries = vec![
            PendingTriggerSummary {
                source_id: ObjectId(11),
                source_name: "Wedding Announcement".to_string(),
                description: "At the beginning of your end step, create a token.".to_string(),
            },
            PendingTriggerSummary {
                source_id: ObjectId(12),
                source_name: "Ocelot Pride".to_string(),
                description: "At the beginning of your end step, if you gained life...".to_string(),
            },
        ];
        state.waiting_for = WaitingFor::OrderTriggers {
            player: PlayerId(0),
            triggers: summaries.clone(),
        };

        for viewer in [PlayerId(0), PlayerId(1)] {
            let filtered = filter_state_for_player(&state, viewer);
            match filtered.waiting_for {
                WaitingFor::OrderTriggers { player, triggers } => {
                    assert_eq!(player, PlayerId(0));
                    assert_eq!(triggers, summaries);
                }
                other => panic!(
                    "OrderTriggers must survive filtering for viewer {viewer:?}: got {other:?}"
                ),
            }
        }
    }

    /// Build a minimal `PendingTriggerContext` whose private fields are all
    /// populated, so a viewer-side redaction can be verified by checking that
    /// each private field is cleared/`None` while public scheduling metadata is
    /// preserved.
    fn make_pending_ctx_with_private_payload(
        controller: PlayerId,
        source_id: ObjectId,
        description: &str,
    ) -> engine::game::triggers::PendingTriggerContext {
        use engine::game::triggers::{
            PendingTrigger, PendingTriggerContext, PendingTriggerDispatchOrigin,
        };
        use engine::types::ability::{ModalChoice, PlayerFilter, ResolvedAbility};
        use engine::types::events::GameEvent;

        let event = GameEvent::GameStarted;
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            Vec::new(),
            source_id,
            controller,
        );
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["mode A".into(), "mode B".into()],
            allow_repeat_modes: false,
            constraints: Vec::new(),
            mode_costs: Vec::new(),
            mode_pawprints: Vec::new(),
            entwine_cost: None,
            chooser: PlayerFilter::Controller,
            selection: engine::types::ability::TargetSelectionMode::Chosen,
            dynamic_max_choices: None,
        };
        let pending = PendingTrigger {
            source_id,
            controller,
            condition: None,
            ability,
            timestamp: 0,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(event.clone()),
            modal: Some(modal),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                    countered_spell_zone: None,
                },
            )],
            description: Some(description.to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        };
        PendingTriggerContext {
            pending,
            trigger_events: vec![event],
            dispatch_origin: PendingTriggerDispatchOrigin::Normal,
        }
    }

    /// CR 603.3b + CR 400.2: A single-group `pending_trigger_order` (one
    /// controller, one trigger with all private fields populated) must show
    /// the full payload to the group's controller and a redacted payload —
    /// `trigger_event`/`modal`/`distribute`/`description` cleared to `None`,
    /// `mode_abilities` and `trigger_events` emptied — to the opponent. The
    /// public spine (controller, source_id, timestamp, `ordered` flag) is
    /// preserved for both viewers so the opponent's frontend can still
    /// render an "opponent is ordering N triggers" indicator.
    #[test]
    fn pending_trigger_order_redacts_private_payload_for_opponent() {
        use engine::types::game_state::{PendingTriggerOrder, TriggerOrderGroup};

        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let source_id = ObjectId(42);
        let ctx = make_pending_ctx_with_private_payload(
            controller,
            source_id,
            "private mode description",
        );
        state.pending_trigger_order = Some(PendingTriggerOrder {
            groups: vec![TriggerOrderGroup {
                controller,
                triggers: vec![ctx],
                ordered: false,
            }],
            resume_after_ordering: None,
        });

        // Controller sees the full payload.
        let owner_view = filter_state_for_player(&state, controller);
        let owner_order = owner_view
            .pending_trigger_order
            .as_ref()
            .expect("controller must still see pending_trigger_order");
        assert_eq!(owner_order.groups.len(), 1);
        let owner_group = &owner_order.groups[0];
        assert_eq!(owner_group.controller, controller);
        assert!(!owner_group.ordered);
        assert_eq!(owner_group.triggers.len(), 1);
        let owner_ctx = &owner_group.triggers[0];
        assert_eq!(owner_ctx.pending.source_id, source_id);
        assert!(owner_ctx.pending.trigger_event.is_some());
        assert!(owner_ctx.pending.modal.is_some());
        assert_eq!(
            owner_ctx.pending.description.as_deref(),
            Some("private mode description")
        );
        assert_eq!(owner_ctx.pending.mode_abilities.len(), 1);
        assert_eq!(owner_ctx.trigger_events.len(), 1);

        // Opponent sees the spine but no private payload.
        let opp_view = filter_state_for_player(&state, PlayerId(1));
        let opp_order = opp_view
            .pending_trigger_order
            .as_ref()
            .expect("opponent must still see pending_trigger_order spine");
        assert_eq!(opp_order.groups.len(), 1);
        let opp_group = &opp_order.groups[0];
        assert_eq!(opp_group.controller, controller);
        assert!(!opp_group.ordered);
        assert_eq!(opp_group.triggers.len(), 1);
        let opp_ctx = &opp_group.triggers[0];
        // Public spine preserved.
        assert_eq!(opp_ctx.pending.source_id, source_id);
        assert_eq!(opp_ctx.pending.controller, controller);
        assert_eq!(opp_ctx.pending.timestamp, 0);
        assert_eq!(
            opp_ctx.dispatch_origin,
            engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        );
        // Private payload redacted.
        assert!(opp_ctx.pending.trigger_event.is_none());
        assert!(opp_ctx.pending.modal.is_none());
        assert!(opp_ctx.pending.distribute.is_none());
        assert!(opp_ctx.pending.description.is_none());
        assert!(opp_ctx.pending.mode_abilities.is_empty());
        assert!(opp_ctx.trigger_events.is_empty());
    }

    /// CR 603.3b: A two-group `pending_trigger_order` (one group per player)
    /// must redact each group's private payload independently for the viewer
    /// who does NOT control that group. Each viewer sees their own group's
    /// full payload and the other group's spine only.
    #[test]
    fn pending_trigger_order_redacts_per_group_in_multi_group_order() {
        use engine::types::game_state::{PendingTriggerOrder, TriggerOrderGroup};

        let mut state = GameState::new_two_player(42);
        let ctx0 = make_pending_ctx_with_private_payload(
            PlayerId(0),
            ObjectId(101),
            "p0 private description",
        );
        let ctx1 = make_pending_ctx_with_private_payload(
            PlayerId(1),
            ObjectId(202),
            "p1 private description",
        );
        state.pending_trigger_order = Some(PendingTriggerOrder {
            groups: vec![
                TriggerOrderGroup {
                    controller: PlayerId(0),
                    triggers: vec![ctx0],
                    ordered: false,
                },
                TriggerOrderGroup {
                    controller: PlayerId(1),
                    triggers: vec![ctx1],
                    ordered: false,
                },
            ],
            resume_after_ordering: None,
        });

        // PlayerId(0) view: own group full, opponent group redacted.
        let p0_view = filter_state_for_player(&state, PlayerId(0));
        let p0_order = p0_view
            .pending_trigger_order
            .as_ref()
            .expect("pending_trigger_order must still be present");
        assert_eq!(p0_order.groups.len(), 2);
        let p0_own = &p0_order.groups[0];
        assert_eq!(p0_own.controller, PlayerId(0));
        assert_eq!(p0_own.triggers.len(), 1);
        let p0_own_ctx = &p0_own.triggers[0];
        assert!(p0_own_ctx.pending.trigger_event.is_some());
        assert!(p0_own_ctx.pending.modal.is_some());
        assert_eq!(
            p0_own_ctx.pending.description.as_deref(),
            Some("p0 private description")
        );
        assert_eq!(p0_own_ctx.trigger_events.len(), 1);

        let p0_opp = &p0_order.groups[1];
        assert_eq!(p0_opp.controller, PlayerId(1));
        assert_eq!(p0_opp.triggers.len(), 1);
        let p0_opp_ctx = &p0_opp.triggers[0];
        assert_eq!(p0_opp_ctx.pending.source_id, ObjectId(202));
        assert_eq!(
            p0_opp_ctx.dispatch_origin,
            engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        );
        assert!(p0_opp_ctx.pending.trigger_event.is_none());
        assert!(p0_opp_ctx.pending.modal.is_none());
        assert!(p0_opp_ctx.pending.description.is_none());
        assert!(p0_opp_ctx.pending.mode_abilities.is_empty());
        assert!(p0_opp_ctx.trigger_events.is_empty());

        // PlayerId(1) view: own group full, opponent group redacted.
        let p1_view = filter_state_for_player(&state, PlayerId(1));
        let p1_order = p1_view
            .pending_trigger_order
            .as_ref()
            .expect("pending_trigger_order must still be present");
        assert_eq!(p1_order.groups.len(), 2);
        let p1_opp = &p1_order.groups[0];
        assert_eq!(p1_opp.controller, PlayerId(0));
        let p1_opp_ctx = &p1_opp.triggers[0];
        assert_eq!(p1_opp_ctx.pending.source_id, ObjectId(101));
        assert_eq!(
            p1_opp_ctx.dispatch_origin,
            engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        );
        assert!(p1_opp_ctx.pending.trigger_event.is_none());
        assert!(p1_opp_ctx.pending.modal.is_none());
        assert!(p1_opp_ctx.pending.description.is_none());
        assert!(p1_opp_ctx.pending.mode_abilities.is_empty());
        assert!(p1_opp_ctx.trigger_events.is_empty());

        let p1_own = &p1_order.groups[1];
        assert_eq!(p1_own.controller, PlayerId(1));
        let p1_own_ctx = &p1_own.triggers[0];
        assert!(p1_own_ctx.pending.trigger_event.is_some());
        assert!(p1_own_ctx.pending.modal.is_some());
        assert_eq!(
            p1_own_ctx.pending.description.as_deref(),
            Some("p1 private description")
        );
        assert_eq!(p1_own_ctx.trigger_events.len(), 1);
    }

    /// CR 603.3b + CR 400.2: The singleton `pending_trigger` and its sidecar
    /// `pending_trigger_event_batch` carry the same private payload shape as
    /// the entries inside `pending_trigger_order`. Opponents must see only
    /// the public spine; the controller still sees the full payload.
    #[test]
    fn pending_trigger_and_event_batch_redact_for_opponent() {
        use engine::types::events::GameEvent;

        let controller = PlayerId(0);
        let source_id = ObjectId(303);
        let ctx = make_pending_ctx_with_private_payload(
            controller,
            source_id,
            "pending trigger private description",
        );

        let mut state = GameState::new_two_player(42);
        state.pending_trigger = Some(ctx.pending.clone());
        state.pending_trigger_event_batch = vec![GameEvent::GameStarted];

        // Controller view: payload intact, batch intact.
        let owner_view = filter_state_for_player(&state, controller);
        let owner_pending = owner_view
            .pending_trigger
            .as_ref()
            .expect("controller must still see pending_trigger");
        assert_eq!(owner_pending.source_id, source_id);
        assert!(owner_pending.trigger_event.is_some());
        assert!(owner_pending.modal.is_some());
        assert_eq!(
            owner_pending.description.as_deref(),
            Some("pending trigger private description")
        );
        assert_eq!(owner_pending.mode_abilities.len(), 1);
        assert_eq!(owner_view.pending_trigger_event_batch.len(), 1);

        // Opponent view: spine preserved, payload + sidecar cleared.
        let opp_view = filter_state_for_player(&state, PlayerId(1));
        let opp_pending = opp_view
            .pending_trigger
            .as_ref()
            .expect("opponent must still see pending_trigger spine");
        assert_eq!(opp_pending.source_id, source_id);
        assert_eq!(opp_pending.controller, controller);
        assert!(opp_pending.trigger_event.is_none());
        assert!(opp_pending.modal.is_none());
        assert!(opp_pending.distribute.is_none());
        assert!(opp_pending.description.is_none());
        assert!(opp_pending.mode_abilities.is_empty());
        assert!(opp_view.pending_trigger_event_batch.is_empty());
    }

    /// CR 113.2c + CR 603.2 + CR 603.3b: `deferred_triggers` is a FIFO queue
    /// of `PendingTriggerContext`s waiting on the active `pending_trigger` to
    /// resolve. Redaction must be per-entry — a viewer who controls one entry
    /// but not another sees only the controlled one's private payload.
    #[test]
    fn deferred_triggers_redact_per_entry_for_opponent() {
        let ctx0 = make_pending_ctx_with_private_payload(
            PlayerId(0),
            ObjectId(401),
            "p0 deferred description",
        );
        let ctx1 = make_pending_ctx_with_private_payload(
            PlayerId(1),
            ObjectId(402),
            "p1 deferred description",
        );

        let mut state = GameState::new_two_player(42);
        state.deferred_triggers = vec![ctx0, ctx1];

        let p0_view = filter_state_for_player(&state, PlayerId(0));
        assert_eq!(p0_view.deferred_triggers.len(), 2);
        let p0_own = &p0_view.deferred_triggers[0];
        assert!(p0_own.pending.trigger_event.is_some());
        assert!(p0_own.pending.modal.is_some());
        assert_eq!(
            p0_own.pending.description.as_deref(),
            Some("p0 deferred description")
        );
        let p0_opp = &p0_view.deferred_triggers[1];
        assert_eq!(p0_opp.pending.source_id, ObjectId(402));
        assert_eq!(p0_opp.pending.controller, PlayerId(1));
        assert_eq!(
            p0_opp.dispatch_origin,
            engine::game::triggers::PendingTriggerDispatchOrigin::Normal
        );
        assert!(p0_opp.pending.trigger_event.is_none());
        assert!(p0_opp.pending.modal.is_none());
        assert!(p0_opp.pending.description.is_none());
        assert!(p0_opp.pending.mode_abilities.is_empty());
        assert!(p0_opp.trigger_events.is_empty());

        let p1_view = filter_state_for_player(&state, PlayerId(1));
        let p1_opp = &p1_view.deferred_triggers[0];
        assert_eq!(p1_opp.pending.controller, PlayerId(0));
        assert!(p1_opp.pending.trigger_event.is_none());
        assert!(p1_opp.pending.modal.is_none());
        assert!(p1_opp.pending.description.is_none());
        let p1_own = &p1_view.deferred_triggers[1];
        assert!(p1_own.pending.trigger_event.is_some());
        assert!(p1_own.pending.modal.is_some());
        assert_eq!(
            p1_own.pending.description.as_deref(),
            Some("p1 deferred description")
        );
    }

    #[test]
    fn wrapper_preserves_only_viewer_entitled_search_records_and_events() {
        let mut state = setup_state();
        let p0_library = state.players[0].library[0];
        let p1_library = state.players[1].library[0];
        for (searcher, owner, object_id, audience) in [
            (PlayerId(0), PlayerId(0), p0_library, vec![PlayerId(0)]),
            (PlayerId(1), PlayerId(1), p1_library, vec![PlayerId(1)]),
        ] {
            let identity = ObjectIncarnationRef::from_object(&state.objects[&object_id]);
            state.active_library_searches.insert(
                ActiveLibrarySearch::try_new(
                    searcher,
                    owner,
                    Some(owner),
                    audience,
                    vec![(owner, Zone::Library, identity)],
                )
                .unwrap(),
            );
        }
        let events = vec![
            GameEvent::HiddenSearchViewed {
                searcher: PlayerId(0),
                cards: Vec::new(),
                audience: vec![PlayerId(0)],
            },
            GameEvent::HiddenSearchViewed {
                searcher: PlayerId(1),
                cards: Vec::new(),
                audience: vec![PlayerId(1)],
            },
        ];

        let filtered = filter_state_for_player(&state, PlayerId(0));
        assert!(filtered.active_library_searches.get(&PlayerId(0)).is_some());
        assert!(filtered.active_library_searches.get(&PlayerId(1)).is_none());
        let filtered_events = filter_events_for_player(&events, &state, PlayerId(0));
        assert_eq!(filtered_events.len(), 1);
        assert!(matches!(
            filtered_events[0],
            GameEvent::HiddenSearchViewed {
                searcher: PlayerId(0),
                ..
            }
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 16,
            .. ProptestConfig::default()
        })]

        #[test]
        fn property_filter_hides_opponent_hidden_zones(
            opp_hand_count in 1usize..5,
            own_library_count in 1usize..5,
            opp_library_count in 1usize..5,
        ) {
            let mut state = GameState::new_two_player(42);

            for idx in 0..opp_hand_count {
                create_object(
                    &mut state,
                    CardId((100 + idx) as u64),
                    PlayerId(1),
                    format!("Opp Hand {idx}"),
                    Zone::Hand,
                );
            }

            for idx in 0..own_library_count {
                create_object(
                    &mut state,
                    CardId((200 + idx) as u64),
                    PlayerId(0),
                    format!("Own Library {idx}"),
                    Zone::Library,
                );
            }

            for idx in 0..opp_library_count {
                create_object(
                    &mut state,
                    CardId((300 + idx) as u64),
                    PlayerId(1),
                    format!("Opp Library {idx}"),
                    Zone::Library,
                );
            }

            let filtered = filter_state_for_player(&state, PlayerId(0));

            prop_assert_eq!(filtered.players[1].hand.len(), opp_hand_count);
            for obj_id in &filtered.players[1].hand {
                let obj = filtered.objects.get(obj_id).expect("hand object must exist");
                prop_assert!(obj.face_down);
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.abilities.is_empty());
            }

            prop_assert_eq!(filtered.players[0].library.len(), own_library_count);
            for obj_id in &filtered.players[0].library {
                let obj = filtered.objects.get(obj_id).expect("own library object must exist");
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.face_down);
            }

            prop_assert_eq!(filtered.players[1].library.len(), opp_library_count);
            for obj_id in &filtered.players[1].library {
                let obj = filtered.objects.get(obj_id).expect("opponent library object must exist");
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.face_down);
            }
        }
    }
}
