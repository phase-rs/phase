use std::collections::HashSet;

use crate::types::ability::{AbilityCost, AdditionalCost, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::{ManaCostShard, ManaType};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::casting::emit_targeting_events;
use super::engine::EngineError;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::restrictions;
use super::stack;
use super::zones;

use super::ability_utils::flatten_targets_in_chain;

/// Handle the player's decision on an additional cost (kicker, blight, "or pay").
///
/// For `Optional`: `pay=true` pays the cost and sets `additional_cost_paid`, `pay=false` skips.
/// For `Choice`: `pay=true` pays the first cost, `pay=false` pays the second cost.
pub(crate) fn handle_decide_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    additional_cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut ability = pending.ability;

    let cost_to_pay = match additional_cost {
        AdditionalCost::Optional(cost) => {
            if pay {
                ability.context.additional_cost_paid = true;
                Some(cost.clone())
            } else {
                None
            }
        }
        AdditionalCost::Choice(preferred, fallback) => {
            if pay {
                ability.context.additional_cost_paid = true;
                Some(preferred.clone())
            } else {
                Some(fallback.clone())
            }
        }
    };

    let updated_pending = PendingCast { ability, ..pending };

    if let Some(cost) = cost_to_pay {
        pay_additional_cost(state, player, cost, updated_pending, events)
    } else {
        pay_and_push(
            state,
            player,
            updated_pending.object_id,
            updated_pending.card_id,
            updated_pending.ability,
            &updated_pending.cost,
            events,
        )
    }
}

/// Complete the discard-for-cost flow: discard selected cards, then continue casting.
pub(crate) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != expected {
        return Err(EngineError::InvalidAction(format!(
            "Must discard exactly {} card(s), got {}",
            expected,
            chosen.len()
        )));
    }
    for card_id in chosen {
        if !legal_cards.contains(card_id) {
            return Err(EngineError::InvalidAction(
                "Selected card not in hand".to_string(),
            ));
        }
    }

    // Move each chosen card to graveyard and emit discard events
    for &card_id in chosen {
        super::zones::move_to_zone(state, card_id, Zone::Graveyard, events);
        events.push(GameEvent::Discarded {
            player_id: player,
            object_id: card_id,
        });
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        events,
    )
}

/// Check for an additional cost on the object being cast. If one exists,
/// return `WaitingFor::OptionalCostChoice` so the player can decide;
/// otherwise proceed directly to `pay_and_push`.
///
/// This function sits between targeting and payment in the casting pipeline:
/// `CastSpell → [ModeChoice] → [TargetSelection] → [AdditionalCostChoice] → pay_and_push → Stack`
pub(super) fn check_additional_cost_or_pay(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone());

    if let Some(additional_cost) = additional {
        return Ok(WaitingFor::OptionalCostChoice {
            player,
            cost: additional_cost,
            pending_cast: Box::new(PendingCast {
                object_id,
                card_id,
                ability,
                cost: cost.clone(),
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: Vec::new(),
            }),
        });
    }

    pay_and_push(state, player, object_id, card_id, ability, cost, events)
}

/// CR 601.2b: Pay an additional cost, returning a WaitingFor if interactive input is needed
/// (e.g. choosing which card to discard), or continuing to pay_and_push if atomic.
fn pay_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: AbilityCost,
    pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    match cost {
        AbilityCost::PayLife { amount } => {
            // CR 118.3: A player can pay life as a cost only if their life >= amount.
            let player_state = &mut state.players[player.0 as usize];
            player_state.life -= amount as i32;
            events.push(GameEvent::LifeChanged {
                player_id: player,
                amount: -(amount as i32),
            });
        }
        AbilityCost::Blight { count } => {
            // Place blight counters on caster's lands
            let lands: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && obj
                                .card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Land)
                    })
                })
                .collect();
            for (i, &land_id) in lands.iter().enumerate() {
                if i >= count as usize {
                    break;
                }
                if let Some(obj) = state.objects.get_mut(&land_id) {
                    *obj.counters
                        .entry(super::game_object::CounterType::Generic(
                            "blight".to_string(),
                        ))
                        .or_insert(0) += 1;
                }
            }
        }
        AbilityCost::Discard { count, .. } => {
            // CR 601.2b: Discard requires interactive card selection — return a WaitingFor.
            let eligible: Vec<ObjectId> = state.players[player.0 as usize]
                .hand
                .iter()
                .copied()
                .filter(|id| *id != pending.object_id)
                .collect();
            return Ok(WaitingFor::DiscardForCost {
                player,
                count: count as usize,
                cards: eligible,
                pending_cast: Box::new(pending),
            });
        }
        AbilityCost::Mana { cost: mana_cost } => {
            // Add mana cost to the pending payment (handled by pay_and_push → pay_mana_cost)
            let combined = super::restrictions::add_mana_cost(&pending.cost, &mana_cost);
            return pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &combined,
                events,
            );
        }
        _ => {
            // Other cost types (Sacrifice, Exile, etc.) — not yet interactive
        }
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        events,
    )
}

pub(super) fn pay_and_push(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    pay_and_push_adventure(
        state, player, object_id, card_id, ability, cost, false, events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_and_push_adventure(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    cast_as_adventure: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Check for X in cost -- if present, return ManaPayment for player input
    if let crate::types::mana::ManaCost::Cost { shards, .. } = cost {
        if shards.contains(&ManaCostShard::X) {
            return Ok(WaitingFor::ManaPayment { player });
        }
    }

    super::casting::pay_mana_cost(state, player, object_id, cost, events)?;

    // Record commander cast before moving (need to check zone before move)
    let was_in_command_zone = state
        .objects
        .get(&object_id)
        .map(|obj| obj.zone == Zone::Command && obj.is_commander)
        .unwrap_or(false);

    // Emit targeting events before the spell moves to the stack
    emit_targeting_events(
        state,
        &flatten_targets_in_chain(&ability),
        object_id,
        player,
        events,
    );

    // Move card from hand/command zone to stack zone
    zones::move_to_zone(state, object_id, Zone::Stack, events);

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    // Push stack entry
    stack::push_to_stack(
        state,
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id,
                ability,
                cast_as_adventure,
            },
        },
        events,
    );

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
    });

    let obj = state
        .objects
        .get(&object_id)
        .expect("spell object still exists after stack push")
        .clone();
    restrictions::record_spell_cast(state, player, &obj);

    Ok(WaitingFor::Priority { player })
}

/// Find and mark the first unused land producing `needed` color. Returns true if found.
fn tap_matching_land(
    available: &[ManaSourceOption],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
    needed: ManaType,
) -> bool {
    let Some(option) = available
        .iter()
        .find(|option| option.mana_type == needed && !used_sources.contains(&option.object_id))
    else {
        return false;
    };

    used_sources.insert(option.object_id);
    to_tap.push(*option);
    true
}

/// Auto-tap untapped lands controlled by `player` to produce enough mana for `cost`.
///
/// Strategy: tap lands producing colors required by the cost first (colored shards),
/// then tap any remaining untapped lands for generic requirements.
///
/// `deprioritize_source` — if set, this land is tapped last (it's the permanent whose
/// activated ability we're paying for, so tapping other lands first is preferable UX).
/// Land-creatures are also deprioritized behind pure lands since they may block.
pub(super) fn auto_tap_lands(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
) {
    use crate::types::card_type::CoreType;
    use crate::types::mana::ManaCost;

    let (shards, generic) = match cost {
        ManaCost::NoCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    // Build list of activatable mana options for untapped lands this player controls,
    // sorted into tiers: pure lands first, then land-creatures, then the source permanent.
    let mut available: Vec<ManaSourceOption> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller != player || obj.tapped {
                return None;
            }
            if !obj.card_types.core_types.contains(&CoreType::Land) {
                return None;
            }
            Some(mana_sources::activatable_land_mana_options(
                state, oid, player,
            ))
        })
        .flatten()
        .collect();

    // Tier sort: 0 = pure land, 1 = land-creature, 2 = deprioritized source
    available.sort_by_key(|option| {
        if deprioritize_source == Some(option.object_id) {
            return 2;
        }
        let is_creature = state
            .objects
            .get(&option.object_id)
            .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature));
        if is_creature {
            1
        } else {
            0
        }
    });

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Phase 1: satisfy colored and hybrid shards by tapping matching lands
    let mut deferred_generic: usize = 0;
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
            }
            ShardRequirement::Hybrid(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                // Prefer 1 matching-color land over 2 generic lands
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, color) {
                    deferred_generic += 2;
                }
            }
            ShardRequirement::ColorlessHybrid(color) => {
                if !tap_matching_land(
                    &available,
                    &mut used_sources,
                    &mut to_tap,
                    ManaType::Colorless,
                ) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    // Phase 2: satisfy generic cost + deferred shards with any remaining untapped lands
    let mut remaining_generic = generic as usize + deferred_generic;
    for option in &available {
        if remaining_generic == 0 {
            break;
        }
        if used_sources.insert(option.object_id) {
            to_tap.push(*option);
            remaining_generic = remaining_generic.saturating_sub(1);
        }
    }

    // Phase 3: tap and produce mana
    // We bypass resolve_mana_ability here because auto-tap has already chosen
    // which color each source should produce (via ManaSourceOption.mana_type).
    // Resolving the raw ability would ignore that choice for AnyOneColor sources.
    for option in to_tap {
        if let Some(obj) = state.objects.get_mut(&option.object_id) {
            if !obj.tapped {
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped {
                    object_id: option.object_id,
                });
            }
        }
        mana_payment::produce_mana(state, option.object_id, option.mana_type, player, events);
    }
}
