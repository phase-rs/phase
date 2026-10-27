use crate::game::filter::{matches_target_filter_for_zone, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::stickers::{
    apply_selected_sticker, available_sticker_candidates, may_put_sticker_on,
    name_sticker_position_choices, StickerCandidate,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect, EffectError, EffectKind, PlayerFilter,
    QuantityExpr, ResolvedAbility, StickerTicketCostPayment, TargetChoiceTiming, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ResolvingTriggerContext};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::stickers::{AppliedSticker, StickerKind};
use crate::types::zones::Zone;

/// CR 115.10a + CR 608.2d: enumerate a described, untargeted recipient in its
/// actual zone. Determined references and announced targets never rescan a pool.
pub(crate) fn recipient_ids(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let target = match &ability.effect {
        Effect::PutSticker { target, .. } | Effect::ApplySticker { target, .. } => target,
        _ => return Vec::new(),
    };
    let described =
        ability.target_choice_timing == TargetChoiceTiming::Resolution && !target.is_context_ref();
    let ids = if described {
        let effective = super::resolved_object_filter(state, ability, target);
        let ctx = FilterContext::from_ability(ability);
        let mut zones = target.extract_zones();
        if zones.is_empty() {
            zones.push(Zone::Battlefield);
        }
        zones
            .into_iter()
            .flat_map(|zone| {
                crate::game::targeting::zone_object_ids(state, zone)
                    .into_iter()
                    .map(move |id| (zone, id))
            })
            .filter(|(zone, id)| {
                matches_target_filter_for_zone(state, *id, *zone, &effective, &ctx)
            })
            .map(|(_, id)| id)
            .collect()
    } else if let TargetFilter::SpecificObject { id } = target {
        vec![*id]
    } else if target.is_context_ref() {
        crate::game::targeting::resolved_object_ids_for_filter(state, ability, target)
    } else {
        super::effect_object_targets(target, &ability.targets)
    };
    ids.into_iter()
        .filter(|id| {
            // CR 400.7: positional and legacy pins both constrain a bound
            // recipient. ParentTargetSlot is resolved before any projection.
            let position = match target {
                _ if described => None,
                TargetFilter::ParentTargetSlot { index } => Some(*index),
                _ => ability
                    .targets
                    .iter()
                    .position(|t| *t == TargetRef::Object(*id)),
            };
            position.is_none_or(|index| ability.target_occurrence_is_current(index, state))
                && (described || ability.target_pin_is_current(*id, state))
                // CR 123.3b: ownership applies to every route, without any
                // targeting-only hexproof, shroud or protection checks.
                && state.objects.get(id).is_some_and(|object| {
                    may_put_sticker_on(object, ability.controller)
                })
        })
        .collect()
}

/// CR 400.7 + CR 608.2d: bind only this instruction to the chosen occurrence.
pub(crate) fn bind_recipient(
    ability: &mut ResolvedAbility,
    id: ObjectId,
    pin: ObjectIncarnationRef,
) {
    if let Effect::PutSticker { target, .. } = &mut ability.effect {
        *target = TargetFilter::SpecificObject { id };
    }
    ability.replace_target_occurrences(vec![(TargetRef::Object(id), Some(pin))]);
}

pub(crate) fn optional_is_infeasible(state: &GameState, ability: &ResolvedAbility) -> bool {
    let Effect::PutSticker {
        kind,
        max_ticket_cost,
        ticket_cost_payment,
        ..
    } = &ability.effect
    else {
        return false;
    };
    recipient_ids(state, ability).is_empty()
        || available_sticker_candidates(
            state,
            ability.controller,
            *kind,
            max_ticket_cost
                .as_ref()
                .map(|expr| resolve_quantity_with_targets(state, expr, ability).max(0) as u32),
            matches!(ticket_cost_payment, StickerTicketCostPayment::WithoutPaying),
        )
        .is_empty()
}

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::PutSticker {
            target: _,
            kind,
            count,
            max_ticket_cost,
            ticket_cost_payment,
        } => {
            // CR 608.2c: direct resolver callers share the instruction's
            // fresh-result boundary, including a missing determined recipient.
            state.placed_sticker_this_resolution = None;
            let Some(id) = recipient_ids(state, ability).first().copied() else {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::PutSticker,
                    source_id: ability.source_id,
                    subject: None,
                });
                return Ok(());
            };
            let mut bound = ability.clone();
            bind_recipient(
                &mut bound,
                id,
                ObjectIncarnationRef::of(id, state.objects[&id].incarnation),
            );
            resolve_put_sticker(
                state,
                &bound,
                PutStickerRequest {
                    target: &TargetFilter::SpecificObject { id },
                    kind: *kind,
                    count,
                    max_ticket_cost: max_ticket_cost.as_ref(),
                    ticket_cost_payment: *ticket_cost_payment,
                },
                events,
            )
        }
        Effect::ApplySticker {
            target: _,
            sticker,
            pay_ticket,
        } => {
            let targets = recipient_ids(state, ability);
            let Some(target_id) = targets.first().copied() else {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::ApplySticker,
                    source_id: ability.source_id,
                    subject: None,
                });
                return Ok(());
            };
            apply_selected_sticker(
                state,
                ability.controller,
                target_id,
                sticker.clone(),
                *pay_ticket,
                events,
            );
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ApplySticker,
                source_id: ability.source_id,
                subject: None,
            });
            Ok(())
        }
        _ => Err(EffectError::MissingParam("Sticker effect".to_string())),
    }
}

struct PutStickerRequest<'a> {
    target: &'a TargetFilter,
    kind: Option<StickerKind>,
    count: &'a QuantityExpr,
    max_ticket_cost: Option<&'a QuantityExpr>,
    ticket_cost_payment: StickerTicketCostPayment,
}

fn resolve_put_sticker(
    state: &mut GameState,
    ability: &ResolvedAbility,
    request: PutStickerRequest<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // The recipient authority has already resolved and pinned this instruction.
    let selected = super::effect_object_targets(request.target, &ability.targets)
        .first()
        .copied();

    let max_ticket_cost = request.max_ticket_cost.map(|expr| QuantityExpr::Fixed {
        value: resolve_quantity_with_targets(state, expr, ability).max(0),
    });
    let (count_expr, up_to) = request.count.peel_up_to();
    let count = resolve_quantity_with_targets(state, count_expr, ability).max(0) as u32;

    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    if up_to {
        prompt_count_choice(
            state,
            ability,
            request.target,
            request.kind,
            count,
            max_ticket_cost.as_ref(),
            request.ticket_cost_payment,
        );
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    if count > 1 {
        let chain = repeated_single_put_definition(
            request.target.clone(),
            request.kind,
            count,
            max_ticket_cost,
            request.ticket_cost_payment,
        );
        let mut resolved = crate::game::ability_utils::build_resolved_from_def(
            &chain,
            ability.source_id,
            ability.controller,
        );
        resolved.mirror_targets_from(ability);
        resolved.context = ability.context.clone();
        resolved.chosen_x = ability.chosen_x;
        resolved.chosen_players = ability.chosen_players.clone();
        resolved.set_replacement_applied_recursive(ability.replacement_applied.clone());
        return super::resolve_ability_chain(state, &resolved, events, 1);
    }

    let Some(target_id) = selected else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    let Some(target_obj) = state.objects.get(&target_id) else {
        return Ok(());
    };
    let max_ticket_cost = max_ticket_cost
        .as_ref()
        .map(|expr| resolve_quantity_with_targets(state, expr, ability).max(0) as u32);
    let candidates = available_sticker_candidates(
        state,
        ability.controller,
        request.kind,
        max_ticket_cost,
        matches!(
            request.ticket_cost_payment,
            StickerTicketCostPayment::WithoutPaying
        ),
    );

    let branches = placement_branches(target_obj, candidates);
    if branches.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    if branches.len() == 1 {
        let mut resolved = crate::game::ability_utils::build_resolved_from_def(
            &branches[0],
            ability.source_id,
            ability.controller,
        );
        resolved.mirror_targets_from(ability);
        resolved.context = ability.context.clone();
        resolved.set_replacement_applied_recursive(ability.replacement_applied.clone());
        return super::resolve_ability_chain(state, &resolved, events, 1);
    }

    super::choose_one_of::prompt_next(
        state,
        super::choose_one_of::PromptRequest {
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_occurrences: crate::types::game_state::ParentTargetOccurrences::of(ability),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            continuation: None,
            players: vec![ability.controller],
            trigger_context: ResolvingTriggerContext::capture(state),
        },
    );
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PutSticker,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

fn placement_branches(
    target: &crate::game::game_object::GameObject,
    candidates: Vec<StickerCandidate>,
) -> Vec<AbilityDefinition> {
    candidates
        .into_iter()
        .map(|candidate| {
            let description = candidate.description.clone();
            let mut branch = if matches!(candidate.sticker, AppliedSticker::Name { .. }) {
                let positions: Vec<_> = name_sticker_position_choices(target, &candidate.sticker)
                    .into_iter()
                    .map(|position| apply_sticker_branch(target.id, position))
                    .collect();
                if positions.len() == 1 {
                    positions.into_iter().next().unwrap()
                } else {
                    // CR 123.3 + CR 123.6b: the instructed player chooses the
                    // discrete sticker; only its position belongs to the recipient's
                    // controller. Placement and payment retain the original controller.
                    // CR 108.4 + CR 108.4a: a controllerless card uses its owner,
                    // not a departed permanent's last-known controller.
                    let chooser = match target.zone {
                        Zone::Battlefield | Zone::Stack => {
                            PlayerFilter::ParentObjectTargetController
                        }
                        Zone::Command if target.is_emblem => {
                            PlayerFilter::ParentObjectTargetController
                        }
                        Zone::Command
                        | Zone::Library
                        | Zone::Hand
                        | Zone::Graveyard
                        | Zone::Exile => PlayerFilter::ParentObjectTargetOwner,
                    };
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChooseOneOf {
                            chooser,
                            branches: positions,
                            kind: ChooseOneOfKind::Ordinary,
                        },
                    )
                }
            } else {
                apply_sticker_branch(target.id, candidate)
            };
            branch.description = Some(description);
            branch
        })
        .collect()
}

fn apply_sticker_branch(target: ObjectId, candidate: StickerCandidate) -> AbilityDefinition {
    let mut branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ApplySticker {
            target: TargetFilter::SpecificObject { id: target },
            sticker: candidate.sticker,
            pay_ticket: candidate.pay_ticket,
        },
    );
    branch.description = Some(candidate.description);
    branch
}

fn prompt_count_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
    kind: Option<StickerKind>,
    count: u32,
    max_ticket_cost: Option<&QuantityExpr>,
    ticket_cost_payment: StickerTicketCostPayment,
) {
    let mut branches = Vec::new();

    let mut zero = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
    zero.description = Some("Do not put a sticker".to_string());
    branches.push(zero);

    for amount in 1..=count {
        let mut branch = repeated_single_put_definition(
            target.clone(),
            kind,
            amount,
            max_ticket_cost.cloned(),
            ticket_cost_payment,
        );
        branch.description = Some(format!(
            "Put {amount} sticker{}",
            if amount == 1 { "" } else { "s" }
        ));
        branches.push(branch);
    }

    super::choose_one_of::prompt_next(
        state,
        super::choose_one_of::PromptRequest {
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_occurrences: crate::types::game_state::ParentTargetOccurrences::of(ability),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            continuation: None,
            players: vec![ability.controller],
            trigger_context: ResolvingTriggerContext::capture(state),
        },
    );
}

fn repeated_single_put_definition(
    target: TargetFilter,
    kind: Option<StickerKind>,
    count: u32,
    max_ticket_cost: Option<QuantityExpr>,
    ticket_cost_payment: StickerTicketCostPayment,
) -> AbilityDefinition {
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutSticker {
            target: target.clone(),
            kind,
            count: QuantityExpr::Fixed { value: 1 },
            max_ticket_cost: max_ticket_cost.clone(),
            ticket_cost_payment,
        },
    );
    let mut cursor = &mut root;
    for _ in 1..count {
        cursor.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutSticker {
                target: target.clone(),
                kind,
                count: QuantityExpr::Fixed { value: 1 },
                max_ticket_cost: max_ticket_cost.clone(),
                ticket_cost_payment,
            },
        )));
        cursor = cursor.sub_ability.as_mut().unwrap();
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::game::stickers::set_player_sticker_sheets;
    use crate::game::zones::move_to_zone;

    #[test]
    fn determined_slot_reads_raw_position_and_honors_current_and_legacy_pins() {
        for legacy in [false, true] {
            for live_second in [false, true] {
                let mut scenario = GameScenario::new();
                let source = scenario.add_creature(P0, "Separate source", 2, 2).id();
                let first = scenario.add_creature(P0, "Stale slot zero", 2, 2).id();
                let second = scenario.add_creature(P0, "Slot one", 2, 2).id();
                let mut runner = scenario.build();
                let state = runner.state_mut();
                let first_pin = ObjectIncarnationRef::from_object(&state.objects[&first]);
                let second_pin = ObjectIncarnationRef::from_object(&state.objects[&second]);
                let mut ability = ResolvedAbility::new(
                    Effect::PutSticker {
                        target: TargetFilter::ParentTargetSlot { index: 1 },
                        kind: Some(StickerKind::PowerToughness),
                        count: QuantityExpr::Fixed { value: 1 },
                        max_ticket_cost: Some(QuantityExpr::Fixed { value: 2 }),
                        ticket_cost_payment: StickerTicketCostPayment::WithoutPaying,
                    },
                    vec![],
                    source,
                    P0,
                );
                if legacy {
                    ability.set_unpinned_targets(vec![
                        TargetRef::Object(first),
                        TargetRef::Object(second),
                    ]);
                    ability.target_incarnations = vec![first_pin, second_pin];
                } else {
                    ability.replace_target_occurrences(vec![
                        (TargetRef::Object(first), Some(first_pin)),
                        (TargetRef::Object(second), Some(second_pin)),
                    ]);
                }
                let mut events = Vec::new();
                move_to_zone(state, first, Zone::Exile, &mut events);
                move_to_zone(state, first, Zone::Battlefield, &mut events);
                if !live_second {
                    move_to_zone(state, second, Zone::Exile, &mut events);
                    move_to_zone(state, second, Zone::Battlefield, &mut events);
                }
                assert!(!first_pin.is_current(state));
                assert_eq!(
                    recipient_ids(state, &ability),
                    if live_second { vec![second] } else { vec![] }
                );
                set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);
                events.clear();
                resolve(state, &ability, &mut events).unwrap();
                assert_eq!(
                    state.objects[&second].stickers.len(),
                    usize::from(live_second)
                );
                assert!(state.objects[&first].stickers.is_empty());
                assert!(state.objects[&source].stickers.is_empty());
                assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::StickerPlaced { object_id, .. } if *object_id == second)).count(), usize::from(live_second));
                // Missing raw slot one never falls back to slot zero or source.
                ability.set_unpinned_targets(vec![TargetRef::Object(first)]);
                assert!(recipient_ids(state, &ability).is_empty());
            }
        }
    }
}
