use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{CollectEvidenceResume, GameState, PendingCast, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::collections::HashSet;

use super::super::engine::EngineError;

fn graveyard_cards(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .players
        .get(player.0 as usize)
        .map(|p| p.graveyard.iter().copied().collect())
        .unwrap_or_default()
}

fn total_mana_value(state: &GameState, cards: &[ObjectId]) -> u32 {
    cards
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|obj| obj.mana_cost.mana_value())
        .sum()
}

// CR 701.59b: Can't collect evidence if graveyard total mana value < N.
pub(crate) fn can_collect_evidence(state: &GameState, player: PlayerId, amount: u32) -> bool {
    total_mana_value(state, &graveyard_cards(state, player)) >= amount
}

fn waiting_state(
    state: &GameState,
    player: PlayerId,
    amount: u32,
    resume: CollectEvidenceResume,
) -> WaitingFor {
    WaitingFor::CollectEvidenceChoice {
        player,
        minimum_mana_value: amount,
        cards: graveyard_cards(state, player),
        resume: Box::new(resume),
    }
}

/// CR 701.59a: Collect evidence N — exile graveyard cards with total mana value >= N.
pub(crate) fn begin_cost_payment(
    state: &GameState,
    player: PlayerId,
    amount: u32,
    pending_cast: PendingCast,
) -> Result<WaitingFor, EngineError> {
    if !can_collect_evidence(state, player, amount) {
        return Err(EngineError::ActionNotAllowed(format!(
            "Not enough total mana value in graveyard to collect evidence {}",
            amount
        )));
    }

    Ok(waiting_state(
        state,
        player,
        amount,
        CollectEvidenceResume::Casting {
            pending_cast: Box::new(pending_cast),
        },
    ))
}

/// CR 701.59a: Collect evidence N as an effect — prompt player to exile cards.
pub(crate) fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::CollectEvidence { amount } => *amount,
        _ => {
            return Err(EffectError::MissingParam(
                "CollectEvidence amount".to_string(),
            ))
        }
    };

    if can_collect_evidence(state, ability.controller, amount) {
        let pending_ability = ability
            .sub_ability
            .as_ref()
            .map(|sub| sub.as_ref().clone())
            .unwrap_or_else(|| {
                ResolvedAbility::new(
                    Effect::TargetOnly {
                        target: crate::types::ability::TargetFilter::Any,
                    },
                    vec![],
                    ability.source_id,
                    ability.controller,
                )
            });
        state.waiting_for = waiting_state(
            state,
            ability.controller,
            amount,
            CollectEvidenceResume::Effect {
                pending_ability: Box::new(pending_ability),
            },
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CollectEvidence,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.59a + CR 701.59c: Exile chosen cards and resume linked ability if evidence was collected.
pub(crate) fn handle_choice(
    state: &mut GameState,
    player: PlayerId,
    minimum_mana_value: u32,
    legal_cards: &[ObjectId],
    resume: &CollectEvidenceResume,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let unique_count = chosen.iter().copied().collect::<HashSet<_>>().len();
    if unique_count != chosen.len() {
        return Err(EngineError::InvalidAction(
            "Selected cards must be unique".to_string(),
        ));
    }

    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card not eligible to collect evidence".to_string(),
            ));
        }
    }

    let still_legal: Vec<ObjectId> = state
        .players
        .get(player.0 as usize)
        .map(|p| p.graveyard.iter().copied().collect())
        .unwrap_or_default();
    for id in chosen {
        if !still_legal.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected card is no longer in your graveyard".to_string(),
            ));
        }
    }

    let total = total_mana_value(state, chosen);
    if total < minimum_mana_value {
        return Err(EngineError::InvalidAction(format!(
            "Chosen cards have total mana value {}, need at least {}",
            total, minimum_mana_value
        )));
    }

    for &id in chosen {
        super::super::zones::move_to_zone(state, id, Zone::Exile, events);
    }

    events.push(GameEvent::PlayerPerformedAction {
        player_id: player,
        action: PlayerActionKind::CollectEvidence,
    });

    match resume {
        CollectEvidenceResume::Casting { pending_cast } => {
            let mut pending = pending_cast.as_ref().clone();
            pending.ability.context.additional_cost_paid = true;
            super::super::casting_costs::pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.distribute,
                pending.origin_zone,
                events,
            )
        }
        CollectEvidenceResume::Effect { pending_ability } => {
            state.waiting_for = WaitingFor::Priority { player };
            super::resolve_ability_chain(state, pending_ability, events, 0)
                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            Ok(state.waiting_for.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, QuantityExpr, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    fn add_graveyard_card(
        state: &mut GameState,
        owner: PlayerId,
        card_id: u64,
        name: &str,
        generic_cost: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Graveyard,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.mana_cost = crate::types::mana::ManaCost::Cost {
            generic: generic_cost,
            shards: vec![],
        };
        id
    }

    #[test]
    fn collect_evidence_cost_choice_requires_threshold() {
        let mut state = GameState::new_two_player(42);
        add_graveyard_card(&mut state, PlayerId(0), 1, "One", 3);
        add_graveyard_card(&mut state, PlayerId(0), 2, "Two", 4);

        let pending = PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    up_to: false,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            ),
            crate::types::mana::ManaCost::zero(),
        );

        let result = begin_cost_payment(&state, PlayerId(0), 8, pending);
        assert!(result.is_err());
    }

    #[test]
    fn collect_evidence_choice_exiles_cards_and_emits_action() {
        let mut state = GameState::new_two_player(42);
        let first = add_graveyard_card(&mut state, PlayerId(0), 1, "One", 3);
        let second = add_graveyard_card(&mut state, PlayerId(0), 2, "Two", 5);
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Analyze the Pollen".to_string(),
            Zone::Hand,
        );
        let pending = PendingCast::new(
            source_id,
            CardId(100),
            ResolvedAbility::new(
                Effect::SearchLibrary {
                    filter: TargetFilter::Typed(TypedFilter::land()),
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: true,
                    target_player: None,
                    up_to: false,
                    selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            crate::types::mana::ManaCost::zero(),
        );

        let waiting = begin_cost_payment(&state, PlayerId(0), 8, pending).unwrap();
        let (minimum_mana_value, cards, resume) = match waiting {
            WaitingFor::CollectEvidenceChoice {
                minimum_mana_value,
                cards,
                resume,
                ..
            } => (minimum_mana_value, cards, resume),
            other => panic!("Expected CollectEvidenceChoice, got {:?}", other),
        };

        // CR 601.2a: Simulate announcement — `finalize_cast` expects the spell
        // to already be on the stack from the announcement step. Push the
        // StackEntry only; the object's zone remains at its origin (Hand)
        // until `finalize_cast` commits the Hand→Stack transition.
        let mut events = Vec::new();
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: source_id,
            source_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(100),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let next = handle_choice(
            &mut state,
            PlayerId(0),
            minimum_mana_value,
            &cards,
            &resume,
            &[first, second],
            &mut events,
        )
        .unwrap();

        assert!(matches!(next, WaitingFor::Priority { .. }));
        assert!(state.players[0].graveyard.is_empty());
        assert_eq!(state.objects.get(&first).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&second).unwrap().zone, Zone::Exile);
        let stack_entry = state.stack.back().expect("spell should be on stack");
        assert!(
            stack_entry
                .ability()
                .expect("spell should have ability")
                .context
                .additional_cost_paid
        );
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::CollectEvidence,
            } if *player_id == PlayerId(0)
        )));
    }

    #[test]
    fn collect_evidence_choice_rejects_duplicate_cards() {
        let mut state = GameState::new_two_player(42);
        let first = add_graveyard_card(&mut state, PlayerId(0), 1, "One", 8);
        let waiting = WaitingFor::CollectEvidenceChoice {
            player: PlayerId(0),
            minimum_mana_value: 8,
            cards: vec![first],
            resume: Box::new(CollectEvidenceResume::Effect {
                pending_ability: Box::new(ResolvedAbility::new(
                    Effect::TargetOnly {
                        target: TargetFilter::Any,
                    },
                    vec![],
                    ObjectId(100),
                    PlayerId(0),
                )),
            }),
        };
        let (minimum_mana_value, cards, resume) = match waiting {
            WaitingFor::CollectEvidenceChoice {
                minimum_mana_value,
                cards,
                resume,
                ..
            } => (minimum_mana_value, cards, resume),
            other => panic!("Expected CollectEvidenceChoice, got {:?}", other),
        };

        let err = handle_choice(
            &mut state,
            PlayerId(0),
            minimum_mana_value,
            &cards,
            &resume,
            &[first, first],
            &mut Vec::new(),
        )
        .expect_err("duplicate cards must be rejected");

        assert!(matches!(err, EngineError::InvalidAction(_)));
    }

    #[test]
    fn collect_evidence_effect_resumes_sub_ability() {
        let mut state = GameState::new_two_player(42);
        let first = add_graveyard_card(&mut state, PlayerId(0), 1, "One", 2);
        let second = add_graveyard_card(&mut state, PlayerId(0), 2, "Two", 2);
        let land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        let mut ability = ResolvedAbility::new(
            Effect::CollectEvidence { amount: 4 },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: true,
                target_player: None,
                up_to: false,
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let (minimum_mana_value, cards, resume) = match state.waiting_for.clone() {
            WaitingFor::CollectEvidenceChoice {
                minimum_mana_value,
                cards,
                resume,
                ..
            } => (minimum_mana_value, cards, resume),
            other => panic!("Expected CollectEvidenceChoice, got {:?}", other),
        };

        let mut resume_events = Vec::new();
        let next = handle_choice(
            &mut state,
            PlayerId(0),
            minimum_mana_value,
            &cards,
            &resume,
            &[first, second],
            &mut resume_events,
        )
        .unwrap();

        assert!(matches!(next, WaitingFor::SearchChoice { .. }));
        assert!(resume_events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::CollectEvidence,
                ..
            }
        )));
    }
}
