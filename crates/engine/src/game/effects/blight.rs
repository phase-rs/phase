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
    let count = match &ability.effect {
        Effect::BlightEffect { count } => *count,
        _ => return Ok(()),
    };

    let controller = ability.controller;
    let source_id = ability.source_id;

    // CR 701.68a: Eligible creatures are those the controller of this ability
    // controls on the battlefield.
    let eligible: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == controller
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

    // CR 701.68a: The controller chooses exactly one creature they control.
    // `count` (in EffectZoneChoice) is the number of creatures chosen (1);
    // `count_param` carries N — the number of -1/-1 counters to place.
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: controller,
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
        count_param: count,
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
            Effect::BlightEffect { count },
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
        // target_filter() must return None for BlightEffect.
        assert!(
            Effect::BlightEffect { count: 1 }.target_filter().is_none(),
            "BlightEffect must be non-targeted"
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
            .quantity_modification(QuantityModification::Double);
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
}
