use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{
    CollectEvidenceResume, GameState, PendingCast, PendingCostMoveResume, PendingManaAbility,
    WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::collections::HashSet;

use super::super::engine::EngineError;
use super::super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};

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
        // CR 202.3d + CR 709.4b: graveyard cards are off the stack, so a split
        // card contributes its combined mana value to the evidence total.
        .map(|obj| obj.effective_mana_value())
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

/// CR 605.2 + CR 701.59: begin collect-evidence payment for a mana ability's
/// activation cost (Cryptex's `{T}, Collect evidence 3: Add one mana...`).
/// Mirrors `begin_cost_payment` but resumes a parked `PendingManaAbility`
/// rather than a `PendingCast`. Payability (CR 701.59b graveyard-MV threshold)
/// is checked by the caller before this is reached.
pub(crate) fn begin_cost_payment_for_mana_ability(
    state: &GameState,
    player: PlayerId,
    amount: u32,
    pending: PendingManaAbility,
) -> WaitingFor {
    waiting_state(
        state,
        player,
        amount,
        CollectEvidenceResume::ManaAbility {
            pending_mana_ability: Box::new(pending),
        },
    )
}

/// CR 701.59a: Collect evidence N — exile graveyard cards with total mana value >= N.
pub(crate) fn begin_cost_payment(
    state: &GameState,
    player: PlayerId,
    amount: u32,
    pending_cast: PendingCast,
    source: crate::types::game_state::SpellCostSource,
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
            source,
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
        subject: None,
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

    move_evidence_costs(state, player, chosen.to_vec(), 0, resume.clone(), events)
}

fn cost_source_id(resume: &CollectEvidenceResume) -> ObjectId {
    match resume {
        CollectEvidenceResume::Casting { pending_cast, .. } => pending_cast.object_id,
        CollectEvidenceResume::Effect { pending_ability } => pending_ability.source_id,
        CollectEvidenceResume::ManaAbility {
            pending_mana_ability,
        } => pending_mana_ability.source_id,
    }
}

/// CR 701.59a + CR 614.1 + CR 616.1: Pay the selected evidence cards through
/// the replacement-aware cost-move pipeline. The typed root stores the exact
/// unpaid suffix only when a player choice interrupts this action.
fn move_evidence_costs(
    state: &mut GameState,
    player: PlayerId,
    chosen: Vec<ObjectId>,
    start_index: usize,
    resume: CollectEvidenceResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let source_id = cost_source_id(&resume);
    for index in start_index..chosen.len() {
        match zone_pipeline::move_object(
            state,
            ZoneMoveRequest::cost(chosen[index], Zone::Exile, source_id),
            events,
        ) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsChoice(_) => {
                state.pending_cost_move_resume =
                    Some(PendingCostMoveResume::CollectEvidencePayment {
                        player,
                        chosen,
                        paused_at_index: index,
                        resume: Box::new(resume),
                    });
                return Ok(state.waiting_for.clone());
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                unreachable!(
                    "a collect-evidence cost move to exile cannot require an Aura attachment"
                )
            }
        }
    }

    complete_cost_payment(state, player, chosen, resume, events)
}

/// CR 701.59a + CR 118.11: Finish collecting evidence after every selected
/// cost move settled, including a redirected or fully substituted move.
fn complete_cost_payment(
    state: &mut GameState,
    player: PlayerId,
    chosen: Vec<ObjectId>,
    resume: CollectEvidenceResume,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    events.push(GameEvent::PlayerPerformedAction {
        player_id: player,
        action: PlayerActionKind::CollectEvidence,
    });

    match resume {
        CollectEvidenceResume::Casting {
            pending_cast,
            source,
        } => {
            let mut pending = *pending_cast;
            pending.ability.context.additional_cost_paid = true;
            pending.additional_cost_source = source;
            // CR 602.2b: An ACTIVATED ability paying collect evidence as its cost
            // (Kylox's Voltstrider) goes on the stack via the activation
            // authority, not the spell-cast path. The exile loop above already
            // paid the interactive part; `push_activated_ability_to_stack` pays
            // any remaining (non-interactive) cost — collect evidence is a no-op
            // there — and pushes the ability. Detected by the activation index
            // carried on the pending; spell casts (bestow Detective's Phoenix)
            // have `None` and fall through to `pay_and_push`.
            if pending.activation_ability_index.is_some() {
                return super::super::casting_costs::finish_activated_ability_at_payment_boundary(
                    state, player, pending, events,
                );
            }
            let base_cost = pending.base_cost.clone();
            super::super::casting_costs::pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                base_cost,
                pending.casting_variant,
                pending.casting_permission_index,
                pending.cast_timing_permission,
                pending.distribute,
                pending.origin_zone,
                pending.payment_mode,
                events,
            )
        }
        CollectEvidenceResume::Effect { pending_ability } => {
            state.waiting_for = WaitingFor::Priority { player };
            super::resolve_ability_chain(state, &pending_ability, events, 0)
                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            Ok(state.waiting_for.clone())
        }
        // CR 605.2 + CR 701.59: Resume the parked mana-ability activation with
        // the selected evidence stamped in. The owned continuation can move its
        // pending activation directly after every cost move has settled.
        CollectEvidenceResume::ManaAbility {
            pending_mana_ability,
        } => {
            let mut pending = *pending_mana_ability;
            pending.collected_evidence = chosen;
            super::super::mana_abilities::advance_mana_ability_activation(state, pending, events)
        }
    }
}

/// CR 701.59a + CR 118.11 + CR 616.1: Resume exactly the unpaid evidence
/// suffix after the replacement dispatcher settles its paused move.
pub(crate) fn resume_cost_move_payment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::CollectEvidencePayment {
        player,
        chosen,
        paused_at_index,
        resume,
    }) = state.pending_cost_move_resume.take()
    else {
        unreachable!("collect-evidence cost-move resume requires its typed continuation")
    };

    move_evidence_costs(state, player, chosen, paused_at_index + 1, *resume, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCost, Effect, QuantityExpr, TargetFilter, TypedFilter};
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
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            ),
            crate::types::mana::ManaCost::zero(),
        );

        let result = begin_cost_payment(
            &state,
            PlayerId(0),
            8,
            pending,
            crate::types::game_state::SpellCostSource::Other,
        );
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
                    selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                    split: None,
                    source_zones: vec![crate::types::zones::Zone::Library],
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            crate::types::mana::ManaCost::zero(),
        );

        let waiting = begin_cost_payment(
            &state,
            PlayerId(0),
            8,
            pending,
            crate::types::game_state::SpellCostSource::Other,
        )
        .unwrap();
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
                selection_constraint: crate::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    fn mana_pending(source_id: ObjectId) -> PendingManaAbility {
        PendingManaAbility {
            player: PlayerId(0),
            source_id,
            ability_index: 0,
            ability_snapshot: None,
            color_override: None,
            resume: crate::types::game_state::ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        }
    }

    #[test]
    fn collect_evidence_cost_amount_recurses_composite() {
        use crate::game::mana_abilities;
        // Bare collect-evidence cost.
        assert_eq!(
            mana_abilities::collect_evidence_cost_amount(&AbilityCost::CollectEvidence {
                amount: 3
            }),
            Some(3)
        );
        // Composite[Tap, CollectEvidence{3}] — Cryptex's shape — recurses.
        assert_eq!(
            mana_abilities::collect_evidence_cost_amount(&AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, AbilityCost::CollectEvidence { amount: 3 },],
            }),
            Some(3)
        );
        // No collect-evidence component anywhere.
        assert_eq!(
            mana_abilities::collect_evidence_cost_amount(&AbilityCost::Composite {
                costs: vec![AbilityCost::Tap],
            }),
            None
        );
    }

    #[test]
    fn begin_cost_payment_for_mana_ability_produces_prompt_with_cards() {
        let mut state = GameState::new_two_player(42);
        let a = add_graveyard_card(&mut state, PlayerId(0), 1, "One", 2);
        let b = add_graveyard_card(&mut state, PlayerId(0), 2, "Two", 2);

        let waiting = begin_cost_payment_for_mana_ability(
            &state,
            PlayerId(0),
            3,
            mana_pending(ObjectId(100)),
        );

        let (minimum_mana_value, cards, resume) = match waiting {
            WaitingFor::CollectEvidenceChoice {
                minimum_mana_value,
                cards,
                resume,
                ..
            } => (minimum_mana_value, cards, resume),
            other => panic!("Expected CollectEvidenceChoice, got {:?}", other),
        };

        assert_eq!(minimum_mana_value, 3);
        assert!(cards.contains(&a) && cards.contains(&b));
        assert!(!cards.is_empty());
        assert!(matches!(
            resume.as_ref(),
            CollectEvidenceResume::ManaAbility { .. }
        ));
    }
}
