use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::{combat, targeting};
use crate::types::ability::{
    Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 509.1g + CR 506.3e + CR 707.2: Resolve `Effect::CopyTokenBlockingAttacker`.
///
/// For each attacking creature matched by `source_filter`, create a token that's
/// a copy of it (CR 707.2) and put that token onto the battlefield blocking the
/// attacker it copies (CR 506.3e + CR 509.1g). Mirror Match is the canonical
/// card. The created token ids are republished to `state.last_created_token_ids`
/// so a composed delayed trigger can exile "those tokens" at end of combat via
/// `TargetFilter::LastCreated`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (source_filter, owner) = match &ability.effect {
        Effect::CopyTokenBlockingAttacker {
            source_filter,
            owner,
        } => (source_filter.clone(), owner.clone()),
        _ => {
            return Err(EffectError::MissingParam(
                "CopyTokenBlockingAttacker".to_string(),
            ))
        }
    };

    // CR 508.1: resolve the attackers to copy. Non-targeting — evaluated against
    // the battlefield (or any zones the filter names) at resolution time, the
    // same way `Effect::CopyTokenOf`'s `source_filter` branch enumerates its
    // copy sources.
    let zones = {
        let explicit = source_filter.extract_zones();
        if explicit.is_empty() {
            vec![Zone::Battlefield]
        } else {
            explicit
        }
    };
    let filter_ctx = FilterContext::from_ability(ability);
    let attacker_ids: Vec<ObjectId> = zones
        .into_iter()
        .flat_map(|zone| targeting::zone_object_ids(state, zone))
        .filter(|id| matches_target_filter(state, *id, &source_filter, &filter_ctx))
        .collect();

    let mut all_created: Vec<ObjectId> = Vec::new();
    for attacker_id in attacker_ids {
        // CR 707.2: create one token copy of this specific attacker, delegating
        // to the single-authority token-copy resolver so copiable values,
        // predefined abilities, and ETB events are handled identically to every
        // other copy-token effect.
        let copy_effect = Effect::CopyTokenOf {
            target: TargetFilter::Any,
            owner: owner.clone(),
            source_filter: Some(TargetFilter::SpecificObject { id: attacker_id }),
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        };
        let copy_ability =
            ResolvedAbility::new(copy_effect, vec![], ability.source_id, ability.controller);
        crate::game::effects::token_copy::resolve(state, &copy_ability, events)?;

        // CR 506.3e + CR 509.1g: each fresh copy is put onto the battlefield
        // blocking the attacker it copied. `last_created_token_ids` holds exactly
        // this attacker's single copy at this point in the loop.
        for token_id in state.last_created_token_ids.clone() {
            combat::place_blocking(state, token_id, attacker_id);
            all_created.push(token_id);
        }
    }

    // CR 603.7c + CR 701.36a: republish the full created-token set so a composed
    // "exile those tokens at end of combat" delayed trigger snapshots all copies
    // via `TargetFilter::LastCreated` rather than just the last loop iteration's.
    state.last_created_token_ids = all_created;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CopyTokenOf,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, FilterProp, TypedFilter};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    /// CR 506.3e + CR 509.1g: For each creature attacking the controller, a copy
    /// token is created and put onto the battlefield blocking that attacker. The
    /// attacker becomes blocked and the created tokens are published for the
    /// "those tokens" anaphor.
    #[test]
    fn copies_and_blocks_each_attacker() {
        let mut state = GameState::new_two_player(42);
        let defender = PlayerId(0); // Mirror Match's controller (defending player).
        let aggressor = PlayerId(1);

        // Two creatures controlled by the aggressor, attacking the defender.
        let make_attacker = |state: &mut GameState, name: &str| {
            let id = create_object(
                state,
                CardId(1),
                aggressor,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            obj.card_types = obj.base_card_types.clone();
            id
        };
        let atk_a = make_attacker(&mut state, "Grizzly Bears");
        let atk_b = make_attacker(&mut state, "Hill Giant");

        let mut combat = CombatState::default();
        combat.attackers.push(AttackerInfo::new(
            atk_a,
            AttackTarget::Player(defender),
            defender,
        ));
        combat.attackers.push(AttackerInfo::new(
            atk_b,
            AttackTarget::Player(defender),
            defender,
        ));
        state.combat = Some(combat);

        // Mirror Match: copy + block each creature attacking the controller.
        let source_filter =
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Attacking {
                    defender: Some(ControllerRef::You),
                }]),
            );
        let ability = ResolvedAbility::new(
            Effect::CopyTokenBlockingAttacker {
                source_filter,
                owner: TargetFilter::Controller,
            },
            vec![],
            create_object(
                &mut state,
                CardId(2),
                defender,
                "Mirror Match".to_string(),
                Zone::Stack,
            ),
            defender,
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Two copy tokens were created, controlled by the defender.
        assert_eq!(state.last_created_token_ids.len(), 2);
        for token_id in &state.last_created_token_ids {
            let token = &state.objects[token_id];
            assert!(token.is_token, "created object is a token");
            assert_eq!(
                token.controller, defender,
                "copy is controlled by Mirror Match's controller"
            );
            assert!(!token.tapped, "blocking copy is not tapped");
        }

        // Both attackers are now blocked, each by exactly one created copy.
        let combat = state.combat.as_ref().unwrap();
        for atk in [atk_a, atk_b] {
            let info = combat
                .attackers
                .iter()
                .find(|a| a.object_id == atk)
                .unwrap();
            assert!(info.blocked, "attacker {atk:?} became blocked");
            assert_eq!(
                combat.blocker_assignments[&atk].len(),
                1,
                "one blocker per attacker"
            );
        }
        // The two blockers are the two created tokens.
        let blockers: std::collections::HashSet<_> =
            combat.blocker_to_attacker.keys().copied().collect();
        let created: std::collections::HashSet<_> =
            state.last_created_token_ids.iter().copied().collect();
        assert_eq!(blockers, created, "every created copy is a blocker");
    }
}
