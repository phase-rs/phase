use crate::game::costs::{self, PaymentFailure, PaymentOutcome};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::targeting::resolve_effect_player_ref;
use crate::game::{casting, casting_costs};
use crate::types::ability::{AbilityCost, Effect, QuantityExpr, QuantityRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PayableResource, WaitingFor};
use crate::types::mana::{ManaCost, ManaCostShard};
use crate::types::player::PlayerId;

use super::{EffectError, ResolvedAbility};

/// CR 107.1c + CR 107.14: Detect a "pay any amount of X" shape — the parser
/// emits `QuantityExpr::Ref { QuantityRef::Variable { name: "X" } }` for
/// these prompts (Galvanic Discharge, etc.). A fixed or dynamic reference
/// (e.g., `Fixed { 2 }` or `Power { CostPaidObject }`) is paid unconditionally.
fn is_pay_any_amount(amount: &QuantityExpr) -> bool {
    matches!(
        amount,
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if name == "X"
    )
}

/// CR 118.1: Pay a cost as part of an effect resolution.
/// CR 117.1: Mana payment uses auto-tap + pool deduction.
/// CR 119.4: Paying life IS losing life — replacement effects and the
/// CantLoseLife lock both apply, routed via `life_costs::pay_life_as_cost`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (cost, scale, payer_filter) = match &ability.effect {
        Effect::PayCost { cost, scale, payer } => (cost, scale, payer),
        _ => return Err(EffectError::MissingParam("PayCost".to_string())),
    };
    let Some(payer) = resolve_effect_player_ref(state, ability, payer_filter) else {
        state.cost_payment_failed_flag = true;
        return Ok(());
    };
    let mut payment_ability = ability.clone();
    payment_ability.controller = payer;

    // CR 118.1 + CR 118.5: Per-object scaled mana cost (was
    // `PaymentCost::ScaledMana`). `scale` is resolution-only metadata: the mana
    // `cost` base (which may carry colored pips) is multiplied by `times`; when
    // `times` resolves to 0 the scaled cost is `{0}` — paid trivially as a no-op
    // SUCCESS (the empty selection IS the acknowledgment), never a payment
    // failure. The concrete scaled `Mana` cost routes through the authority.
    if let Some(times) = scale {
        let AbilityCost::Mana { cost: base } = cost else {
            return Err(EffectError::InvalidParam(
                "PayCost.scale requires a Mana cost base".to_string(),
            ));
        };
        let times = resolve_quantity_with_targets(state, times, &payment_ability).max(0);
        let times = u32::try_from(times).unwrap_or(0);
        let scaled = scale_mana_cost(base, times);
        resolve_ability_cost_payment(
            state,
            &payment_ability,
            payer,
            &AbilityCost::Mana { cost: scaled },
            events,
        )?;
        return Ok(());
    }

    match cost {
        // CR 107.3a + CR 601.2b: A resolution-time mana cost with an unannounced
        // X (e.g. Well of Lost Dreams) prompts the payer for an amount before
        // payment. This X-prompt stays adapter-side — the authority pays a
        // concrete `Mana` cost only.
        AbilityCost::Mana { cost: mana_cost }
            if payment_ability.chosen_x.is_none() && casting_costs::cost_has_x(mana_cost) =>
        {
            let per_x = mana_x_shard_count(mana_cost);
            let max = max_resolution_mana_x_value(state, payer, ability.source_id, mana_cost);
            let max =
                trigger_event_amount_for_x_payment(state).map_or(max, |amount| max.min(amount));
            state.waiting_for = WaitingFor::PayAmountChoice {
                player: payer,
                resource: PayableResource::ManaGeneric { per_x },
                min: 0,
                max,
                accumulated: 0,
                source_id: ability.source_id,
                pending_mana_ability: None,
            };
        }
        // CR 107.1c + CR 107.14: "Pay any amount of {E}" — suspend the chain and
        // surface a `PayAmountChoice` prompt. The sub-ability continuation
        // machinery in `effects::mod` stashes the remainder of the chain; when
        // the player submits the chosen amount (see
        // `engine_resolution_choices::handle_resolution_choice`), the engine
        // deducts energy, records the paid amount on `last_effect_count` (the
        // fallback for `QuantityRef::EventContextAmount`), and drains the
        // continuation so the subsequent "that much damage" effect reads the
        // player's chosen value. This X-prompt stays adapter-side; the authority
        // pays a concrete `PayEnergy` amount only.
        AbilityCost::PayEnergy { amount } if is_pay_any_amount(amount) => {
            let max = state
                .players
                .iter()
                .find(|p| p.id == payer)
                .map(|p| p.energy)
                .unwrap_or(0);
            state.waiting_for = WaitingFor::PayAmountChoice {
                player: payer,
                resource: PayableResource::Energy,
                min: 0,
                max,
                accumulated: 0,
                source_id: ability.source_id,
                pending_mana_ability: None,
            };
        }
        // CR 119.4 + CR 118.3 + CR 119.8: "pay any amount of life" — suspend the
        // chain and surface a `PayAmountChoice` prompt (mirrors the energy "any
        // amount" arm above). `max` = payer's current life, clamped to 0 when a
        // CantLoseLife / can't-pay-life lock applies (can_pay_life_cost is false
        // for any amount > 0). On submit the engine deducts life via
        // `life_costs::pay_life_as_cost` and stamps `last_effect_count` so the
        // downstream "draw/look at that many" step reads the chosen amount.
        AbilityCost::PayLife { amount } if is_pay_any_amount(amount) => {
            // CR 119.4a + CR 810.9a: max payable for "pay any amount of life" is
            // the TEAM total; guard on team_life (a member may be individually
            // <= 0 while the team is positive, CR 810.9 — life loss lands on
            // each Player::life individually).
            let team_life = crate::game::players::team_life_total(state, payer);
            let max =
                if team_life > 0 && crate::game::life_costs::can_pay_life_cost(state, payer, 1) {
                    u32::try_from(team_life).unwrap_or(0)
                } else {
                    0
                };
            state.waiting_for = WaitingFor::PayAmountChoice {
                player: payer,
                resource: PayableResource::Life,
                min: 0,
                max,
                accumulated: 0,
                source_id: ability.source_id,
                pending_mana_ability: None,
            };
        }
        // CR 107.1c + CR 107.14: A fixed-amount energy payment routes through the
        // authority, then stamps `last_effect_count` so downstream chain steps
        // that reference `QuantityRef::EventContextAmount` (e.g. "deals that much
        // damage") read the paid value. The stamping is resolution-scope adapter
        // behavior the authority's `PayEnergy` arm does not carry.
        AbilityCost::PayEnergy { amount } => {
            let resolved = u32::try_from(
                resolve_quantity_with_targets(state, amount, &payment_ability).max(0),
            )
            .unwrap_or(0);
            let outcome =
                resolve_ability_cost_payment(state, &payment_ability, payer, cost, events)?;
            // Gate the stamp on THIS payment's outcome, not the global
            // `cost_payment_failed_flag` — a stale flag from an earlier effect
            // must not suppress the stamp for a payment that succeeded.
            if outcome == PaymentOutcome::Paid {
                state.last_effect_count = Some(resolved as i32);
            }
        }
        // All other resolution-time cost shapes (Mana without X, PayLife,
        // PaySpeed, Composite, Discard, …) route through the single payment
        // authority. CR 119.4: paying life IS losing life; CR 119.8: a
        // CantLoseLife lock therefore blocks the payment (the replacement
        // pipeline + lock both apply inside the authority's
        // `pay_life_as_cost`).
        _ => {
            resolve_ability_cost_payment(state, &payment_ability, payer, cost, events)?;
        }
    }
    Ok(())
}

/// CR 118.1: Multiply a `ManaCost` by `times` — every shard is repeated
/// `times` times and the generic component scaled. `times == 0` yields `{0}`
/// (`Cost { shards: [], generic: 0 }`), which the existing mana-payment path
/// treats as trivially paid.
fn scale_mana_cost(base: &ManaCost, times: u32) -> ManaCost {
    match base {
        ManaCost::NoCost
        | ManaCost::SelfManaCost
        | ManaCost::SelfManaValue
        | ManaCost::SelfManaCostReduced { .. } => ManaCost::zero(),
        ManaCost::Cost { shards, generic } => {
            let mut scaled_shards = Vec::with_capacity(shards.len() * times as usize);
            for _ in 0..times {
                scaled_shards.extend(shards.iter().copied());
            }
            ManaCost::Cost {
                shards: scaled_shards,
                generic: generic.saturating_mul(times),
            }
        }
    }
}

/// CR 118.12: Pay a resolution-time `AbilityCost` via the single payment
/// authority (`game::costs`). The duplicate
/// Mana/ManaDynamic/PayLife/PayEnergy/Composite/Discard payment arms that used
/// to live here were folded into `costs::pay_ability_cost_for_resolution`
/// (cost-payment unification, Phase 2); the resolution-time affordability match
/// that used to live here (`can_pay_resolution_ability_cost`, A3) was folded
/// into `costs::can_pay` with `PaymentScope::Resolution` (Phase 5).
///
/// CR 601.2h: only the multi-cost `Composite` shape is pre-gated through the
/// affordability authority — it is the one with cross-sub-cost atomicity
/// ("partial payments are not allowed"), so a `Composite` must never commit one
/// sub-cost before discovering a later sub-cost is unpayable. A *singleton* cost
/// needs no pre-gate: the authority's own internal pre-flight + `Failed` mapping
/// is exactly equivalent, and pre-gating it would re-run the board-scale auto-tap
/// planner and re-resolve the `QuantityExpr` a second time (Phase 4 deferred
/// perf fix). The authority's outcome maps to the resolution-scope failure
/// channel (`cost_payment_failed_flag`).
fn resolve_ability_cost_payment(
    state: &mut GameState,
    ability: &ResolvedAbility,
    payer: PlayerId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EffectError> {
    if matches!(cost, AbilityCost::Composite { .. })
        && !costs::can_pay(
            state,
            payer,
            ability.source_id,
            cost,
            &costs::PaymentScope::Resolution { ability },
        )
    {
        state.cost_payment_failed_flag = true;
        return Ok(PaymentOutcome::Failed {
            reason: PaymentFailure {
                reason: "resolution-time cost not affordable (pre-gate)".to_string(),
            },
        });
    }
    match costs::pay_ability_cost_for_resolution(state, payer, cost, ability, events) {
        Ok(outcome @ PaymentOutcome::Paid) => Ok(outcome),
        // CR 616.1: a replacement-effect choice — or an interactive
        // `DiscardChoice` — interrupted payment. `state.waiting_for` is already
        // set by the authority; the resolution chain resumes from there.
        Ok(PaymentOutcome::Paused { remaining_cost }) => {
            if let Some(remaining_cost) = remaining_cost.clone() {
                super::prepend_remaining_pay_cost_continuation(
                    state,
                    ability,
                    payer,
                    remaining_cost,
                );
            }
            Ok(PaymentOutcome::Paused { remaining_cost })
        }
        Ok(outcome @ PaymentOutcome::Failed { .. }) => {
            state.cost_payment_failed_flag = true;
            Ok(outcome)
        }
        // An engine invariant violation (e.g. missing source object) surfaces
        // as an effect error rather than a silent payment failure.
        Err(e) => Err(EffectError::InvalidParam(format!(
            "resolution-time cost payment failed: {e:?}"
        ))),
    }
}

fn mana_x_shard_count(cost: &ManaCost) -> u32 {
    match cost {
        ManaCost::Cost { shards, .. } => shards
            .iter()
            .filter(|shard| matches!(shard, ManaCostShard::X))
            .count() as u32,
        ManaCost::NoCost
        | ManaCost::SelfManaCost
        | ManaCost::SelfManaValue
        | ManaCost::SelfManaCostReduced { .. } => 0,
    }
}

fn max_resolution_mana_x_value(
    state: &GameState,
    payer: PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    cost: &ManaCost,
) -> u32 {
    // Resolution-time X costs are not spell casts — convoke/improvise/waterbend
    // tap-payments do not apply, so no spell object is passed.
    let mut max = casting_costs::max_x_value(state, payer, cost, None);
    loop {
        let mut concrete = cost.clone();
        concrete.concretize_x(max);
        if casting::can_pay_effect_mana_cost_after_auto_tap(state, payer, source_id, &concrete) {
            return max;
        }
        if max == 0 {
            return 0;
        }
        max -= 1;
    }
}

fn trigger_event_amount(state: &GameState) -> Option<u32> {
    state
        .current_trigger_event
        .as_ref()
        .and_then(crate::game::targeting::extract_amount_from_event)
        .and_then(|amount| u32::try_from(amount.max(0)).ok())
}

/// CR 107.3i + CR 508.1m: Only event amounts that bound pay-{X} via an explicit
/// comparator where-X clause (Well of Lost Dreams class) may cap X announcement.
/// `AttackersDeclared` exposes attacker *count* for "that many" effects, not as
/// a pay-{X} cap — using it capped Elenda and Azor at X=1 (#4226).
fn trigger_event_amount_for_x_payment(state: &GameState) -> Option<u32> {
    let event = state.current_trigger_event.as_ref()?;
    match event {
        GameEvent::AttackersDeclared { .. } => None,
        _ => trigger_event_amount(state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ManaProduction, ManaSpendRestriction, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaCostShard, ManaRestriction, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(effect: Effect) -> ResolvedAbility {
        ResolvedAbility::new(effect, vec![], ObjectId(1), PlayerId(0))
    }

    fn create_colorless_source(
        state: &mut GameState,
        card_id: CardId,
        name: &str,
        restrictions: Vec<ManaSpendRestriction>,
    ) -> ObjectId {
        let source = create_object(
            state,
            card_id,
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions,
                    grants: Vec::new(),
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        source
    }

    #[test]
    fn mana_payment_deducts_from_pool() {
        let mut state = GameState::new_two_player(42);
        // Give player 0 three colorless mana
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana { cost },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
    }

    #[test]
    fn mana_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        // Player 0 has empty mana pool (default)
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana { cost },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
    }

    /// CR 119.4a + CR 810.9a: "pay any amount of life" surfaces a
    /// `PayAmountChoice` whose `max` is the payer's TEAM total in 2HG. Payer P0
    /// individually at -2, teammate P1 at 9 → team total 7, so `max == 7` even
    /// though the payer's individual life is negative (a member may be below 0
    /// while the team is positive, CR 810.9). Reverting Site 9 to the
    /// individual `p.life` read (-2, not > 0) would set `max == 0`.
    #[test]
    fn pay_any_amount_life_max_is_team_total_in_2hg() {
        let mut state =
            GameState::new(crate::types::format::FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = -2;
        state.players[1].life = 9; // team total 7

        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                resource,
                max,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert!(matches!(resource, PayableResource::Life));
                assert_eq!(*max, 7, "max payable is the team total (7), not 0");
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
    }

    /// Off-team degeneracy sibling for Site 9: in a 1v1 the max is the payer's
    /// own life (5).
    #[test]
    fn pay_any_amount_life_max_off_team_is_individual() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 5;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        assert!(resolve(&mut state, &ability, &mut events).is_ok());
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(*max, 5),
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
    }

    #[test]
    fn direct_resolution_mana_payment_rejects_activation_only_mana() {
        let mut state = GameState::new_two_player(42);
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForActivation],
            grants: vec![],
            expiry: None,
        });
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 1);
    }

    #[test]
    fn resolution_mana_payment_auto_tap_skips_activation_only_source() {
        let mut state = GameState::new_two_player(42);
        let restricted = create_colorless_source(
            &mut state,
            CardId(10),
            "Restricted Source",
            vec![ManaSpendRestriction::ActivateOnly],
        );
        let unrestricted =
            create_colorless_source(&mut state, CardId(11), "Unrestricted Source", Vec::new());
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert!(!state.objects.get(&restricted).unwrap().tapped);
        assert!(state.objects.get(&unrestricted).unwrap().tapped);
    }

    #[test]
    fn resolution_mana_pay_amount_choice_max_rejects_activation_only_sources() {
        let mut state = GameState::new_two_player(42);
        let restricted = create_colorless_source(
            &mut state,
            CardId(10),
            "Restricted Source",
            vec![ManaSpendRestriction::ActivateOnly],
        );
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::X],
                    generic: 0,
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(max, 0),
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
        assert!(!state.objects.get(&restricted).unwrap().tapped);
    }

    #[test]
    fn life_payment_deducts_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 17);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged { player_id, amount }
                if *player_id == PlayerId(0) && *amount == -3
        )));
    }

    #[test]
    fn life_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 2;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 2); // No change
    }

    /// CR 118.12: A resolution-time `PayCost { AbilityCost::PayLife }` routes
    /// through `costs::pay_ability_cost_for_resolution` (the unified authority,
    /// Phase 2) rather than the deleted in-arm duplicate. The visible behavior
    /// — life deducted, `LifeChanged` event, no failure flag — must be
    /// identical to the pre-Phase-2 implementation.
    #[test]
    fn resolution_ability_pay_life_routes_through_authority() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 4 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 16);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged { player_id, amount }
                if *player_id == PlayerId(0) && *amount == -4
        )));
    }

    /// CR 118.3 + CR 601.2h: An unaffordable resolution-time
    /// `PayCost { AbilityCost::PayLife }` reports failure via
    /// `cost_payment_failed_flag` and commits no partial payment — the
    /// authority's `Failed` outcome maps to the resolution failure channel.
    #[test]
    fn resolution_ability_pay_life_failure_maps_to_flag() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 3;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 4 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 3);
    }

    #[test]
    fn composite_mana_and_life_payment_pays_both_costs() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
        });
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 1,
                        },
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                    },
                ],
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert_eq!(state.players[0].life, 17);
    }

    #[test]
    fn resolution_time_ability_mana_cost_rejects_activation_only_mana() {
        let mut state = GameState::new_two_player(42);
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForActivation],
            grants: vec![],
            expiry: None,
        });
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 1);
    }

    #[test]
    fn resolution_time_dynamic_mana_cost_pays_resolved_amount() {
        let mut state = GameState::new_two_player(42);
        for _ in 0..2 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Fixed { value: 2 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
    }

    #[test]
    fn composite_mana_and_life_payment_prechecks_before_mutating() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 2;
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
        });
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 1,
                        },
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                    },
                ],
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].mana_pool.mana.len(), 1);
        assert_eq!(state.players[0].life, 2);
    }

    /// CR 118.3 + CR 119.4 + CR 107.14: Composite of `PayLife` and `PayEnergy`.
    /// Pre-H3 fix: `can_pay_resolution_ability_cost` had a `_ => false` arm that
    /// silently rejected `PayEnergy`, causing the composite to fail even when
    /// the player had both 1 life and 1 energy.
    #[test]
    fn composite_life_and_energy_payment_pays_both_costs() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 5;
        state.players[0].energy = 3;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    AbilityCost::PayEnergy {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 4);
        assert_eq!(state.players[0].energy, 2);
    }

    /// CR 118.3: Composite of `PayLife` + `PayEnergy` fails when the energy
    /// component is unaffordable, and the pre-flight check prevents the life
    /// portion from being committed (no partial payment).
    #[test]
    fn composite_life_and_energy_payment_fails_when_energy_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 5;
        state.players[0].energy = 0;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                    AbilityCost::PayEnergy {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.cost_payment_failed_flag);
        // CR 118.3: pre-flight rejected the composite — life total is unchanged.
        assert_eq!(state.players[0].life, 5);
        assert_eq!(state.players[0].energy, 0);
    }

    /// CR 107.14: `AbilityCost::PayEnergy` carries a `QuantityExpr` amount.
    /// A `Fixed` amount deducts when affordable and trips
    /// `cost_payment_failed_flag` when the payer lacks enough energy — the
    /// building-block contract for the widened field.
    #[test]
    fn resolution_pay_energy_fixed_amount_pays_when_affordable() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 5;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 5 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 0);
    }

    /// A stale `cost_payment_failed_flag` left by an EARLIER effect must not
    /// suppress the `last_effect_count` stamp for an energy payment that
    /// succeeds — the stamp is gated on this payment's own `PaymentOutcome`,
    /// not the global flag.
    #[test]
    fn resolution_pay_energy_stamps_count_despite_stale_failed_flag() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 5;
        state.cost_payment_failed_flag = true;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 5 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.players[0].energy, 0);
        assert_eq!(state.last_effect_count, Some(5));
    }

    #[test]
    fn resolution_pay_energy_fixed_amount_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 4;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 5 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.cost_payment_failed_flag);
        // No partial payment — energy unchanged.
        assert_eq!(state.players[0].energy, 4);
        // CR 118.12: an unpaid cost must not feed "that much" downstream.
        assert_eq!(state.last_effect_count, None);
    }

    #[test]
    fn energy_payment_deducts_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 3;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 2 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EnergyChanged { player, delta }
                if *player == PlayerId(0) && *delta == -2
        )));
    }

    #[test]
    fn energy_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 1;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 2 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 1); // No change
    }

    #[test]
    fn ability_cost_discard_payment_enters_discard_choice() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(10), PlayerId(0), "A".into(), Zone::Hand);
        let second = create_object(&mut state, CardId(11), PlayerId(0), "B".into(), Zone::Hand);
        let ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                effect_kind,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*effect_kind, crate::types::ability::EffectKind::PayCost);
                assert!(cards.contains(&first));
                assert!(cards.contains(&second));
            }
            other => panic!("expected DiscardChoice, got {other:?}"),
        }
    }

    #[test]
    fn composite_discard_choice_does_not_pay_following_cost_before_choice() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let first = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(10),
            PlayerId(0),
            "A".into(),
            crate::types::zones::Zone::Hand,
        );
        let second = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(11),
            PlayerId(0),
            "B".into(),
            crate::types::zones::Zone::Hand,
        );
        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 3 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(gain_life));
        let mut events = Vec::new();

        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice { cards, .. } => {
                assert!(cards.contains(&first));
                assert!(cards.contains(&second));
            }
            other => panic!("expected DiscardChoice, got {other:?}"),
        }
        assert_eq!(
            state.players[0].life, 20,
            "following PayLife component must not be paid before discard choice"
        );

        let waiting_for = state.waiting_for.clone();
        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting_for,
            crate::types::actions::GameAction::SelectCards { cards: vec![first] },
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].life, 22,
            "after the discard choice, the remaining PayLife cost must run before the rider"
        );
    }

    /// CR 118.3 + CR 601.2h: a resolution-time cost shape the authority cannot
    /// execute (random discard is not auto-payable) must fail the payment —
    /// never silently report `Paid` while discarding nothing. Discriminates the
    /// Resolution-scope guard on the authority's interactive pass-through arm:
    /// without it, this shape falls through to `Paid` with the hand untouched
    /// and the flag unset.
    #[test]
    fn ability_cost_random_discard_fails_instead_of_silent_paid() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(10), PlayerId(0), "A".into(), Zone::Hand);
        let second = create_object(&mut state, CardId(11), PlayerId(0), "B".into(), Zone::Hand);
        let ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Random,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.cost_payment_failed_flag,
            "unexecutable resolution cost shape must set the failed flag, not fake Paid"
        );
        assert_eq!(
            state.objects[&first].zone,
            Zone::Hand,
            "no card may be discarded by a failed payment"
        );
        assert_eq!(state.objects[&second].zone, Zone::Hand);
    }

    #[test]
    fn ability_cost_discard_choice_drains_continuation() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(10), PlayerId(0), "A".into(), Zone::Hand);
        create_object(&mut state, CardId(11), PlayerId(0), "B".into(), Zone::Hand);
        state.players[0].life = 20;

        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut pay_ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        pay_ability.sub_ability = Some(Box::new(gain_life));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay_ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DiscardChoice { .. }
        ));

        let waiting_for = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SelectCards { cards: vec![first] },
            &mut events,
        )
        .unwrap();

        assert!(matches!(
            outcome,
            ResolutionChoiceOutcome::WaitingFor(_)
                | ResolutionChoiceOutcome::WaitingForWithInlineTriggers(_)
                | ResolutionChoiceOutcome::ActionResult(_)
        ));
        assert_eq!(state.players[0].life, 23);
        assert_eq!(state.last_effect_count, Some(1));
    }

    /// CR 107.14: A fixed-amount energy payment stamps `last_effect_count`
    /// so downstream chain steps like "deals that much damage" can read the
    /// paid value through `QuantityRef::EventContextAmount`.
    #[test]
    fn energy_payment_stamps_last_effect_count() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 5;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.last_effect_count, Some(3));
    }

    /// CR 107.1c + CR 107.14: "Pay any amount of {E}" — the resolver pauses
    /// on a `PayAmountChoice` prompt with `max` bounded by the player's
    /// current energy. No energy is deducted until `SubmitPayAmount` fires.
    #[test]
    fn pay_any_amount_of_energy_pauses_for_choice() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 3;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                resource,
                min,
                max,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*resource, PayableResource::Energy);
                assert_eq!(*min, 0);
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }
        assert_eq!(
            state.players[0].energy, 3,
            "energy must not be deducted yet"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::EnergyChanged { .. })),
            "no EnergyChanged event until the player commits an amount"
        );
    }

    /// CR 107.1c + CR 107.14 + CR 603.7c: Galvanic Discharge chain shape —
    /// GainEnergy(3) → PayCost{Energy, Variable X} → DealDamage{EventContextAmount}.
    /// The player picks 2 out of 3 energy; damage equals the chosen amount.
    #[test]
    fn pay_any_amount_then_deal_that_much_damage_flow() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::ability::{QuantityExpr, QuantityRef, TargetFilter, TargetRef};
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        // Target creature owned by player 1.
        let target_id = create_object(
            &mut state,
            CardId(777),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        let target = state.objects.get_mut(&target_id).unwrap();
        target.card_types.core_types.push(CoreType::Creature);
        target.toughness = Some(2);
        target.power = Some(2);

        // Player 0 starts with 3 energy (after a prior GainEnergy step in the chain).
        state.players[0].energy = 3;

        // PayCost { Energy, Variable("X") } followed by DealDamage { EventContextAmount, target }.
        let damage = ResolvedAbility::new(
            Effect::DealDamage {
                damage_source: None,
                excess: None,
                target: TargetFilter::Any,
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(500),
            PlayerId(0),
        );
        let mut pay_ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(500),
            PlayerId(0),
        );
        pay_ability.sub_ability = Some(Box::new(damage));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay_ability, &mut events, 0).unwrap();

        // Chain paused on PayAmountChoice.
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(*max, 3),
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }

        // Player commits 2.
        let wf = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            wf,
            GameAction::SubmitPayAmount { amount: 2 },
            &mut events,
        )
        .unwrap();
        match outcome {
            ResolutionChoiceOutcome::WaitingFor(_) => {}
            ResolutionChoiceOutcome::WaitingForWithInlineTriggers(_) => {}
            ResolutionChoiceOutcome::ActionResult(_) => {}
        }

        assert_eq!(state.players[0].energy, 1, "two energy consumed");
        assert_eq!(
            state.objects.get(&target_id).map(|o| o.damage_marked),
            Some(2),
            "Galvanic Discharge dealt 2 damage (the chosen amount)"
        );
    }

    #[test]
    fn pay_x_mana_from_life_gain_trigger_draws_chosen_x_cards() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        });

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::ManaGeneric { per_x: 1 });
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }

        let waiting_for = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SubmitPayAmount { amount: 2 },
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            ResolutionChoiceOutcome::WaitingFor(_)
                | ResolutionChoiceOutcome::WaitingForWithInlineTriggers(_)
                | ResolutionChoiceOutcome::ActionResult(_)
        ));
        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[0].mana_pool.mana.len(), 1);
    }

    /// CR 603.2 + CR 603.5 + CR 107.3a + CR 608.2c + CR 119.3 + CR 121.1:
    /// Well of Lost Dreams production-shape end-to-end. Triggered ability shape
    /// `optional PayCost { Mana { X } } → IfYouDo SequentialSibling Draw { X }`
    /// where X is capped by the life-gained amount (CR 118.1). Reproduces issue
    /// #270: clicking "Yes" then submitting X=3 must (a) deduct 3 mana,
    /// (b) draw 3 cards, (c) leave no residual `waiting_for`. The existing
    /// `pay_x_mana_from_life_gain_trigger_draws_chosen_x_cards` test
    /// intentionally elides the `optional=true` + `IfYouDo` + `SequentialSibling`
    /// wrappers, so it cannot catch regressions in the optional-Yes →
    /// PayAmountChoice → continuation glue path.
    #[test]
    fn pay_x_optional_may_pay_with_if_you_do_draw_full_chain() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink};
        use crate::types::actions::GameAction;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        // CR 121.1: Five library cards available to draw.
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        // CR 107.3a: Five generic mana to spend (X is capped by life gained,
        // not by mana — verifying max=3 not max=5 distinguishes link (b) from
        // link (a) in the four-link diagnostic ladder).
        for _ in 0..5 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        // CR 119.3: Three life gained — this is the `EventContextAmount` cap.
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        });

        // CR 608.2c: Build the IfYouDo SequentialSibling Draw rider — exact
        // production AST shape from `card-data.json` for Well of Lost Dreams.
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        // CR 603.5 + CR 107.3a: Outer optional PayCost{Mana{X}} with the
        // IfYouDo Draw attached as `sub_ability`.
        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();

        // Step A: resolve_ability_chain → OptionalEffectChoice.
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice (link a — optional prompt), got {:?}",
            state.waiting_for
        );

        // Step B: Accept "Yes" → PayAmountChoice with max=3 (capped by
        // life-gained=3, NOT mana=5). Fingerprint distinguishes:
        //   max=5  → trigger_event_amount() lost the cap (link b);
        //   max=3  → cap intact, continue;
        //   Priority → cost_has_x not detected (link b alt).
        let waiting = handle_optional_effect_choice(&mut state, true, &mut events).unwrap();
        match &waiting {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::ManaGeneric { per_x: 1 });
                assert_eq!(
                    *max, 3,
                    "PayAmountChoice max must be capped by life gained (3), got {max} \
                     — regression in trigger_event_amount cap or X detection"
                );
            }
            other => panic!(
                "expected PayAmountChoice after Yes (link c — deferred-stash sub_ability), \
                 got {other:?}"
            ),
        }

        // Step C: pending_continuation must hold the Draw rider with
        // optional_effect_performed=true. Link (c) regression: continuation None.
        // Link (d) regression: continuation present but performed flag false.
        let cont = state
            .pending_continuation
            .as_ref()
            .expect("pending_continuation must be stashed (link c)");
        assert!(
            matches!(cont.chain.effect, Effect::Draw { .. }),
            "pending continuation must wrap the Draw rider, got {:?}",
            cont.chain.effect
        );
        assert!(
            cont.chain.context.optional_effect_performed,
            "pending_continuation.chain.context.optional_effect_performed must be true \
             after Yes (link d — set_optional_effect_performed_recursive regression)"
        );

        // Step D: Submit X=3 → 3 mana spent, 3 cards drawn, no residue.
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitPayAmount { amount: 3 },
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.players[0].hand.len(),
            3,
            "IfYouDo Draw{{X=3}} must draw 3 cards after Yes + Submit 3"
        );
        assert_eq!(
            state.players[0].mana_pool.mana.len(),
            2,
            "3 of 5 mana must be spent on X cost"
        );
        // (c) No residual choice: the chain fully resolved back to priority.
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "after the Draw resolves, no residual waiting_for must remain, got {:?}",
            state.waiting_for
        );
    }

    /// CR 608.2c: Declining the outer "may pay {X}" leaves the
    /// `IfYouDo`-gated Draw sub-ability with `optional_effect_performed=false`;
    /// the gate evaluates false and nothing happens — no mana spent, no cards
    /// drawn, no residual `waiting_for`.
    #[test]
    fn pay_x_optional_may_pay_decline_does_not_draw_or_pay() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink};
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        for _ in 0..5 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        });

        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));

        handle_optional_effect_choice(&mut state, false, &mut events).unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            0,
            "decline must not draw any cards"
        );
        assert_eq!(
            state.players[0].mana_pool.mana.len(),
            5,
            "decline must not spend any mana"
        );
    }

    /// CR 107.3a: Accepting the optional, then submitting amount=0, is a
    /// legal payment of zero mana (the controller chooses X for the {X} cost,
    /// and 0 is legal since the trigger sets no positive lower bound). The `IfYouDo` Draw fires — the effect WAS
    /// performed (the optional was accepted and the X cost was satisfied at
    /// zero mana) — but with X=0 the Draw quantity is 0, so no cards are
    /// drawn. The performed signal must NOT be conflated with "X > 0".
    #[test]
    fn pay_x_optional_may_pay_submit_zero_pays_zero_draws_zero() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink};
        use crate::types::actions::GameAction;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        for _ in 0..5 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        });

        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        let waiting = handle_optional_effect_choice(&mut state, true, &mut events).unwrap();
        let _ = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitPayAmount { amount: 0 },
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[0].hand.len(),
            0,
            "X=0 must draw 0 cards (the Draw count IS X)"
        );
        assert_eq!(
            state.players[0].mana_pool.mana.len(),
            5,
            "X=0 must spend 0 mana"
        );
    }

    /// CR 118.1 + CR 107.3a: PayAmountChoice `max` for the Well of Lost
    /// Dreams shape is the MIN of (player mana, life-gained). With 10 mana
    /// and only 2 life gained, max must be 2 — not 10. This pins the
    /// trigger-event-amount cap independently from the X-payable cap, so a
    /// regression that drops the event cap surfaces as max=10 here even when
    /// the basic mana check still passes.
    #[test]
    fn trigger_event_amount_for_x_payment_ignores_attackers_declared_count() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![ObjectId(99)],
            defending_player: PlayerId(1),
            attacks: vec![],
        });
        assert_eq!(
            trigger_event_amount_for_x_payment(&state),
            None,
            "attack-batch attacker count must not cap pay-{{X}} (#4226)"
        );
        assert_eq!(
            trigger_event_amount(&state),
            Some(1),
            "extract_amount_from_event still exposes attacker count for other readers"
        );
    }

    #[test]
    fn pay_x_optional_max_capped_by_event_amount_not_player_mana() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink};
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        for n in 0..3 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        // Plenty of mana — 10 generic available.
        for _ in 0..10 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        // Only 2 life gained.
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 2,
        });

        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        handle_optional_effect_choice(&mut state, true, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(
                *max, 2,
                "max must be capped by life gained (2), NOT player mana (10)"
            ),
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
    }

    #[test]
    fn player_scope_pay_any_mana_accumulates_chosen_x_for_tail() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Join Forces Source".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        for _ in 0..2 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        for _ in 0..3 {
            state.players[1].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.player_scope = Some(crate::types::ability::PlayerFilter::All);
        pay.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                max,
                accumulated,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*max, 2);
                assert_eq!(*accumulated, 0);
            }
            other => panic!("expected first PayAmountChoice, got {other:?}"),
        }

        let waiting_for = state.waiting_for.clone();
        handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SubmitPayAmount { amount: 2 },
            &mut events,
        )
        .unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                max,
                accumulated,
                ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(*max, 3);
                assert_eq!(*accumulated, 2);
            }
            other => panic!("expected second PayAmountChoice, got {other:?}"),
        }

        let waiting_for = state.waiting_for.clone();
        handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SubmitPayAmount { amount: 1 },
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].hand.len(), 3);
        assert_eq!(state.players[0].mana_pool.mana.len(), 0);
        assert_eq!(state.players[1].mana_pool.mana.len(), 2);
    }

    /// CR 107.1c: "Pay any amount" with zero energy still pauses with
    /// `max = 0` — the player can only pick 0 (the "may" branch).
    #[test]
    fn pay_any_amount_with_zero_energy_max_is_zero() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 0;
        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(*max, 0),
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }
    }

    /// CR 119.8: An `Effect::PayCost { Life }` under CantLoseLife is unpayable —
    /// `cost_payment_failed_flag` is set and life total does not change.
    #[test]
    fn life_payment_blocked_by_cant_lose_life() {
        use crate::game::zones::create_object;
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::identifiers::CardId;
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

        let ability = make_ability(Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);

        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 20, "life total must not change");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no LifeChanged event should be emitted"
        );
    }

    // -----------------------------------------------------------------------
    // Join Forces — CR 101.4 + CR 800.4 + CR 107.3i + CR 117.1 + CR 121.1
    //
    // Multi-player payment loop:
    //   * Each player in turn order (starting with the controller) is prompted
    //     to pay any amount of mana via `PayAmountChoice`.
    //   * The accumulated total threads through as `chosen_x` on the chained
    //     sub-effect so `Variable("X")` resolves to the total at resolution.
    //   * `state.last_effect_count` matches the running total (read by
    //     `QuantityRef::EventContextAmount` for "that much" patterns).
    // -----------------------------------------------------------------------

    /// Builds the join-forces resolution chain that the parser produces for
    /// Minds Aglow: `PayCost { Mana { X } }` with `player_scope: All` and
    /// `starting_with: Some(You)`, threading into `Draw { Variable("X") }`.
    fn build_minds_aglow_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        use crate::types::ability::{ControllerRef, PlayerFilter};
        use crate::types::mana::ManaCostShard;

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        pay.player_scope = Some(PlayerFilter::All);
        pay.starting_with = Some(ControllerRef::You);
        // The sub_ability inherits player_scope: All from the parser; mirror
        // that so each iteration's draw is scoped per-player.
        let mut draw = draw;
        draw.player_scope = Some(PlayerFilter::All);
        pay.sub_ability = Some(Box::new(draw));
        pay
    }

    /// Add `n` colorless mana to `player`'s mana pool.
    fn add_colorless(state: &mut GameState, player: PlayerId, n: u32) {
        for _ in 0..n {
            state.players[player.0 as usize].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
    }

    /// Create `n` library cards owned by `player` so Draw has cards to draw.
    fn seed_library(state: &mut GameState, player: PlayerId, n: u32) {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;
        // Use a per-player CardId base offset so libraries don't collide.
        let base: u64 = 1000 + 100 * (player.0 as u64);
        for i in 0..n {
            create_object(
                state,
                CardId(base + i as u64),
                player,
                format!("P{} Card {i}", player.0),
                Zone::Library,
            );
        }
    }

    /// 4-player Minds Aglow with P0=controller, varied payments per player.
    /// P0 pays 3, P1 pays 2, P2 pays 1, P3 pays 0. The total X = 6 should
    /// flow through to each player's Draw, so all four players draw 6 cards.
    /// CR 107.3i: X has one value per resolution.
    #[test]
    fn minds_aglow_four_player_loop_each_draws_total() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::commander(), 4, 42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Minds Aglow".to_string(),
            Zone::Battlefield,
        );
        // 10 library cards per player so Draw 6 is safe.
        for pid in 0..4 {
            seed_library(&mut state, PlayerId(pid), 10);
        }
        // Mana budgets: P0=3, P1=2, P2=1, P3=0.
        add_colorless(&mut state, PlayerId(0), 3);
        add_colorless(&mut state, PlayerId(1), 2);
        add_colorless(&mut state, PlayerId(2), 1);

        let pay = build_minds_aglow_chain(source_id, PlayerId(0));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();

        // First prompt: P0 (starting_with=You overrides APNAP).
        let payments: &[(PlayerId, u32)] = &[
            (PlayerId(0), 3),
            (PlayerId(1), 2),
            (PlayerId(2), 1),
            (PlayerId(3), 0),
        ];
        for (expected_player, amount) in payments {
            match &state.waiting_for {
                WaitingFor::PayAmountChoice { player, .. } => {
                    assert_eq!(*player, *expected_player, "wrong player prompted");
                }
                other => panic!("expected PayAmountChoice for {expected_player:?}, got {other:?}"),
            }
            let waiting_for = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                waiting_for,
                GameAction::SubmitPayAmount { amount: *amount },
                &mut events,
            )
            .unwrap();
        }

        // All four players draw 6 cards each (= total X across the loop).
        // CR 107.3i: every Variable("X") in the chain resolves to the same
        // accumulated total.
        for pid in 0..4 {
            assert_eq!(
                state.players[pid].hand.len(),
                6,
                "Player {} should have drawn 6 cards (total mana paid = 6)",
                pid
            );
        }
    }

    /// CR 101.4 + CR 800.4: "Starting with you" must override APNAP. In this
    /// test the active player is P2 but the spell is cast by P0; the prompt
    /// order is P0 → P1 → P2 (controller first), NOT P2 → P0 → P1.
    #[test]
    fn minds_aglow_starting_with_you_overrides_apnap() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::commander(), 3, 7);
        state.active_player = PlayerId(2);
        let source_id = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Minds Aglow".to_string(),
            Zone::Battlefield,
        );
        for pid in 0..3 {
            seed_library(&mut state, PlayerId(pid), 5);
        }
        // Everyone pays 0 for simplicity — we only care about prompt order.
        // 0 mana available is enough since pay-any-amount allows min=0.

        let pay = build_minds_aglow_chain(source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();

        // Expected prompt sequence under "starting with you": P0, P1, P2.
        // (Without the override the order would be P2, P0, P1.)
        let expected_sequence: &[PlayerId] = &[PlayerId(0), PlayerId(1), PlayerId(2)];
        for expected_player in expected_sequence {
            match &state.waiting_for {
                WaitingFor::PayAmountChoice { player, .. } => {
                    assert_eq!(
                        *player, *expected_player,
                        "prompt order violated CR 101.4 + CR 800.4 (starting_with override)"
                    );
                }
                other => panic!("expected PayAmountChoice for {expected_player:?}, got {other:?}"),
            }
            let waiting_for = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                waiting_for,
                GameAction::SubmitPayAmount { amount: 0 },
                &mut events,
            )
            .unwrap();
        }

        // CR 121.1: Drawing 0 cards is a legal no-op. All players should
        // have empty hands.
        for pid in 0..3 {
            assert_eq!(
                state.players[pid].hand.len(),
                0,
                "Player {} should have drawn 0 cards (no mana paid)",
                pid
            );
        }
    }

    /// CR 121.1: Drawing 0 cards (and `Variable("X") = 0`) is a legal no-op.
    /// 4-player game where everyone pays 0 — no errors, all hands empty.
    #[test]
    fn minds_aglow_refusal_path_zero_total_zero_draws() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::commander(), 4, 11);
        let source_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Minds Aglow".to_string(),
            Zone::Battlefield,
        );
        for pid in 0..4 {
            seed_library(&mut state, PlayerId(pid), 3);
        }

        let pay = build_minds_aglow_chain(source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();

        for _ in 0..4 {
            assert!(matches!(
                state.waiting_for,
                WaitingFor::PayAmountChoice { .. }
            ));
            let waiting_for = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                waiting_for,
                GameAction::SubmitPayAmount { amount: 0 },
                &mut events,
            )
            .unwrap();
        }

        for pid in 0..4 {
            assert_eq!(
                state.players[pid].hand.len(),
                0,
                "Player {} should have drawn 0 cards on refusal path",
                pid
            );
        }
    }

    /// 4-player game where only the caster (P0) has mana. P0 pays 5, the
    /// other three pay 0. Total X = 5 → all four players draw 5 cards.
    #[test]
    fn minds_aglow_caster_alone_pays_all() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::commander(), 4, 13);
        let source_id = create_object(
            &mut state,
            CardId(1100),
            PlayerId(0),
            "Minds Aglow".to_string(),
            Zone::Battlefield,
        );
        for pid in 0..4 {
            seed_library(&mut state, PlayerId(pid), 10);
        }
        // Only P0 has mana; P1/P2/P3 pay 0.
        add_colorless(&mut state, PlayerId(0), 5);

        let pay = build_minds_aglow_chain(source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();

        // P0 pays 5, others pay 0.
        let payments: &[u32] = &[5, 0, 0, 0];
        for amount in payments {
            assert!(matches!(
                state.waiting_for,
                WaitingFor::PayAmountChoice { .. }
            ));
            let waiting_for = state.waiting_for.clone();
            handle_resolution_choice(
                &mut state,
                waiting_for,
                GameAction::SubmitPayAmount { amount: *amount },
                &mut events,
            )
            .unwrap();
        }

        for pid in 0..4 {
            assert_eq!(
                state.players[pid].hand.len(),
                5,
                "Player {} should have drawn 5 cards (total mana paid = 5)",
                pid
            );
        }
    }

    // CR 118.1: ScaledMana base-cost multiplication — colored-pip and
    // generic-only bases scale uniformly; `times == 0` yields `{0}`.
    #[test]
    fn scale_mana_cost_repeats_colored_pip() {
        // Thelon's Curse: {U} × 3 → {U}{U}{U}.
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
        let scaled = scale_mana_cost(&base, 3);
        assert_eq!(
            scaled,
            ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue
                ],
                generic: 0,
            }
        );
    }

    #[test]
    fn scale_mana_cost_scales_generic() {
        // Magnetic Mountain: {4} × 2 → {8}.
        let scaled = scale_mana_cost(&ManaCost::generic(4), 2);
        assert_eq!(scaled, ManaCost::generic(8));
    }

    #[test]
    fn scale_mana_cost_zero_times_is_empty() {
        // CR 118.5: {4} × 0 → {0} — trivially paid (no resources required).
        let scaled = scale_mana_cost(&ManaCost::generic(4), 0);
        assert_eq!(scaled, ManaCost::zero());
        // Colored base × 0 also collapses to {0}.
        let colored = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
        assert_eq!(scale_mana_cost(&colored, 0), ManaCost::zero());
    }

    /// CR 119.4 + CR 118.3: Install a CantLoseLife permanent for `owner`, used
    /// to verify the "pay any amount of life" prompt clamps `max` to 0 (CR 119.8).
    fn add_cant_lose_life_permanent(state: &mut GameState, owner: PlayerId) {
        use crate::game::zones::create_object;
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let id = create_object(
            state,
            CardId(900),
            owner,
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );
    }

    fn pay_any_life_ability() -> Effect {
        Effect::PayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            scale: None,
            payer: crate::types::ability::TargetFilter::Controller,
        }
    }

    /// CR 119.4 + CR 118.3: "pay any amount of life" suspends on a
    /// PayAmountChoice with `max` = current life and no life deducted yet
    /// (mirrors `pay_any_amount_of_energy_pauses_for_choice`).
    #[test]
    fn pay_any_amount_of_life_pauses_for_choice() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let ability = make_ability(pay_any_life_ability());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                resource,
                min,
                max,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*resource, PayableResource::Life);
                assert_eq!(*min, 0);
                assert_eq!(*max, 20);
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
        assert_eq!(state.players[0].life, 20, "life must not be deducted yet");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no LifeChanged event until the player commits an amount"
        );
    }

    /// CR 119.8: Under a CantLoseLife lock the "pay any amount of life" prompt
    /// clamps `max` to 0 — the player can only decline (pay 0).
    #[test]
    fn pay_any_amount_of_life_clamps_max_under_cant_lose_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let ability = make_ability(pay_any_life_ability());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::Life);
                assert_eq!(*max, 0, "CantLoseLife clamps the payable life to 0");
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
    }

    /// CR 119.4 + CR 603.7c + CR 608.2c: Necrodominance / Plunge into Darkness
    /// shape — optional `PayCost { PayLife, Variable X } → IfYouDo Draw
    /// { EventContextAmount }`. Accept the optional, submit 3 life: 3 life lost
    /// AND 3 cards drawn (the chosen amount binds the downstream count).
    ///
    /// Fail-on-revert: with the prompt removed the PayLife{Variable X} resolves
    /// to 0 (no prompt), `last_effect_count` is never stamped, the Draw reads 0,
    /// and life is unchanged.
    #[test]
    fn pay_any_amount_of_life_optional_then_draw_that_many() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Necrodominance".to_string(),
            Zone::Battlefield,
        );
        // Five library cards available to draw.
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        state.players[0].life = 20;

        // IfYouDo SequentialSibling Draw { EventContextAmount } rider.
        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        let mut pay = ResolvedAbility::new(pay_any_life_ability(), vec![], source_id, PlayerId(0));
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();

        // Step A: optional prompt.
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Step B: accept → PayAmountChoice { resource: Life, max: 20 }.
        let waiting = handle_optional_effect_choice(&mut state, true, &mut events).unwrap();
        match &waiting {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::Life);
                assert_eq!(*max, 20);
            }
            other => panic!("expected PayAmountChoice after Yes, got {other:?}"),
        }

        // Step C: submit 3 → 3 life lost, 3 cards drawn, no residue.
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitPayAmount { amount: 3 },
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.players[0].life, 17,
            "3 life paid via the life-loss authority"
        );
        assert_eq!(
            state.players[0].hand.len(),
            3,
            "IfYouDo Draw{{EventContextAmount=3}} draws the chosen count"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "chain must fully resolve, got {:?}",
            state.waiting_for
        );
    }

    /// CR 119.8: Under CantLoseLife the prompt clamps max=0; submitting 0
    /// declines the payment — no life lost, the IfYouDo Draw reads 0.
    #[test]
    fn pay_any_amount_of_life_under_lock_submit_zero_draws_zero() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_payment_choices::handle_optional_effect_choice;
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::game::zones::create_object;
        use crate::types::ability::{AbilityCondition, SubAbilityLink, TargetFilter};
        use crate::types::actions::GameAction;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Necrodominance".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        state.players[0].life = 20;
        add_cant_lose_life_permanent(&mut state, PlayerId(0));

        let mut draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        draw.condition = Some(AbilityCondition::effect_performed());
        draw.sub_link = SubAbilityLink::SequentialSibling;

        let mut pay = ResolvedAbility::new(pay_any_life_ability(), vec![], source_id, PlayerId(0));
        pay.sub_ability = Some(Box::new(draw));
        pay.optional = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        let waiting = handle_optional_effect_choice(&mut state, true, &mut events).unwrap();
        match &waiting {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::Life);
                assert_eq!(*max, 0, "CantLoseLife clamps max to 0");
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }
        handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SubmitPayAmount { amount: 0 },
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].life, 20, "no life lost under the lock");
        assert_eq!(
            state.players[0].hand.len(),
            0,
            "paying 0 life draws 0 cards"
        );
    }
}
