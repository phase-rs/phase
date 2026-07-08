use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::stickers::{
    apply_selected_sticker, available_sticker_candidates, name_sticker_position_choices,
    StickerCandidate,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility,
    StickerTicketCostPayment, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::stickers::{AppliedSticker, StickerKind};

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::PutSticker {
            target,
            kind,
            count,
            max_ticket_cost,
            ticket_cost_payment,
        } => resolve_put_sticker(
            state,
            ability,
            PutStickerRequest {
                target,
                kind: *kind,
                count,
                max_ticket_cost: max_ticket_cost.as_ref(),
                ticket_cost_payment: *ticket_cost_payment,
            },
            events,
        ),
        Effect::ApplySticker {
            target,
            sticker,
            pay_ticket,
        } => {
            let targets = super::effect_object_targets(target, &ability.targets);
            let Some(target_id) = targets.first().copied() else {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::ApplySticker,
                    source_id: ability.source_id,
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
    let (count_expr, up_to) = request.count.peel_up_to();
    let count = resolve_quantity_with_targets(state, count_expr, ability).max(0) as u32;

    if count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
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
            request.max_ticket_cost,
            request.ticket_cost_payment,
        );
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    if count > 1 {
        let chain = repeated_single_put_definition(
            request.target.clone(),
            request.kind,
            count,
            request.max_ticket_cost.cloned(),
            request.ticket_cost_payment,
        );
        let mut resolved = crate::game::ability_utils::build_resolved_from_def(
            &chain,
            ability.source_id,
            ability.controller,
        );
        resolved.targets = ability.targets.clone();
        resolved.context = ability.context.clone();
        resolved.chosen_x = ability.chosen_x;
        resolved.chosen_players = ability.chosen_players.clone();
        return super::resolve_ability_chain(state, &resolved, events, 1);
    }

    let targets = super::effect_object_targets(request.target, &ability.targets);
    let Some(target_id) = targets.first().copied() else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let Some(target_obj) = state.objects.get(&target_id) else {
        return Ok(());
    };
    let max_ticket_cost = request
        .max_ticket_cost
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

    let expanded = expand_candidates_for_target(target_obj, candidates);
    if expanded.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    if expanded.len() == 1 {
        let chosen = expanded.into_iter().next().unwrap();
        apply_selected_sticker(
            state,
            ability.controller,
            target_id,
            chosen.sticker,
            chosen.pay_ticket,
            events,
        );
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PutSticker,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let branches: Vec<AbilityDefinition> = expanded
        .into_iter()
        .map(|candidate| {
            let mut branch = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ApplySticker {
                    target: TargetFilter::SpecificObject { id: target_id },
                    sticker: candidate.sticker,
                    pay_ticket: candidate.pay_ticket,
                },
            );
            branch.description = Some(candidate.description);
            branch
        })
        .collect();
    super::choose_one_of::prompt_next(
        state,
        super::choose_one_of::PromptRequest {
            controller: ability.controller,
            source_id: ability.source_id,
            branches,
            parent_targets: ability.targets.clone(),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            players: vec![ability.controller],
        },
    );
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PutSticker,
        source_id: ability.source_id,
    });
    Ok(())
}

fn expand_candidates_for_target(
    target: &crate::game::game_object::GameObject,
    candidates: Vec<StickerCandidate>,
) -> Vec<StickerCandidate> {
    let mut expanded = Vec::new();
    for candidate in candidates {
        if matches!(candidate.sticker, AppliedSticker::Name { .. }) {
            let positions = name_sticker_position_choices(target, &candidate.sticker);
            expanded.extend(positions);
        } else {
            expanded.push(candidate);
        }
    }
    expanded
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
            parent_targets: ability.targets.clone(),
            context: ability.context.clone(),
            replacement_applied: ability.replacement_applied.clone(),
            players: vec![ability.controller],
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
