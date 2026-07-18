use engine::game::static_abilities::object_crew_power_contribution;
use engine::types::ability::{ActivationRestriction, StaticCondition, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::{CostResume, GameState, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::CrewAction;
use engine::types::zones::{ExileCostSourceZone, Zone};

use crate::eval::evaluate_creature;
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::sacrifice_cost;

pub struct PaymentSelectionPolicy;

impl TacticalPolicy for PaymentSelectionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PaymentSelection
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: payment resource ordering applies universally.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::score(
            self.score(ctx),
            PolicyReason::new("payment_selection_value_score"),
        )
    }
}

impl PaymentSelectionPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if let Some(score) = station_activation_score(ctx) {
            return score;
        }
        if let Some(score) = crew_or_saddle_score(ctx) {
            return score;
        }

        let GameAction::SelectCards { cards } = &ctx.candidate.action else {
            return 0.0;
        };
        let WaitingFor::PayCost {
            kind,
            min_count,
            resume,
            ..
        } = &ctx.decision.waiting_for
        else {
            return 0.0;
        };

        if matches!(kind, PayCostKind::Sacrifice) {
            return 0.0;
        }

        let cost: f64 = cards
            .iter()
            .map(|&id| payment_cost(ctx.state, id, kind, ctx.penalties()))
            .sum();
        let extra_count = cards.len().saturating_sub(*min_count);
        let extra_penalty = extra_count as f64 * 0.35;
        let resume_scale = match resume {
            CostResume::Spell { .. } | CostResume::SpellCost { .. } => 1.0,
            CostResume::ManaAbility { .. } => 0.8,
        };

        -(cost + extra_penalty) * resume_scale
    }
}

fn crew_or_saddle_score(ctx: &PolicyContext<'_>) -> Option<f64> {
    let (creature_ids, threshold, action) = match (&ctx.decision.waiting_for, &ctx.candidate.action)
    {
        (
            WaitingFor::CrewVehicle {
                vehicle_id,
                crew_power,
                ..
            },
            GameAction::CrewVehicle {
                vehicle_id: action_vehicle_id,
                creature_ids,
            },
        ) if vehicle_id == action_vehicle_id => (creature_ids, *crew_power, CrewAction::Crew),
        (
            WaitingFor::SaddleMount {
                mount_id,
                saddle_power,
                ..
            },
            GameAction::SaddleMount {
                mount_id: action_mount_id,
                creature_ids,
            },
        ) if mount_id == action_mount_id => (creature_ids, *saddle_power, CrewAction::Saddle),
        _ => return None,
    };

    let contribution: u32 = creature_ids
        .iter()
        .map(|&id| object_crew_power_contribution(ctx.state, id, action).max(0) as u32)
        .sum();
    let preservation_cost: f64 = creature_ids
        .iter()
        .map(|&id| permanent_value(ctx.state, id) * 0.05)
        .sum();

    // CR 702.122a / CR 702.171a: Crew and saddle only need total power at
    // least N. Extra contribution beyond N is legal but strategically wasteful.
    let overshoot = contribution.saturating_sub(threshold);
    Some(-(f64::from(overshoot) * 0.2 + preservation_cost))
}

fn station_activation_score(ctx: &PolicyContext<'_>) -> Option<f64> {
    let GameAction::ActivateStation {
        spacecraft_id,
        creature_id: Some(creature_id),
    } = &ctx.candidate.action
    else {
        return None;
    };
    let WaitingFor::StationTarget {
        spacecraft_id: waiting_spacecraft_id,
        ..
    } = ctx.decision.waiting_for
    else {
        return None;
    };
    if *spacecraft_id != waiting_spacecraft_id {
        return None;
    }

    let contribution =
        object_crew_power_contribution(ctx.state, *creature_id, CrewAction::Station).max(0) as u32;
    let preservation_cost =
        permanent_value(ctx.state, *creature_id) * 0.05 + f64::from(contribution) * 0.02;

    let Some(remaining) = next_station_threshold_remaining(ctx.state, *spacecraft_id) else {
        return Some(-preservation_cost);
    };

    // CR 721.2a-b: Station threshold abilities unlock at N+ charge counters.
    // Prefer the least sufficient creature for the next threshold instead of
    // spending excess power that has no additional threshold value.
    if contribution >= remaining {
        let overshoot = contribution - remaining;
        Some(3.0 - f64::from(overshoot) * 0.2 - preservation_cost)
    } else {
        Some(f64::from(contribution) / f64::from(remaining) - preservation_cost * 0.25)
    }
}

fn next_station_threshold_remaining(state: &GameState, spacecraft_id: ObjectId) -> Option<u32> {
    let spacecraft = state.objects.get(&spacecraft_id)?;
    let current = spacecraft
        .counters
        .get(&station_counter())
        .copied()
        .unwrap_or(0);

    let static_thresholds = spacecraft
        .static_definitions
        .as_slice()
        .iter()
        .filter_map(|def| {
            def.condition
                .as_ref()
                .and_then(station_static_condition_threshold)
        });
    let trigger_thresholds = spacecraft
        .trigger_definitions
        .as_slice()
        .iter()
        .map(|entry| &entry.definition)
        .filter_map(|def| {
            def.condition
                .as_ref()
                .and_then(station_trigger_condition_threshold)
        });
    let activation_thresholds = spacecraft.abilities.iter().flat_map(|def| {
        def.activation_restrictions
            .iter()
            .filter_map(station_activation_threshold)
    });

    static_thresholds
        .chain(trigger_thresholds)
        .chain(activation_thresholds)
        .filter(|threshold| *threshold > current)
        .min()
        .map(|threshold| threshold - current)
}

fn station_static_condition_threshold(condition: &StaticCondition) -> Option<u32> {
    match condition {
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum: None,
        } if is_station_charge_counter(counters) => Some(*minimum),
        _ => None,
    }
}

fn station_trigger_condition_threshold(condition: &TriggerCondition) -> Option<u32> {
    match condition {
        TriggerCondition::HasCounters {
            counters,
            minimum,
            maximum: None,
        } if is_station_charge_counter(counters) => Some(*minimum),
        _ => None,
    }
}

fn station_activation_threshold(restriction: &ActivationRestriction) -> Option<u32> {
    match restriction {
        ActivationRestriction::CounterThreshold {
            counters,
            minimum,
            maximum: None,
        } if is_station_charge_counter(counters) => Some(*minimum),
        _ => None,
    }
}

fn is_station_charge_counter(counters: &CounterMatch) -> bool {
    counters == &CounterMatch::OfType(station_counter())
}

fn station_counter() -> CounterType {
    CounterType::Generic("charge".to_string())
}

fn payment_cost(
    state: &GameState,
    obj_id: ObjectId,
    kind: &PayCostKind,
    penalties: &crate::config::PolicyPenalties,
) -> f64 {
    match kind {
        PayCostKind::Discard => card_value(state, obj_id),
        // CR 701.20a + CR 701.20b: Revealing doesn't move the card — it stays
        // in hand — so the real resource cost is ~0, mirroring Behold's
        // reveal-from-hand branch.
        PayCostKind::Reveal => card_value(state, obj_id) * 0.1,
        PayCostKind::ReturnToHand => 0.5 + permanent_value(state, obj_id) * 0.5,
        PayCostKind::ExileFromZone { zone } => match zone {
            ExileCostSourceZone::Hand => card_value(state, obj_id) * 1.2,
            ExileCostSourceZone::Graveyard => 0.1 + card_value(state, obj_id) * 0.2,
        },
        PayCostKind::ExileMaterials { .. } => match state.objects.get(&obj_id).map(|o| o.zone) {
            Some(Zone::Battlefield) => permanent_value(state, obj_id),
            Some(Zone::Graveyard) => 0.1 + card_value(state, obj_id) * 0.2,
            _ => card_value(state, obj_id),
        },
        // CR 701.13: Exile a battlefield permanent you control as a cost
        // (Food Chain class) — valued like the battlefield ExileMaterials case.
        PayCostKind::ExilePermanent { .. } => permanent_value(state, obj_id),
        PayCostKind::ExileFromManaZone { zone } => match zone {
            Zone::Battlefield => permanent_value(state, obj_id),
            Zone::Hand => card_value(state, obj_id) * 1.2,
            Zone::Graveyard => 0.1 + card_value(state, obj_id) * 0.2,
            _ => card_value(state, obj_id) * 0.5,
        },
        PayCostKind::RemoveCounter { .. } => permanent_value(state, obj_id) * 0.5,
        PayCostKind::TapCreatures { .. } => permanent_value(state, obj_id) * 0.35,
        // CR 117.1 + CR 601.2b: "exile any number" aggregate-threshold cost
        // (Baron Helmut Zemo's Boast). `AbilityCost::ExileWithAggregate` is a
        // zone-parameterized building block, so value the chosen card by its
        // source `zone` — mirroring `ExileFromManaZone` — rather than assuming
        // graveyard fuel: a hand/battlefield aggregate exile spends real cards.
        PayCostKind::ExileAggregate { zone, .. } => match zone {
            Zone::Battlefield => permanent_value(state, obj_id),
            Zone::Hand => card_value(state, obj_id) * 1.2,
            Zone::Graveyard => 0.1 + card_value(state, obj_id) * 0.2,
            _ => card_value(state, obj_id) * 0.5,
        },
        PayCostKind::Behold { .. } => card_value(state, obj_id) * 0.1,
        // CR 701.3d: Unattaching an Equipment as a cost keeps it on the
        // battlefield (only the attachment link is removed), so the real resource
        // cost is ~0 — the AI should treat the chosen Equipment as free to detach.
        PayCostKind::UnattachFrom { .. } => 0.0,
        PayCostKind::Sacrifice => sacrifice_cost(state, obj_id, penalties),
    }
}

fn permanent_value(state: &GameState, obj_id: ObjectId) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        return evaluate_creature(state, obj_id);
    }
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return 3.0;
    }
    if obj.is_token {
        return 0.4;
    }
    (obj.mana_cost.mana_value() as f64).min(4.0)
}

fn card_value(state: &GameState, obj_id: ObjectId) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };

    let mut value = 0.0;
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(obj.base_power.unwrap_or(0)).max(0) as f64;
        let toughness = obj
            .toughness
            .unwrap_or(obj.base_toughness.unwrap_or(0))
            .max(0) as f64;
        value += power * 1.5 + toughness;
    }
    if obj.card_types.core_types.contains(&CoreType::Land) {
        value += 3.0;
    }
    value + obj.mana_cost.mana_value() as f64 * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ContinuousModification, Effect, QuantityExpr, ResolvedAbility, StaticCondition,
        StaticDefinition, TargetFilter,
    };
    use engine::types::game_state::PendingCast;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;

    const AI: PlayerId = PlayerId(0);

    fn pending() -> Box<PendingCast> {
        Box::new(PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(100),
                AI,
            ),
            ManaCost::zero(),
        ))
    }

    fn score_for_action(state: &GameState, waiting_for: WaitingFor, action: GameAction) -> f64 {
        let decision = AiDecisionContext {
            waiting_for,
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Selection,
            },
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        PaymentSelectionPolicy.score(&ctx)
    }

    fn score_for(state: &GameState, waiting_for: WaitingFor, cards: Vec<ObjectId>) -> f64 {
        score_for_action(state, waiting_for, GameAction::SelectCards { cards })
    }

    fn station_score_for(
        state: &GameState,
        spacecraft_id: ObjectId,
        eligible_creatures: Vec<ObjectId>,
        creature_id: ObjectId,
    ) -> f64 {
        score_for_action(
            state,
            WaitingFor::StationTarget {
                player: AI,
                spacecraft_id,
                eligible_creatures,
            },
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(creature_id),
            },
        )
    }

    fn crew_score_for(
        state: &GameState,
        vehicle_id: ObjectId,
        crew_power: u32,
        eligible_creatures: Vec<ObjectId>,
        creature_ids: Vec<ObjectId>,
    ) -> f64 {
        let contributions = eligible_creatures
            .iter()
            .map(|&id| object_crew_power_contribution(state, id, CrewAction::Crew))
            .collect();
        score_for_action(
            state,
            WaitingFor::CrewVehicle {
                player: AI,
                vehicle_id,
                crew_power,
                eligible_creatures,
                contributions,
            },
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids,
            },
        )
    }

    fn saddle_score_for(
        state: &GameState,
        mount_id: ObjectId,
        saddle_power: u32,
        eligible_creatures: Vec<ObjectId>,
        creature_ids: Vec<ObjectId>,
    ) -> f64 {
        let contributions = eligible_creatures
            .iter()
            .map(|&id| object_crew_power_contribution(state, id, CrewAction::Saddle))
            .collect();
        score_for_action(
            state,
            WaitingFor::SaddleMount {
                player: AI,
                mount_id,
                saddle_power,
                eligible_creatures,
                contributions,
            },
            GameAction::SaddleMount {
                mount_id,
                creature_ids,
            },
        )
    }

    fn make_creature(state: &mut GameState, name: &str, zone: Zone, power: i32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            AI,
            name.to_string(),
            zone,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(power);
        obj.power = Some(power);
        obj.base_toughness = Some(power);
        obj.toughness = Some(power);
        id
    }

    fn make_artifact(state: &mut GameState, name: &str, zone: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            AI,
            name.to_string(),
            zone,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        id
    }

    fn make_spacecraft_with_threshold(
        state: &mut GameState,
        current_charge_counters: u32,
        threshold: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            AI,
            "Test Spacecraft".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.counters.insert(
            CounterType::Generic("charge".to_string()),
            current_charge_counters,
        );
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                    minimum: threshold,
                    maximum: None,
                })
                .modifications(vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }])
                .description(format!("CR 721.2b: Spacecraft unlocks at {threshold}+")),
        );
        id
    }

    fn make_land(state: &mut GameState, name: &str, zone: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            AI,
            name.to_string(),
            zone,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    #[test]
    fn discard_cost_prefers_lower_value_card() {
        let mut state = GameState::new_two_player(42);
        let land = make_land(&mut state, "Land", Zone::Hand);
        let creature = make_creature(&mut state, "Large Creature", Zone::Hand, 5);
        let waiting_for = |choices| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::Discard,
            choices,
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let land_score = score_for(&state, waiting_for(vec![land, creature]), vec![land]);
        let creature_score = score_for(&state, waiting_for(vec![land, creature]), vec![creature]);

        assert!(land_score > creature_score);
    }

    #[test]
    fn graveyard_exile_cost_prefers_low_value_card() {
        let mut state = GameState::new_two_player(42);
        let blank = create_object(
            &mut state,
            CardId(1),
            AI,
            "Spent Spell".to_string(),
            Zone::Graveyard,
        );
        let creature = make_creature(&mut state, "Escape Threat", Zone::Graveyard, 5);
        let waiting_for = |choices| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
            choices,
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let blank_score = score_for(&state, waiting_for(vec![blank, creature]), vec![blank]);
        let creature_score = score_for(&state, waiting_for(vec![blank, creature]), vec![creature]);

        assert!(blank_score > creature_score);
    }

    // The aggregate-exile cost (`PayCostKind::ExileAggregate`) is zone-parameterized,
    // so its payment must be valued by the source `zone`: a graveyard exile is cheap
    // fuel (0.1 + card_value*0.2) while a hand exile spends a real card
    // (card_value*1.2). The AI must therefore prefer paying a graveyard aggregate
    // over an otherwise-identical hand one.
    //
    // Discrimination: the pre-fix arm valued every `ExileAggregate` as graveyard,
    // making these two scores equal — so `assert!(graveyard > hand)` flips red.
    #[test]
    fn exile_aggregate_cost_values_by_source_zone() {
        use engine::types::ability::{AggregateFunction, Comparator, ObjectProperty, TargetFilter};
        use engine::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let hand_card = make_creature(&mut state, "Hand Card", Zone::Hand, 5);
        let gy_card = make_creature(&mut state, "Graveyard Card", Zone::Graveyard, 5);
        let agg = |zone, choices| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::ExileAggregate {
                zone,
                function: AggregateFunction::Sum,
                property: ObjectProperty::ManaSymbolCount(ManaColor::Black),
                comparator: Comparator::GE,
                value: 15,
                filter: TargetFilter::Any,
            },
            choices,
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let hand_score = score_for(&state, agg(Zone::Hand, vec![hand_card]), vec![hand_card]);
        let gy_score = score_for(&state, agg(Zone::Graveyard, vec![gy_card]), vec![gy_card]);

        assert!(
            gy_score > hand_score,
            "graveyard aggregate exile must be cheaper (preferred) than hand: gy={gy_score} hand={hand_score}"
        );
    }

    #[test]
    fn range_payment_penalizes_extra_cards() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(1), AI, "A".to_string(), Zone::Graveyard);
        let second = create_object(&mut state, CardId(2), AI, "B".to_string(), Zone::Graveyard);
        let waiting_for = |choices| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
            choices,
            count: 2,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let one_score = score_for(&state, waiting_for(vec![first, second]), vec![first]);
        let two_score = score_for(
            &state,
            waiting_for(vec![first, second]),
            vec![first, second],
        );

        assert!(one_score > two_score);
    }

    #[test]
    fn sacrifice_cost_is_left_to_sacrifice_value_policy() {
        let mut state = GameState::new_two_player(42);
        let creature = make_creature(&mut state, "Bear", Zone::Battlefield, 2);
        let waiting_for = WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::Sacrifice,
            choices: vec![creature],
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        assert_eq!(score_for(&state, waiting_for, vec![creature]), 0.0);
    }

    #[test]
    fn station_prefers_exact_threshold_over_large_overshoot() {
        let mut state = GameState::new_two_player(42);
        let spacecraft = make_spacecraft_with_threshold(&mut state, 0, 8);
        let exact = make_creature(&mut state, "Eight Power", Zone::Battlefield, 8);
        let oversized = make_creature(&mut state, "Twenty Two Power", Zone::Battlefield, 22);

        let exact_score = station_score_for(&state, spacecraft, vec![exact, oversized], exact);
        let oversized_score =
            station_score_for(&state, spacecraft, vec![exact, oversized], oversized);

        assert!(exact_score > oversized_score);
    }

    #[test]
    fn station_prefers_larger_progress_when_no_creature_reaches_threshold() {
        let mut state = GameState::new_two_player(42);
        let spacecraft = make_spacecraft_with_threshold(&mut state, 0, 8);
        let low = make_creature(&mut state, "Three Power", Zone::Battlefield, 3);
        let high = make_creature(&mut state, "Five Power", Zone::Battlefield, 5);

        let low_score = station_score_for(&state, spacecraft, vec![low, high], low);
        let high_score = station_score_for(&state, spacecraft, vec![low, high], high);

        assert!(high_score > low_score);
    }

    #[test]
    fn station_accounts_for_existing_charge_counters() {
        let mut state = GameState::new_two_player(42);
        let spacecraft = make_spacecraft_with_threshold(&mut state, 4, 8);
        let exact = make_creature(&mut state, "Four Power", Zone::Battlefield, 4);
        let oversized = make_creature(&mut state, "Eight Power", Zone::Battlefield, 8);

        let exact_score = station_score_for(&state, spacecraft, vec![exact, oversized], exact);
        let oversized_score =
            station_score_for(&state, spacecraft, vec![exact, oversized], oversized);

        assert!(exact_score > oversized_score);
    }

    #[test]
    fn crew_prefers_least_sufficient_contribution() {
        let mut state = GameState::new_two_player(42);
        let vehicle = make_artifact(&mut state, "Vehicle", Zone::Battlefield);
        let exact = make_creature(&mut state, "Eight Power", Zone::Battlefield, 8);
        let oversized = make_creature(&mut state, "Twenty Two Power", Zone::Battlefield, 22);

        let exact_score = crew_score_for(&state, vehicle, 8, vec![exact, oversized], vec![exact]);
        let oversized_score =
            crew_score_for(&state, vehicle, 8, vec![exact, oversized], vec![oversized]);

        assert!(exact_score > oversized_score);
    }

    #[test]
    fn saddle_prefers_least_sufficient_contribution() {
        let mut state = GameState::new_two_player(42);
        let mount = make_creature(&mut state, "Mount", Zone::Battlefield, 4);
        let exact = make_creature(&mut state, "Eight Power", Zone::Battlefield, 8);
        let oversized = make_creature(&mut state, "Twenty Two Power", Zone::Battlefield, 22);

        let exact_score = saddle_score_for(&state, mount, 8, vec![exact, oversized], vec![exact]);
        let oversized_score =
            saddle_score_for(&state, mount, 8, vec![exact, oversized], vec![oversized]);

        assert!(exact_score > oversized_score);
    }
}
