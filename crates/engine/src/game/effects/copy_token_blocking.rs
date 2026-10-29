use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::{combat, targeting};
use crate::types::ability::{
    Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
    TokenAttackDestination,
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
    super::with_instruction_result_entry(
        state,
        ability,
        crate::types::ability::InstructionResultEntry::Independent,
        |state, executable, declarations| resolve_owned(state, executable, events, declarations),
    )
}

pub(crate) fn resolve_owned(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    declarations: super::AdmittedInstructionResult<'_>,
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

    let mut attackers: std::collections::VecDeque<_> = attacker_ids
        .into_iter()
        .filter_map(|id| {
            state
                .objects
                .get(&id)
                .map(crate::types::identifiers::ObjectIncarnationRef::from_object)
        })
        .collect();
    let (caller, work) = if let Some(current) = attackers.pop_front() {
        let work = copy_work(state, ability, current, &owner)?;
        (
            crate::types::game_state::TokenCreationCallerState::Blocking {
                current,
                remaining: attackers,
                owner,
            },
            work,
        )
    } else {
        (
            crate::types::game_state::TokenCreationCallerState::Plain,
            std::collections::VecDeque::new(),
        )
    };
    super::token_copy::begin_creation_owned(
        state,
        ability,
        work,
        EffectKind::CopyTokenOf,
        caller,
        events,
        declarations,
    )?;
    Ok(())
}

pub(crate) fn copy_work(
    state: &mut GameState,
    ability: &ResolvedAbility,
    attacker: crate::types::identifiers::ObjectIncarnationRef,
    owner: &TargetFilter,
) -> Result<std::collections::VecDeque<crate::types::game_state::TokenCreationWork>, EffectError> {
    if !state
        .objects
        .get(&attacker.object_id)
        .is_some_and(|object| {
            crate::types::identifiers::ObjectIncarnationRef::from_object(object) == attacker
                && object.zone == Zone::Battlefield
        })
    {
        return Ok(std::collections::VecDeque::new());
    }
    let copy = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::Any,
            owner: owner.clone(),
            source_filter: Some(TargetFilter::SpecificObject {
                id: attacker.object_id,
            }),
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        vec![],
        ability.source_id,
        ability.controller,
    );
    super::token_copy::prepare_copy_creation(state, &copy)
}

pub(crate) fn complete_creation_segment(
    state: &mut GameState,
    attacker: crate::types::identifiers::ObjectIncarnationRef,
    members: &[crate::types::identifiers::ObjectIncarnationRef],
) {
    if !state
        .objects
        .get(&attacker.object_id)
        .is_some_and(|object| {
            crate::types::identifiers::ObjectIncarnationRef::from_object(object) == attacker
                && object.zone == Zone::Battlefield
        })
    {
        return;
    }
    for member in members {
        if state.objects.get(&member.object_id).is_some_and(|object| {
            crate::types::identifiers::ObjectIncarnationRef::from_object(object) == *member
                && object.zone == Zone::Battlefield
                && object
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature)
                && !object
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Battle)
        }) {
            combat::place_blocking(state, member.object_id, attacker.object_id);
        }
    }
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
    #[test]
    fn copy_token_blocking_completion_synchronous() {
        let (state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::CopyTokenBlockingAttacker {
                source_filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().properties(vec![
                        crate::types::ability::FilterProp::Attacking { defender: None },
                    ]),
                ),
                owner: TargetFilter::Controller,
            },
            super::super::token_copy::tests::CreationPauseCase::Synchronous,
        );
        assert_eq!(state.combat.as_ref().unwrap().blocker_assignments.len(), 2);
        assert!(state.last_created_token_ids.iter().all(|id| state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(id)));
    }
    #[test]
    fn copy_token_blocking_completion_queued_child() {
        let (state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::CopyTokenBlockingAttacker {
                source_filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().properties(vec![
                        crate::types::ability::FilterProp::Attacking { defender: None },
                    ]),
                ),
                owner: TargetFilter::Controller,
            },
            super::super::token_copy::tests::CreationPauseCase::Queued,
        );
        assert_eq!(state.combat.as_ref().unwrap().blocker_assignments.len(), 2);
        assert!(state.last_created_token_ids.iter().all(|id| state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(id)));
    }
    #[test]
    fn copy_token_blocking_completion_counter_pause() {
        let (state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::CopyTokenBlockingAttacker {
                source_filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().properties(vec![
                        crate::types::ability::FilterProp::Attacking { defender: None },
                    ]),
                ),
                owner: TargetFilter::Controller,
            },
            super::super::token_copy::tests::CreationPauseCase::Counter,
        );
        assert_eq!(state.combat.as_ref().unwrap().blocker_assignments.len(), 2);
        assert!(state.last_created_token_ids.iter().all(|id| state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(id)));
    }
    #[test]
    fn copy_token_blocking_completion_queued_child_current_reload() {
        let (state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::CopyTokenBlockingAttacker {
                source_filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().properties(vec![
                        crate::types::ability::FilterProp::Attacking { defender: None },
                    ]),
                ),
                owner: TargetFilter::Controller,
            },
            super::super::token_copy::tests::CreationPauseCase::ReloadQueued,
        );
        assert_eq!(state.combat.as_ref().unwrap().blocker_assignments.len(), 2);
        assert!(state.last_created_token_ids.iter().all(|id| state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(id)));
    }
    #[test]
    fn copy_token_blocking_completion_counter_current_reload() {
        let (state, _events) = super::super::token_copy::tests::creation_wrapper_case(
            Effect::CopyTokenBlockingAttacker {
                source_filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().properties(vec![
                        crate::types::ability::FilterProp::Attacking { defender: None },
                    ]),
                ),
                owner: TargetFilter::Controller,
            },
            super::super::token_copy::tests::CreationPauseCase::ReloadCounter,
        );
        assert_eq!(state.combat.as_ref().unwrap().blocker_assignments.len(), 2);
        assert!(state.last_created_token_ids.iter().all(|id| state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(id)));
    }
}
