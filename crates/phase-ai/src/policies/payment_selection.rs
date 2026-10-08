use engine::ai_support::legal_actions_full;
use engine::game::engine::apply_as_current_for_simulation;
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

use crate::card_value::cost_card_value;
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::{permanent_board_value, sacrifice_cost};

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
            CostResume::Resolution => 1.0,
            CostResume::ManaAbility { .. } => 0.8,
        };

        let needed_land_penalty = if discard_spends_last_playable_land(ctx, kind, cards) {
            ctx.penalties().payment_selection_needed_land_penalty
        } else {
            0.0
        };

        -(cost + extra_penalty) * resume_scale + needed_land_penalty
    }
}

/// Preserve a currently playable land only when the engine proves a sibling
/// payment selections all leave that exact land as the sole legal `PlayLand`.
///
/// The probes deliberately stay at the concrete `PayCost` boundary: first the
/// selected payment must itself leave no legal land play, then every qualifying
/// sibling is replayed on a clone to prove the retained land is the only land
/// it can actually play in every replay.
/// Hand counts and land-drop counters are not substitutes for either legality
/// check.
fn discard_spends_last_playable_land(
    ctx: &PolicyContext<'_>,
    kind: &PayCostKind,
    selected: &[ObjectId],
) -> bool {
    let (
        WaitingFor::PayCost {
            player,
            kind: PayCostKind::Discard,
            ..
        },
        GameAction::SelectCards { cards },
    ) = (&ctx.state.waiting_for, &ctx.candidate.action)
    else {
        return false;
    };
    if *player != ctx.ai_player || cards != selected || !matches!(kind, PayCostKind::Discard) {
        return false;
    }
    if !selected.iter().copied().any(|id| is_land(ctx.state, id)) {
        return false;
    }

    let legal_actions = legal_actions_full(ctx.state).0;
    if !legal_actions
        .iter()
        .any(|action| matches!(action, GameAction::SelectCards { cards } if cards == selected))
    {
        return false;
    }

    let mut post_selected = ctx.state.clone();
    if apply_as_current_for_simulation(&mut post_selected, ctx.candidate.action.clone()).is_err()
        || !playable_lands_after_stack_clears(&post_selected, ctx.ai_player).is_empty()
    {
        return false;
    }

    // Simulate each sibling once, then reuse the projected land plays for all
    // land cards in this selected payment. This keeps a multi-card discard
    // from multiplying full-state forecasts by its number of lands.
    let sibling_land_plays: Vec<(Vec<ObjectId>, Vec<ObjectId>)> = legal_actions
        .iter()
        .filter_map(|action| {
            let GameAction::SelectCards { cards: sibling } = action else {
                return None;
            };
            if sibling == selected {
                return None;
            }

            let mut post_discard = ctx.state.clone();
            if apply_as_current_for_simulation(&mut post_discard, action.clone()).is_err() {
                return Some((sibling.clone(), Vec::new()));
            }
            Some((
                sibling.clone(),
                playable_lands_after_stack_clears(&post_discard, ctx.ai_player),
            ))
        })
        .collect();

    selected
        .iter()
        .copied()
        .filter(|&land| is_land(ctx.state, land))
        .any(|land| {
            sibling_land_plays
                .iter()
                .any(|(sibling, _)| !sibling.contains(&land))
                && {
                    sibling_land_plays
                        .iter()
                        .filter(|(sibling, _)| !sibling.contains(&land))
                        .all(|(_, lands)| lands.as_slice() == [land])
                }
        })
}

/// Enumerate land plays in the next empty-stack priority window after a cost is paid.
///
/// A spell or activated ability remains on the stack immediately after `PayCost`,
/// so `legal_actions_full` correctly omits land plays at that instant. For the
/// resolved (empty stack) or single newly-created stack entry, this probe asks
/// the engine's normal candidate generator about the empty-stack priority
/// window. It declines to infer anything when other stack entries exist. This
/// is a bounded tactical forecast, not an action application.
fn playable_lands_after_stack_clears(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let mut next_priority = state.clone();
    if next_priority.stack.len() > 1
        || next_priority
            .stack
            .get(0)
            .is_some_and(|entry| entry.controller != player)
    {
        return Vec::new();
    }
    next_priority.stack.clear();
    next_priority.waiting_for = WaitingFor::Priority { player };
    legal_actions_full(&next_priority)
        .0
        .iter()
        .filter_map(|action| match action {
            GameAction::PlayLand { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

fn is_land(state: &GameState, object_id: ObjectId) -> bool {
    state
        .objects
        .get(&object_id)
        .is_some_and(|object| object.card_types.core_types.contains(&CoreType::Land))
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
    // Crew/saddle taps the creature rather than giving it up, so the same
    // give-up authority is scaled hard (0.05). A creature-land tapped to crew
    // is priced by `permanent_board_value`'s dominance rule, `max(land, body)`: for a
    // 1/1 Dryad Arbor that is the land value (tapping it for a Vehicle costs a
    // mana source for the turn), for an animated Treetop Village it is the
    // body. There is no `Land`-first branch to match — the land reading is a
    // floor, not a short-circuit.
    let preservation_cost: f64 = creature_ids
        .iter()
        .map(|&id| permanent_board_value(ctx.state, id, ctx.penalties()) * 0.05)
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
    // Station spends the creature the same way crew does — same authority,
    // same 0.05 scaling.
    let preservation_cost = permanent_board_value(ctx.state, *creature_id, ctx.penalties()) * 0.05
        + f64::from(contribution) * 0.02;

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

/// What it costs the AI to spend `obj_id` on a `PayCostKind`.
///
/// **Every kind prices board presence through
/// `strategy_helpers::permanent_board_value`; kinds that surrender a permanent
/// add the command-zone repurchase term through `strategy_helpers::sacrifice_cost`.**
/// This function previously carried a private `permanent_value` twin that
/// disagreed with that authority on two axes at once: it priced a land at 3.0
/// against the authority's `sacrifice_land_penalty` (4.5), and it tested
/// `Creature` **before** `Land` where the authority tested `Land` first, so a
/// creature-land (Dryad Arbor, an animated Treetop Village) came out of the two
/// functions with opposite *classifications*, not merely opposite prices. Both
/// are now one call, so divergence is structurally impossible.
///
/// The resolution was not "pick the twin's order or the authority's". By
/// CR 300.2 ("Some objects have more than one card type ... Such objects
/// combine the aspects of each of those card types") a creature-land is a
/// creature **and** a land simultaneously, so
/// neither first-match order is correct — `sacrifice_cost` prices such a
/// permanent by **dominance**, `max(land, creature)`. See its `Land` branch.
///
/// The per-kind economics live at the call sites as multipliers on that one
/// scalar, which is where they belong: `ReturnToHand` halves it because the
/// card comes back, `RemoveCounter` and `TapCreatures` scale it because the
/// permanent is not given up at all, and the exile family pays it in full.
fn payment_cost(
    state: &GameState,
    obj_id: ObjectId,
    kind: &PayCostKind,
    penalties: &crate::config::PolicyPenalties,
) -> f64 {
    match kind {
        PayCostKind::Discard => cost_card_value(state, obj_id),
        // CR 701.20a + CR 701.20b: Revealing doesn't move the card — it stays
        // in hand — so the real resource cost is ~0, mirroring Behold's
        // reveal-from-hand branch.
        PayCostKind::Reveal => cost_card_value(state, obj_id) * 0.1,
        PayCostKind::ReturnToHand => 0.5 + permanent_board_value(state, obj_id, penalties) * 0.5,
        PayCostKind::ExileFromZone { zone } => match zone {
            ExileCostSourceZone::Hand => cost_card_value(state, obj_id) * 1.2,
            ExileCostSourceZone::Graveyard => 0.1 + cost_card_value(state, obj_id) * 0.2,
        },
        PayCostKind::ExileMaterials { .. } => match state.objects.get(&obj_id).map(|o| o.zone) {
            Some(Zone::Battlefield) => sacrifice_cost(state, obj_id, penalties),
            Some(Zone::Graveyard) => 0.1 + cost_card_value(state, obj_id) * 0.2,
            _ => cost_card_value(state, obj_id),
        },
        // CR 701.13: Exile a battlefield permanent you control as a cost
        // (Food Chain class) — valued like the battlefield ExileMaterials case.
        PayCostKind::ExilePermanent { .. } => sacrifice_cost(state, obj_id, penalties),
        PayCostKind::ExileFromManaZone { zone } => match zone {
            Zone::Battlefield => sacrifice_cost(state, obj_id, penalties),
            Zone::Hand => cost_card_value(state, obj_id) * 1.2,
            Zone::Graveyard => 0.1 + cost_card_value(state, obj_id) * 0.2,
            Zone::Library | Zone::Stack | Zone::Exile | Zone::Command => {
                cost_card_value(state, obj_id) * 0.5
            }
        },
        PayCostKind::RemoveCounter { .. } => permanent_board_value(state, obj_id, penalties) * 0.5,
        PayCostKind::TapCreatures { .. } => permanent_board_value(state, obj_id, penalties) * 0.35,
        // CR 117.1 + CR 601.2b: "exile any number" aggregate-threshold cost
        // (Baron Helmut Zemo's Boast). `AbilityCost::ExileWithAggregate` is a
        // zone-parameterized building block, so value the chosen card by its
        // source `zone` — mirroring `ExileFromManaZone` — rather than assuming
        // graveyard fuel: a hand/battlefield aggregate exile spends real cards.
        PayCostKind::ExileAggregate { zone, .. } => match zone {
            Zone::Battlefield => sacrifice_cost(state, obj_id, penalties),
            Zone::Hand => cost_card_value(state, obj_id) * 1.2,
            Zone::Graveyard => 0.1 + cost_card_value(state, obj_id) * 0.2,
            Zone::Library | Zone::Stack | Zone::Exile | Zone::Command => {
                cost_card_value(state, obj_id) * 0.5
            }
        },
        PayCostKind::Behold { .. } => cost_card_value(state, obj_id) * 0.1,
        // CR 701.3d: Unattaching an Equipment as a cost keeps it on the
        // battlefield (only the attachment link is removed), so the real resource
        // cost is ~0 — the AI should treat the chosen Equipment as free to detach.
        PayCostKind::UnattachFrom { .. } => 0.0,
        PayCostKind::Sacrifice => sacrifice_cost(state, obj_id, penalties),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ContinuousModification, CounterCostSelection, Effect, QuantityExpr, ResolvedAbility,
        StaticCondition, StaticDefinition, TapCreaturesSelectionMode, TargetFilter,
    };
    use engine::types::game_state::{CastingVariant, PendingCast, StackEntry, StackEntryKind};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;

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
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
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

    fn make_commander(state: &mut GameState, name: &str) -> ObjectId {
        let id = make_creature(state, name, Zone::Battlefield, 4);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_commander = true;
        obj.mana_cost = ManaCost::generic(4);
        obj.base_mana_cost = ManaCost::generic(4);
        state.format_config.command_zone = true;
        state.commander_cast_count.insert(id, 1);
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

    fn install_discard_payment(state: &mut GameState, choices: Vec<ObjectId>) -> ObjectId {
        install_discard_payment_with_count(state, choices, 1)
    }

    fn install_discard_payment_with_count(
        state: &mut GameState,
        choices: Vec<ObjectId>,
        count: usize,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(900),
            AI,
            "Discard Cost Spell".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), source, AI);
        engine::game::stack::push_to_stack(
            state,
            StackEntry {
                id: source,
                source_id: source,
                controller: AI,
                kind: StackEntryKind::Spell {
                    card_id: CardId(900),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut Vec::new(),
        );
        let pending = Box::new(PendingCast::new(
            source,
            CardId(900),
            ability,
            ManaCost::zero(),
        ));
        state.waiting_for = WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::Discard,
            choices,
            count,
            min_count: count,
            resume: CostResume::Spell { spell: pending },
        };
        source
    }

    fn production_discard_score(state: &GameState, cards: Vec<ObjectId>) -> f64 {
        score_for_action(
            state,
            state.waiting_for.clone(),
            GameAction::SelectCards { cards },
        )
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
    fn discard_retains_final_playable_land_over_equal_value_nonland() {
        let mut state = GameState::new_two_player(42);
        let land = make_land(&mut state, "Forest", Zone::Hand);
        let nonland = create_object(
            &mut state,
            CardId(2),
            AI,
            "Blank Spell".to_string(),
            Zone::Hand,
        );
        state.players[0].hand = [land, nonland].into_iter().collect();
        let waiting_for = |choices| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::Discard,
            choices,
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let discard_land = score_for(&state, waiting_for(vec![land, nonland]), vec![land]);
        let discard_nonland = score_for(&state, waiting_for(vec![land, nonland]), vec![nonland]);

        assert!(
            discard_nonland > discard_land,
            "the final playable land must be retained at PayCost selection"
        );
    }

    #[test]
    fn production_discard_probe_retains_a_land_only_when_an_engine_legal_sibling_can_play_it() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = AI;
        let land = make_land(&mut state, "Forest", Zone::Hand);
        let nonland = create_object(
            &mut state,
            CardId(2),
            AI,
            "Blank Spell".to_string(),
            Zone::Hand,
        );
        state.players[AI.0 as usize].hand = [land, nonland].into_iter().collect();
        install_discard_payment(&mut state, vec![land, nonland]);

        let discard_land = production_discard_score(&state, vec![land]);
        let discard_nonland = production_discard_score(&state, vec![nonland]);
        assert!(discard_nonland > discard_land);

        let mut replay = state.clone();
        engine::game::engine::apply(
            &mut replay,
            AI,
            GameAction::SelectCards {
                cards: vec![nonland],
            },
        )
        .expect("the exact legal sibling payment applies through the engine");
        assert_eq!(playable_lands_after_stack_clears(&replay, AI), vec![land]);
    }

    #[test]
    fn production_discard_probe_adds_no_needed_land_penalty_when_forced_or_not_playable() {
        let mut forced = GameState::new_two_player(42);
        let forced_land = make_land(&mut forced, "Forest", Zone::Hand);
        install_discard_payment(&mut forced, vec![forced_land]);
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: forced.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![forced_land],
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        };
        let policy_context = PolicyContext {
            state: &forced,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(!discard_spends_last_playable_land(
            &policy_context,
            &PayCostKind::Discard,
            &[forced_land],
        ),);
        assert_eq!(production_discard_score(&forced, vec![forced_land]), -3.0);
        engine::game::engine::apply(
            &mut forced,
            AI,
            GameAction::SelectCards {
                cards: vec![forced_land],
            },
        )
        .expect("the forced-only-land payment remains engine-legal");

        let mut prohibited = GameState::new_two_player(42);
        let land = make_land(&mut prohibited, "Forest", Zone::Hand);
        let nonland = create_object(
            &mut prohibited,
            CardId(3),
            AI,
            "Blank Spell".to_string(),
            Zone::Hand,
        );
        prohibited.players[AI.0 as usize]
            .hand
            .extend([land, nonland]);
        prohibited.lands_played_this_turn = prohibited.max_lands_per_turn;
        install_discard_payment(&mut prohibited, vec![land, nonland]);
        let decision = AiDecisionContext {
            waiting_for: prohibited.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![land] },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        };
        let policy_context = PolicyContext {
            state: &prohibited,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(!discard_spends_last_playable_land(
            &policy_context,
            &PayCostKind::Discard,
            &[land],
        ));
        assert_eq!(production_discard_score(&prohibited, vec![land]), -3.0);
    }

    #[test]
    fn production_discard_probe_does_not_treat_one_of_two_retained_lands_as_final() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = AI;
        let first_land = make_land(&mut state, "Forest", Zone::Hand);
        let second_land = make_land(&mut state, "Island", Zone::Hand);
        let nonland = create_object(
            &mut state,
            CardId(4),
            AI,
            "Blank Spell".to_string(),
            Zone::Hand,
        );
        state.players[AI.0 as usize]
            .hand
            .extend([first_land, second_land, nonland]);
        install_discard_payment(&mut state, vec![first_land, second_land, nonland]);

        let mut sibling_replay = state.clone();
        engine::game::engine::apply(
            &mut sibling_replay,
            AI,
            GameAction::SelectCards {
                cards: vec![nonland],
            },
        )
        .expect("nonland discard sibling applies through the engine");
        let mut sibling_playable_lands = playable_lands_after_stack_clears(&sibling_replay, AI);
        sibling_playable_lands.sort_unstable();
        sibling_playable_lands.dedup();
        assert_eq!(
            sibling_playable_lands,
            vec![first_land, second_land],
            "the negative verdict must be reached with two retained playable lands"
        );

        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![first_land],
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        };
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let policy_context = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert!(
            !discard_spends_last_playable_land(
                &policy_context,
                &PayCostKind::Discard,
                &[first_land],
            ),
            "discarding a land is not the final-land loss while a sibling payment leaves two land plays"
        );
    }

    #[test]
    fn multi_card_discard_does_not_penalize_a_land_when_another_land_survives_the_candidate() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = AI;
        let first_land = make_land(&mut state, "Forest", Zone::Hand);
        let second_land = make_land(&mut state, "Island", Zone::Hand);
        let spell = create_object(
            &mut state,
            CardId(5),
            AI,
            "Blank Spell".to_string(),
            Zone::Hand,
        );
        state.players[AI.0 as usize]
            .hand
            .extend([first_land, second_land, spell]);
        install_discard_payment_with_count(&mut state, vec![first_land, second_land, spell], 2);

        let mut selected_replay = state.clone();
        engine::game::engine::apply(
            &mut selected_replay,
            AI,
            GameAction::SelectCards {
                cards: vec![first_land, spell],
            },
        )
        .expect("multi-card discard applies through the engine");
        let mut retained_playable_lands = playable_lands_after_stack_clears(&selected_replay, AI);
        retained_playable_lands.sort_unstable();
        retained_playable_lands.dedup();
        assert_eq!(
            retained_playable_lands,
            vec![second_land],
            "the negative verdict must be reached with a retained playable land"
        );

        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![first_land, spell],
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        };
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let policy_context = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert!(
            !discard_spends_last_playable_land(
                &policy_context,
                &PayCostKind::Discard,
                &[first_land, spell],
            ),
            "a two-card payment that still leaves another legal land play is not a final-land loss"
        );
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

    /// The battlefield give-up arms are priced by the SINGLE authority
    /// (`strategy_helpers::sacrifice_cost`), not by a private twin.
    ///
    /// FAILS ON THE DELETED `permanent_value`: it priced a land at a hardcoded
    /// **3.0** while the authority prices it at `sacrifice_land_penalty`
    /// (**4.5**) and caps a noncreature at **4.0** — so under the twin the land
    /// was the *cheaper* permanent to exile and scored higher, and this
    /// assertion inverts.
    ///
    /// `ExilePermanent` is chosen because `discard_spends_last_playable_land`
    /// gates hard on `PayCostKind::Discard`, so no land-retention penalty can
    /// confound the comparison.
    #[test]
    fn battlefield_exile_prices_a_land_by_the_single_give_up_authority() {
        let mut state = GameState::new_two_player(42);
        let land = make_land(&mut state, "Swamp", Zone::Battlefield);
        let artifact = make_artifact(&mut state, "Gilded Lotus", Zone::Battlefield);
        state.objects.get_mut(&artifact).unwrap().mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 5,
        };

        let waiting_for = |choices: Vec<ObjectId>| WaitingFor::PayCost {
            player: AI,
            kind: PayCostKind::ExilePermanent { filter: None },
            choices,
            count: 1,
            min_count: 1,
            resume: CostResume::Spell { spell: pending() },
        };

        let land_score = score_for(&state, waiting_for(vec![land, artifact]), vec![land]);
        let artifact_score = score_for(&state, waiting_for(vec![land, artifact]), vec![artifact]);

        assert!(
            artifact_score > land_score,
            "an MV-5 artifact caps at {} and must be preferred over a land \
             priced at {}: artifact={artifact_score} land={land_score}",
            crate::policies::strategy_helpers::NONCREATURE_SACRIFICE_CAP,
            crate::config::PolicyPenalties::default().sacrifice_land_penalty
        );
        assert!(
            land_score < 0.0,
            "reach-guard: the arm really priced the land (a 0.0 score would \
             mean the policy short-circuited before `payment_cost`)"
        );
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

    /// The pricing reach guard is asserted first; every zero below is therefore
    /// a real partition result, not a format-off vacuity.
    #[test]
    fn command_zone_premium_reaches_surrender_costs_but_not_payment_uses() {
        let mut state = GameState::new_two_player(42);
        let commander = make_commander(&mut state, "Commander");
        let bear = make_creature(&mut state, "Bear", Zone::Battlefield, 4);
        let penalties = crate::config::PolicyPenalties::default();
        let delta = |kind: PayCostKind| {
            payment_cost(&state, commander, &kind, &penalties)
                - payment_cost(&state, bear, &kind, &penalties)
        };

        assert_eq!(delta(PayCostKind::Sacrifice), 6.0);
        assert_eq!(
            delta(PayCostKind::ExilePermanent { filter: None }),
            6.0,
            "reach guard: a surrendered permanent carries the literal premium"
        );
        assert_eq!(
            delta(PayCostKind::TapCreatures {
                mode: TapCreaturesSelectionMode::Fixed
            }),
            0.0
        );
        assert_eq!(
            delta(PayCostKind::RemoveCounter {
                counter_type: CounterMatch::Any,
                count: 1,
                selection: CounterCostSelection::SingleObject,
            }),
            0.0
        );
        assert_eq!(delta(PayCostKind::ReturnToHand), 0.0);
        assert_eq!(
            payment_cost(
                &state,
                commander,
                &PayCostKind::UnattachFrom {
                    filter: TargetFilter::Any,
                },
                &penalties,
            ),
            0.0,
            "unattaching never consults either board-value authority"
        );
    }

    /// Crew and station return absolute policy scores, so their equal values on
    /// the same premium-reached state pin the two least-obvious repoints.
    #[test]
    fn crew_and_station_use_board_value_not_surrender_value() {
        let mut state = GameState::new_two_player(42);
        let commander = make_commander(&mut state, "Commander");
        let bear = make_creature(&mut state, "Bear", Zone::Battlefield, 4);
        let penalties = crate::config::PolicyPenalties::default();
        assert_eq!(
            payment_cost(&state, commander, &PayCostKind::Sacrifice, &penalties)
                - payment_cost(&state, bear, &PayCostKind::Sacrifice, &penalties),
            6.0,
            "reach guard: this exact state must price the commander premium before \
             the board-value equalities can prove the partition"
        );
        let vehicle = make_artifact(&mut state, "Vehicle", Zone::Battlefield);
        let spacecraft = make_spacecraft_with_threshold(&mut state, 0, 4);

        assert_eq!(
            crew_score_for(&state, vehicle, 4, vec![commander, bear], vec![commander]),
            crew_score_for(&state, vehicle, 4, vec![commander, bear], vec![bear]),
            "crew score must not leak the command-zone premium"
        );
        assert_eq!(
            station_score_for(&state, spacecraft, vec![commander, bear], commander),
            station_score_for(&state, spacecraft, vec![commander, bear], bear),
            "station score must not leak the command-zone premium"
        );
    }
}
