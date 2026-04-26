use crate::game::zones;
use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// CR 601.2a + CR 118.9: Cast a card from a zone without paying its mana cost.
///
/// Grants a `CastingPermission::ExileWithAltCost` on the target card(s),
/// following the same pattern as Discover (CR 701.57a). If the card is not
/// already in exile, it is moved there first — the casting pipeline expects
/// cards with exile-cast permissions to be in the exile zone.
///
/// After granting the permission, the resolver returns and the player receives
/// priority. They can then cast the card via the normal `GameAction::CastSpell`
/// flow, which handles target selection (CR 601.2c), modal choices, X costs,
/// additional costs, and all other casting steps.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (without_paying, cast_transformed, alt_ability_cost) = match &ability.effect {
        Effect::CastFromZone {
            without_paying_mana_cost,
            cast_transformed,
            alt_ability_cost,
            ..
        } => (
            *without_paying_mana_cost,
            *cast_transformed,
            alt_ability_cost.clone(),
        ),
        _ => return Err(EffectError::MissingParam("CastFromZone".to_string())),
    };

    // Collect target object IDs from the resolved ability's targets.
    let target_ids: Vec<_> = ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .collect();

    if target_ids.is_empty() {
        // No targets resolved — nothing to cast.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::CastFromZone,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    for &obj_id in &target_ids {
        // CR 601.2a: If the card is not in exile, move it there first.
        // The casting pipeline gates on Zone::Exile for permission-based casts,
        // so the card must be in exile before we grant the permission.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if current_zone.is_some_and(|z| z != Zone::Exile) {
            zones::move_to_zone(state, obj_id, Zone::Exile, events);
        }

        // CR 118.9: Grant casting permission. Three cases:
        //   - `alt_ability_cost: Some(_)` → `ExileWithAltAbilityCost` (Nashi:
        //     "pay life equal to its mana value rather than paying its mana
        //     cost" — non-mana alt cost replaces the mana cost).
        //   - `without_paying_mana_cost: true` → `ExileWithAltCost { zero }`
        //     (Discover, Suspend, "without paying its mana cost").
        //   - otherwise → `ExileWithAltCost { mana_cost }` (Nashi-style "you
        //     may play one of those cards" with normal mana payment).
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            let permission = if let Some(cost) = alt_ability_cost.clone() {
                CastingPermission::ExileWithAltAbilityCost {
                    cost,
                    constraint: None,
                }
            } else {
                let cost = if without_paying {
                    ManaCost::zero()
                } else {
                    obj.mana_cost.clone()
                };
                CastingPermission::ExileWithAltCost {
                    cost,
                    cast_transformed,
                    constraint: None,
                }
            };
            obj.casting_permissions.push(permission);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CastFromZone,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{CardPlayMode, Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_test_state() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_card_to_exile(state: &mut GameState, owner: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(state, card_id, owner, "Test Spell".to_string(), Zone::Exile);
        state.objects.get_mut(&obj_id).unwrap().mana_cost = ManaCost::generic(3);
        obj_id
    }

    fn add_card_to_hand(state: &mut GameState, owner: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(state, card_id, owner, "Hand Spell".to_string(), Zone::Hand);
        state.objects.get_mut(&obj_id).unwrap().mana_cost = ManaCost::generic(2);
        obj_id
    }

    #[test]
    fn grants_zero_cost_permission_on_exiled_card() {
        let mut state = make_test_state();
        let obj_id = add_card_to_exile(&mut state, PlayerId(1), CardId(100));

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Card should remain in exile with a zero-cost casting permission.
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Exile);
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
        )));
    }

    #[test]
    fn exiles_card_not_in_exile_then_grants_permission() {
        let mut state = make_test_state();
        let obj_id = add_card_to_hand(&mut state, PlayerId(1), CardId(200));

        // Card starts in opponent's hand.
        assert_eq!(state.objects.get(&obj_id).unwrap().zone, Zone::Hand);

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Card should have been moved to exile and granted permission.
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Exile);
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::zero()
        )));
    }

    #[test]
    fn without_paying_false_uses_card_mana_cost() {
        let mut state = make_test_state();
        let obj_id = add_card_to_exile(&mut state, PlayerId(1), CardId(300));

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: false,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Permission should use the card's own mana cost ({3}).
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost { cost, .. } if *cost == ManaCost::generic(3)
        )));
    }

    #[test]
    fn no_targets_emits_resolved_event() {
        let mut state = make_test_state();

        let ability = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::Any,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should emit EffectResolved with no errors.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::CastFromZone,
                ..
            }
        )));
    }
}
