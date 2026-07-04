use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.68a: Blight N as an effect.
///
/// "To 'blight N' means to put N -1/-1 counters on a creature you control."
/// Blight is a keyword action that has the controller *choose* (not *target*)
/// a creature they control; hexproof and shroud do not prevent it (CR 701.68a).
/// The choice is made at resolution time via `WaitingFor::EffectZoneChoice`,
/// and the counters are placed by the `EffectKind::BlightEffect` handler arm in
/// `engine_resolution_choices.rs`.
///
/// CR 701.68b: If the controller controls no creatures, blight is a no-op.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, blighting_player) = match &ability.effect {
        Effect::BlightEffect { count, player } => {
            // CR 701.68a: the blighting player puts the counters on a creature
            // *they* control. Default `TargetFilter::Controller` keeps "you
            // blight"; Champion of the Weird redirects to the targeted opponent
            // ("Target opponent blights 2").
            let blighting_player =
                crate::game::effects::resolve_player_for_context_ref(state, ability, player);
            (*count, blighting_player)
        }
        _ => return Ok(()),
    };

    let source_id = ability.source_id;

    // CR 701.68a: Eligible creatures are those the blighting player controls on
    // the battlefield.
    let eligible: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == blighting_player
                    && !obj.is_emblem
                    && obj.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .collect();

    // CR 701.68b: If a player is given the choice to blight but is unable to
    // (controls no creatures), the effect does nothing.
    if eligible.is_empty() {
        state.last_effect_count = Some(0);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::BlightEffect,
            source_id,
        });
        return Ok(());
    }

    // CR 701.68a: The blighting player chooses exactly one creature they control.
    // `count` (in EffectZoneChoice) is the number of creatures chosen (1);
    // `count_param` carries N — the number of -1/-1 counters to place.
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: blighting_player,
        cards: eligible,
        count: 1,
        min_count: 1,
        up_to: false,
        source_id,
        effect_kind: EffectKind::BlightEffect,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        // CR 708.2a: Blight places -1/-1 counters; no face-down entry.
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: count,
        library_position: None,
        is_cost_payment: false,
        enters_modified_if: None,
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply;
    use crate::game::zones;
    use crate::types::ability::TargetFilter;
    use crate::types::actions::GameAction;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn make_creature(state: &mut GameState, card_id: u64, controller: PlayerId) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(card_id),
            controller,
            format!("Creature {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        id
    }

    fn blight_ability(source_id: ObjectId, controller: PlayerId, count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::BlightEffect {
                count,
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        )
    }

    /// Discriminator 1: Blight N places exactly N -1/-1 counters on the chosen
    /// creature. With the dispatch routed back to the no-op, this asserts 0
    /// counters land — proving the fix is reverted-fix-discriminating.
    #[test]
    fn blight_n_places_n_counters() {
        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));
        let ability = blight_ability(creature, PlayerId(0), 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Controller must choose the creature via EffectZoneChoice.
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count_param,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards, &[creature]);
                assert_eq!(*count_param, 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        // Drive the choice through the real handler.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(2),
            "blight 2 must place 2 -1/-1 counters"
        );
    }

    /// Discriminator 2: CR 701.68b — controller controls no creatures, blight is
    /// a no-op (no WaitingFor, last_effect_count == Some(0), EffectResolved
    /// emitted, no panic).
    #[test]
    fn blight_no_creatures_is_noop() {
        let mut state = GameState::new_two_player(7);
        // A non-creature so the source id is valid.
        let source = zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        let ability = blight_ability(source, PlayerId(0), 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "no eligible creatures must not prompt a choice"
        );
        assert_eq!(state.last_effect_count, Some(0));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::BlightEffect,
                ..
            }
        )));
    }

    /// Discriminator 3: Controller-scoped eligibility — the choice pool contains
    /// only the controller's creature, never the opponent's.
    #[test]
    fn blight_eligibility_is_controller_scoped() {
        let mut state = GameState::new_two_player(7);
        let mine = make_creature(&mut state, 1, PlayerId(0));
        let _theirs = make_creature(&mut state, 2, PlayerId(1));
        let ability = blight_ability(mine, PlayerId(0), 1);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(cards, &[mine], "only the controller's creature is eligible");
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    /// Discriminator 4: Non-targeted discriminator — the controller's eligible
    /// creature has hexproof; blight still succeeds (CR 701.68a — blight is not
    /// a targeting choice, so hexproof is irrelevant).
    #[test]
    fn blight_succeeds_against_hexproof_creature() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let ability = blight_ability(creature, PlayerId(0), 1);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(1),
            "blight ignores hexproof — it is a choice, not a target"
        );
        // CR 701.68a: a default-controller blight surfaces NO real target slot.
        // `target_filter()` returns `Some(Controller)` (mirroring the other
        // player-axis effects), but `extract_target_filter_from_effect` drops the
        // context ref via its final `is_context_ref()` guard, so the controller's
        // creature choice is never declared as a target (hexproof is irrelevant).
        assert!(
            crate::game::triggers::extract_target_filter_from_effect(&Effect::BlightEffect {
                count: 1,
                player: TargetFilter::Controller,
            })
            .is_none(),
            "default 'you blight' must surface no target slot"
        );
        let _ = TargetFilter::Any; // keep import used across cfg
    }

    /// Discriminator 5: CR 614.1 replacement-aware placement — a counter-doubling
    /// replacement is active; blight 1 results in 2 -1/-1 counters, confirming
    /// the handler routes through `add_counter_with_replacement`.
    #[test]
    fn blight_is_replacement_aware() {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(7);
        let creature = make_creature(&mut state, 1, PlayerId(0));

        // CR 614.1a: counter-doubling replacement effect (Doubling Season-class
        // "those counters" wording — counter_match left None).
        let doubler = zones::create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        state
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions = vec![repl].into();

        let ability = blight_ability(creature, PlayerId(0), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        let obj = state.objects.get(&creature).unwrap();
        assert_eq!(
            obj.counters.get(&CounterType::Minus1Minus1).copied(),
            Some(2),
            "counter-doubling replacement (CR 614.1a) must double blight's counters"
        );
    }

    /// Discriminator 6 (player-redirect): CR 701.68a — "Target opponent blights
    /// N" (Champion of the Weird) makes the *targeted player*, not the
    /// controller, blight. The prompt goes to the opponent and only the
    /// opponent's creatures are eligible. Reverting the `player` plumbing (back
    /// to `ability.controller`) flips both assertions: the prompt would address
    /// PlayerId(0) and scope eligibility to PlayerId(0)'s creature.
    #[test]
    fn blight_redirects_to_targeted_opponent() {
        use crate::types::ability::{ControllerRef, TargetRef, TypedFilter};

        let mut state = GameState::new_two_player(7);
        let controller = PlayerId(0);
        let opponent = PlayerId(1);
        // Controller has a creature too — it must NOT be eligible when the
        // opponent is the blighting player.
        let mine = make_creature(&mut state, 1, controller);
        let theirs = make_creature(&mut state, 2, opponent);

        // Mirror the exact parser output for "Target opponent blights 2": an
        // Opponent player filter carried in `player` (a non-context-ref target),
        // with the chosen opponent in `targets` (declared at announcement).
        let ability = ResolvedAbility::new(
            Effect::BlightEffect {
                count: 2,
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Player(opponent)],
            mine, // source is the controller's permanent
            controller,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count_param,
                ..
            } => {
                assert_eq!(
                    *player, opponent,
                    "the targeted opponent (not the controller) must be prompted to blight"
                );
                assert_eq!(
                    cards,
                    &[theirs],
                    "only the opponent's creatures are eligible to be blighted"
                );
                assert_eq!(*count_param, 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        // Drive the opponent's choice through the real handler.
        apply(
            &mut state,
            opponent,
            GameAction::SelectCards {
                cards: vec![theirs],
            },
        )
        .unwrap();

        assert_eq!(
            state
                .objects
                .get(&theirs)
                .unwrap()
                .counters
                .get(&CounterType::Minus1Minus1)
                .copied(),
            Some(2),
            "the opponent's chosen creature receives the 2 -1/-1 counters"
        );
        assert!(
            !state
                .objects
                .get(&mine)
                .unwrap()
                .counters
                .contains_key(&CounterType::Minus1Minus1),
            "the controller's creature is untouched when the opponent blights"
        );
    }
}
