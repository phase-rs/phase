use engine::game::effects::stack_reach::{stack_entry_node_reach, NodeReach};
use engine::game::game_object::GameObject;
use engine::game::keywords::protection_prevents_from;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::eval::evaluate_creature;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::chalice_avoidance::spell_can_be_countered;
use super::context::{collect_resolved_abilities, PolicyContext};
use super::effect_classify::{effect_polarity, is_spell_beneficial, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::removal_lethality::{outcome_is_lethal, DamageOutcome};
use super::self_protection_classify::{damage_becomes_marked, source_has_effective_deathtouch};

pub struct StackAwarenessPolicy;

impl StackAwarenessPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            } => score_target(ctx, *id),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .map(|t| match t {
                    TargetRef::Object(id) => score_target(ctx, *id),
                    _ => 0.0,
                })
                .sum(),
            _ => 0.0,
        }
    }
}

impl TacticalPolicy for StackAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::StackAwareness
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("stack_awareness_score"),
        }
    }
}

fn score_target(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    score_target_redundancy(ctx, target_id)
        + score_counter_target_value(ctx, target_id)
        + score_pump_response(ctx, target_id)
}

fn score_target_redundancy(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    if is_spell_beneficial(ctx) {
        return 0.0;
    }

    if !has_pending_removal(ctx.state, target_id) {
        return 0.0;
    }

    if will_target_die_from_stack(ctx.state, target_id) {
        0.0
    } else {
        // Pending removal that might not kill — still penalize but less
        ctx.penalties().redundant_damage_penalty * 0.5
    }
}

/// Impact at which a stack entry is worth the AI's best counter, on the
/// [`assess_spell_impact`] scale. Two consumers share it: the last-counter
/// reservation in [`score_counter_target_value`] (below this, spending the AI's
/// only counter is penalized) and the cast-time impact scaling in
/// `effect_timing::counterspell_score` (at or above this, the counter cast keeps
/// its full stack-pressure bracket).
pub(crate) const COUNTER_IMPACT_THRESHOLD: f64 = 3.0;

/// Impact below which countering is card disadvantage: a counter trades exactly
/// one card, so a target worth less than one card is not worth casting it.
/// Used by `effect_timing::counterspell_score` as the floor of the impact ramp.
///
/// This is a *pricing* boundary, not a card-quality judgement: a 1/1 mana dork
/// prices at 1.05 (0.3 mana value + 0.75 body) and clears it, while Birds of
/// Paradise at 0.6 (0.3 + 0.3 for a 0/1 body) holds. Repricing belongs in
/// [`assess_spell_impact`], not in this constant.
pub(crate) const COUNTER_BREAK_EVEN_IMPACT: f64 = 1.0;

/// CR 701.6a: countering removes a spell from the stack. If `entry` is itself a
/// counter, report what countering *it* is worth to `ai_player`: the impact of
/// the most valuable AI-controlled stack object one of its `Counter` nodes acts
/// on ([`stack_entry_node_reach`]) and can counter, or `0.0` when there is none
/// (a rival counter aimed at a third player's spell is someone else's fight —
/// countering it buys the AI nothing).
///
/// Returns `None` when no node of `entry` is a counter, so callers can fall back
/// to [`assess_spell_impact`]. Single authority for the "which spell does this
/// foreign counter threaten" walk, shared with `effect_timing`.
pub(crate) fn foreign_counter_target_of_ai(
    state: &GameState,
    entry: &StackEntry,
    ai_player: PlayerId,
) -> Option<f64> {
    let reaches = stack_entry_node_reach(state, entry);
    let counters: Vec<&NodeReach<'_>> = reaches
        .iter()
        .filter(|reach| matches!(reach.node.effect, Effect::Counter { .. }))
        .collect();
    if counters.is_empty() {
        return None;
    }

    let mut worth = 0.0_f64;
    for target in counters.iter().flat_map(|reach| &reach.acted_on) {
        let TargetRef::Object(target_id) = target else {
            continue;
        };
        if let Some(threatened) = state.stack.iter().find(|e| e.id == *target_id) {
            // CR 101.2: a spell that can't be countered is not threatened.
            if threatened.controller == ai_player && spell_can_be_countered(state, *target_id) {
                worth = worth.max(assess_spell_impact(state, threatened));
            }
        }
    }
    Some(worth)
}

/// When the AI is casting a counter spell, score the target stack entry by its
/// impact. Higher-value spells (by mana value, creature stats, effects) should be
/// preferred counter targets. Returns 0.0 if the pending spell is not a counter.
fn score_counter_target_value(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    // Only applies when the AI's pending spell has a Counter effect
    let is_counter = ctx
        .effects()
        .iter()
        .any(|e| matches!(e, Effect::Counter { .. }));
    if !is_counter {
        return 0.0;
    }

    // Find the stack entry being targeted
    let Some(entry) = ctx.state.stack.iter().find(|e| e.id == target_id) else {
        return 0.0;
    };

    // Not a legitimate counter target: countering your own spell is almost always
    // wrong, and so is countering a rival's counter that is aimed at a third
    // player's spell — that resolves someone else's fight at the AI's expense.
    if entry.controller == ctx.ai_player
        || matches!(
            foreign_counter_target_of_ai(ctx.state, entry, ctx.ai_player),
            Some(worth) if worth <= 0.0
        )
    {
        return -10.0;
    }

    let mut score = assess_spell_impact(ctx.state, entry);

    // Last-counter reservation: if this is the AI's only counterspell, penalize
    // spending it on low-impact targets. Save it for something that matters.
    if score < COUNTER_IMPACT_THRESHOLD {
        let counters_in_hand =
            super::strategy_helpers::count_counterspells_in_hand(ctx.state, ctx.ai_player);
        if counters_in_hand == 1 {
            score += ctx.penalties().counter_last_reservation_penalty;
        }
    }

    // Low-MV creature penalty: scale by counter density in hand.
    // With many counters, save them for high-impact threats. With few counters,
    // the current threat IS the thing to counter — don't hold out for something better.
    if let Some(obj) = ctx.state.objects.get(&entry.source_id) {
        let is_cheap_creature = obj.mana_cost.mana_value() <= 2
            && obj
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature);
        if is_cheap_creature {
            let intent = crate::eval::strategic_intent(ctx.state, ctx.ai_player);
            if !matches!(intent, crate::eval::StrategicIntent::Stabilize) {
                let counters_in_hand =
                    super::strategy_helpers::count_counterspells_in_hand(ctx.state, ctx.ai_player);
                // 1 counter = no penalty, 2 = -0.3, 3+ = -0.6
                let penalty = -0.3 * (counters_in_hand as f64 - 1.0).clamp(0.0, 2.0);
                score += penalty;
            }
        }
    }

    score
}

/// When the AI's pending spell is harmful, boost targeting a creature that an
/// opponent is currently pumping on the stack — removing it wastes both the
/// creature and the pump spell (2-for-1).
fn score_pump_response(ctx: &PolicyContext<'_>, target_id: ObjectId) -> f64 {
    if is_spell_beneficial(ctx) {
        return 0.0;
    }

    // Skip if target is already dying — redundancy penalty handles that case
    if will_target_die_from_stack(ctx.state, target_id) {
        return 0.0;
    }

    let has_opponent_pump = ctx.state.stack.iter().any(|entry| {
        entry.controller != ctx.ai_player
            && stack_entry_node_reach(ctx.state, entry)
                .iter()
                .any(|reach| {
                    matches!(
                        reach.node.effect,
                        Effect::Pump { .. } | Effect::DoublePT { .. }
                    ) && acts_on(reach, target_id)
                })
    });

    if has_opponent_pump {
        ctx.penalties().pump_response_bonus
    } else {
        0.0
    }
}

/// Estimate the game impact of a stack entry based on its effects.
/// Used for counter-target valuation and protect-my-spell incentives.
pub(crate) fn assess_spell_impact(state: &GameState, entry: &StackEntry) -> f64 {
    match &entry.kind {
        StackEntryKind::Spell { .. } => {
            let mv = state
                .objects
                .get(&entry.source_id)
                .map(|o| o.mana_cost.mana_value())
                .unwrap_or(0) as f64;

            let mut score = mv * 0.3;

            let abilities = entry
                .ability()
                .map(collect_resolved_abilities)
                .unwrap_or_default();
            for ability in abilities {
                score += match &ability.effect {
                    Effect::ExtraTurn { count, .. } => {
                        engine::game::quantity::resolve_quantity_with_targets(state, count, ability)
                            .max(0) as f64
                            * 5.0
                    }
                    Effect::DestroyAll { .. }
                    | Effect::DamageAll { .. }
                    | Effect::ChangeZoneAll { .. } => 4.0,
                    Effect::GainControl { .. } | Effect::GainControlAll { .. } => 2.5,
                    Effect::Destroy { .. } | Effect::Fight { .. } => 1.5,
                    Effect::Counter { .. } => 1.5,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value },
                        ..
                    } => *value as f64 * 1.5,
                    Effect::DealDamage { .. } => 1.0,
                    Effect::SearchLibrary { .. } => 1.0,
                    Effect::Token { .. } => 0.5,
                    _ => 0.0,
                };
            }

            // Creature spells: factor in the creature's board value
            let creature_value = evaluate_creature(state, entry.source_id);
            if creature_value > 0.0 {
                score += creature_value * 0.3;
            }

            score.min(8.0)
        }
        // Activated/triggered abilities: moderate value — they're free to re-trigger.
        // KeywordAction (Crew/Equip/Saddle/Station) is similarly low-value to counter:
        // the cost was paid at announcement and the activation can be repeated.
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. } => 0.5,
        // Combat damage on the stack is neither a spell nor an ability, so no
        // counter can target it (CR 112.1 + CR 113.3b) and no protect-my-spell
        // incentive applies. This function values COUNTER TARGETS, so an entry
        // that can never be one is worth nothing to it.
        StackEntryKind::CombatDamage { .. } => 0.0,
    }
}

fn acts_on(reach: &NodeReach<'_>, object: ObjectId) -> bool {
    reach.acted_on.contains(&TargetRef::Object(object))
}

/// Whether another stack entry has a harmful node that acts on this object
/// ([`stack_entry_node_reach`]). An entry is not its own pending removal: a
/// spell that exiles itself does so only by resolving.
pub(crate) fn has_pending_removal(state: &GameState, target_id: ObjectId) -> bool {
    state
        .stack
        .iter()
        .filter(|entry| entry.id != target_id)
        .any(|entry| {
            stack_entry_node_reach(state, entry).iter().any(|reach| {
                acts_on(reach, target_id)
                    && matches!(effect_polarity(&reach.node.effect), EffectPolarity::Harmful)
            })
        })
}

/// Estimate whether pending stack effects will remove this object (creature or spell).
pub(crate) fn will_target_die_from_stack(state: &GameState, target_id: ObjectId) -> bool {
    let Some(object) = state.objects.get(&target_id) else {
        return false;
    };

    let mut dealt = Vec::new();

    for reach in nodes_acting_on(state, target_id) {
        match &reach.node.effect {
            // Destroy is lethal unless target is indestructible
            Effect::Destroy { .. } if !object.has_keyword(&Keyword::Indestructible) => {
                return true;
            }
            // Counter removes the spell from the stack. CR 101.2: unless
            // it can't be countered.
            Effect::Counter { .. } if spell_can_be_countered(state, target_id) => return true,
            // Bounce removes from battlefield
            Effect::Bounce { .. } => return true,
            // ChangeZone to non-battlefield removes from battlefield
            Effect::ChangeZone {
                destination: Zone::Exile | Zone::Graveyard | Zone::Hand | Zone::Library,
                ..
            } => {
                return true;
            }
            Effect::DealDamage { .. } => dealt.extend(node_damage(state, reach.node, object)),
            _ => {}
        }
    }

    damage_kills(object, &dealt, 0)
}

/// The nodes that act on `object_id` ([`stack_entry_node_reach`]) of every
/// stack entry except the one whose id is `object_id`, in the order the entries
/// resolve (CR 405.5: the top one first).
pub(crate) fn nodes_acting_on(state: &GameState, object_id: ObjectId) -> Vec<NodeReach<'_>> {
    state
        .stack
        .iter()
        .rev()
        .filter(|entry| entry.id != object_id)
        .flat_map(|entry| stack_entry_node_reach(state, entry))
        .filter(|reach| acts_on(reach, object_id))
        .collect()
}

/// CR 120.3: what a `DealDamage` node with a fixed amount does to `object`, on
/// the scale [`outcome_is_lethal`] judges. `None` for any other node.
///
/// The damage of a node that names no other source (`damage_source: None`) is
/// dealt by the node's own source: its deathtouch (CR 702.2b), wither and
/// infect (CR 120.3d) are read, and `object`'s protection from it prevents the
/// damage (CR 702.16e). Damage a node attributes to another source is counted
/// as marked damage.
pub(crate) fn node_damage(
    state: &GameState,
    node: &ResolvedAbility,
    object: &GameObject,
) -> Option<DamageOutcome> {
    let Effect::DealDamage {
        amount: QuantityExpr::Fixed { value },
        damage_source,
        ..
    } = &node.effect
    else {
        return None;
    };
    let amount = u32::try_from(*value).unwrap_or(0);
    let source = damage_source
        .is_none()
        .then(|| state.objects.get(&node.source_id))
        .flatten();
    // CR 702.16e: protection prevents the damage.
    if source.is_some_and(|source| protection_prevents_from(object, source)) {
        return Some(DamageOutcome::default());
    }
    // CR 120.3d: wither and infect damage is dealt to a creature as counters.
    let as_counters = object.card_types.core_types.contains(&CoreType::Creature)
        && damage_becomes_marked(state, source) == Some(false);
    Some(DamageOutcome {
        marked: if as_counters { 0 } else { amount },
        minus_counters: if as_counters { amount } else { 0 },
        deathtouch: amount > 0 && source_has_effective_deathtouch(state, source),
    })
}

/// CR 704.5f + CR 704.5g + CR 704.5h: whether `dealt`, dealt together, kills
/// `object` once it has `toughness_bonus` more toughness.
pub(crate) fn damage_kills(
    object: &GameObject,
    dealt: &[DamageOutcome],
    toughness_bonus: i32,
) -> bool {
    let total = dealt
        .iter()
        .fold(DamageOutcome::default(), |total, one| DamageOutcome {
            marked: total.marked.saturating_add(one.marked),
            minus_counters: total.minus_counters.saturating_add(one.minus_counters),
            deathtouch: total.deathtouch || one.deathtouch,
        });
    if toughness_bonus == 0 {
        return outcome_is_lethal(object, &total);
    }
    let mut pumped = object.clone();
    pumped.toughness = object
        .toughness
        .map(|toughness| toughness.saturating_add(toughness_bonus));
    outcome_is_lethal(&pumped, &total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        BounceSelection, EffectKind, QuantityRef, ResolvedAbility, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{
        GameState, PendingCast, StackEntry, StackEntryKind, TargetEffectDetail,
        TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    fn push_stack_entry(state: &mut GameState, effect: Effect, targets: Vec<TargetRef>) {
        let ability = ResolvedAbility::new(effect, targets, ObjectId(999), PlayerId(1));
        state.stack.push_back(StackEntry {
            id: ObjectId(state.next_object_id),
            source_id: ObjectId(999),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ability)),
                card_id: CardId(999),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        state.next_object_id += 1;
    }

    fn counter_effect() -> Effect {
        Effect::Counter {
            target: TargetFilter::StackSpell,
            source_rider: None,
            countered_spell_zone: None,
        }
    }

    fn extra_turn(count: QuantityExpr) -> Effect {
        Effect::ExtraTurn {
            target: TargetFilter::Controller,
            count,
        }
    }

    #[test]
    fn spell_impact_values_each_extra_turn_and_uses_the_owning_node_context() {
        let mut state = make_state();
        let unrelated_id = push_spell(&mut state, PlayerId(1), 0, Effect::NoOp, vec![]);
        let unrelated = state
            .stack
            .iter()
            .find(|entry| entry.id == unrelated_id)
            .unwrap();
        assert_eq!(assess_spell_impact(&state, unrelated), 0.0);

        let one_id = push_spell(
            &mut state,
            PlayerId(1),
            0,
            extra_turn(QuantityExpr::Fixed { value: 1 }),
            vec![],
        );
        let one = state.stack.iter().find(|entry| entry.id == one_id).unwrap();
        assert_eq!(assess_spell_impact(&state, one), 5.0);

        let two_id = push_spell(
            &mut state,
            PlayerId(1),
            0,
            extra_turn(QuantityExpr::Fixed { value: 2 }),
            vec![],
        );
        let two = state.stack.iter().find(|entry| entry.id == two_id).unwrap();
        assert_eq!(assess_spell_impact(&state, two), 8.0);

        let source_card_id = CardId(state.next_object_id);
        let source_id = create_object(
            &mut state,
            source_card_id,
            PlayerId(1),
            "Context Spell".into(),
            Zone::Stack,
        );
        let mut root = ResolvedAbility::new(Effect::NoOp, vec![], source_id, PlayerId(1));
        root.chosen_x = Some(1);
        let mut child = ResolvedAbility::new(
            extra_turn(QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            }),
            vec![],
            source_id,
            PlayerId(1),
        );
        child.chosen_x = Some(2);
        root.sub_ability = Some(Box::new(child));
        let mut root_one = root.clone();
        root_one
            .sub_ability
            .as_mut()
            .expect("dynamic child")
            .chosen_x = Some(1);
        let entry = StackEntry {
            id: ObjectId(state.next_object_id),
            source_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(root)),
                card_id: CardId(state.next_object_id),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        };
        assert_eq!(assess_spell_impact(&state, &entry), 8.0);
        let one_entry = StackEntry {
            id: ObjectId(state.next_object_id + 1),
            source_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(root_one)),
                card_id: CardId(state.next_object_id + 1),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        };
        assert_eq!(assess_spell_impact(&state, &one_entry), 5.0);
    }

    /// Sibling of [`push_stack_entry`] for multiplayer counter fixtures: the entry
    /// carries an explicit `controller` and is backed by a real object, so
    /// [`assess_spell_impact`] can read its mana value. Returns the entry id.
    fn push_spell(
        state: &mut GameState,
        controller: PlayerId,
        mana_value: u32,
        effect: Effect,
        targets: Vec<TargetRef>,
    ) -> ObjectId {
        let source_id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&source_id).unwrap().mana_cost = ManaCost::generic(mana_value);
        let ability = ResolvedAbility::new(effect, targets, source_id, controller);
        let id = source_id;
        state.stack.push_back(StackEntry {
            id,
            source_id,
            controller,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ability)),
                card_id: CardId(id.0),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        id
    }

    fn make_target_ctx(
        _state: &GameState,
        target_id: ObjectId,
        source_effect: Effect,
    ) -> (AiDecisionContext, CandidateAction) {
        let ability = ResolvedAbility::new(source_effect, Vec::new(), ObjectId(888), PlayerId(1));
        let pending_cast = PendingCast::new(ObjectId(888), CardId(888), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(1),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(target_id)],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(1)), TacticalClass::Target),
        };
        (decision, candidate)
    }

    fn score_policy(
        state: &GameState,
        decision: &AiDecisionContext,
        candidate: &CandidateAction,
    ) -> f64 {
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(1),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        StackAwarenessPolicy.score(&ctx)
    }

    // --- Helper tests ---

    #[test]
    fn has_pending_removal_finds_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(has_pending_removal(&state, creature));
    }

    #[test]
    fn has_pending_removal_ignores_different_target() {
        let mut state = make_state();
        let creature_a = add_creature(&mut state, PlayerId(0), 3, 3);
        let creature_b = add_creature(&mut state, PlayerId(0), 2, 2);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature_a)],
        );
        assert!(!has_pending_removal(&state, creature_b));
    }

    #[test]
    fn will_target_die_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_indestructible_survives_destroy() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(!will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_lethal_damage() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 2, 3);
        push_stack_entry(
            &mut state,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_insufficient_damage() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 2, 4);
        push_stack_entry(
            &mut state,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(!will_target_die_from_stack(&state, creature));
    }

    #[test]
    fn will_target_die_bounce() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        push_stack_entry(
            &mut state,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(creature)],
        );
        assert!(will_target_die_from_stack(&state, creature));
    }

    // --- Policy-level tests ---

    #[test]
    fn no_penalty_when_different_targets() {
        let mut state = make_state();
        let creature_a = add_creature(&mut state, PlayerId(0), 3, 3);
        let creature_b = add_creature(&mut state, PlayerId(0), 2, 2);
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature_a)],
        );

        let (decision, candidate) = make_target_ctx(
            &state,
            creature_b,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score.abs() < 0.01,
            "No penalty when targeting different creature, got {score}"
        );
    }

    #[test]
    fn empty_stack_no_penalty() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);

        let (decision, candidate) = make_target_ctx(
            &state,
            creature,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score.abs() < 0.01,
            "No penalty with empty stack, got {score}"
        );
    }

    #[test]
    fn indestructible_not_penalized_second_removal() {
        let mut state = make_state();
        let creature = add_creature(&mut state, PlayerId(0), 3, 3);
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        // First Destroy won't kill it (indestructible)
        push_stack_entry(
            &mut state,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
        );

        // Second removal should still get partial penalty (there IS pending removal,
        // just not lethal)
        let (decision, candidate) = make_target_ctx(
            &state,
            creature,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        let score = score_policy(&state, &decision, &candidate);
        // Should get partial penalty (redundant_damage * 0.5), not full redundant_removal
        assert!(
            score < 0.0 && score > -5.0,
            "Should get partial penalty for indestructible, got {score}"
        );
    }

    /// Three players: B (P0) counters `victim_controller`'s spell. When the victim
    /// is C (P2), countering B's counter only resolves C's spell — the AI (P1)
    /// would be fighting someone else's fight for a card. Returns B's entry id.
    fn three_player_counter_war(victim_controller: PlayerId) -> (GameState, ObjectId) {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        let victim = push_spell(&mut state, victim_controller, 4, Effect::NoOp, Vec::new());
        let rival_counter = push_spell(
            &mut state,
            PlayerId(0),
            2,
            counter_effect(),
            vec![TargetRef::Object(victim)],
        );
        (state, rival_counter)
    }

    #[test]
    fn counter_target_rival_counter_on_third_party_spell_is_penalised() {
        let (state, rival_counter) = three_player_counter_war(PlayerId(2));
        let (decision, candidate) = make_target_ctx(&state, rival_counter, counter_effect());
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score <= -10.0,
            "A rival counter aimed at a third player's spell must not be a counter \
             target, got {score}"
        );
    }

    #[test]
    fn counter_target_rival_counter_on_own_spell_is_valued() {
        // PlayerId(1) is the AI seat that `score_policy` scores from.
        let (state, rival_counter) = three_player_counter_war(PlayerId(1));
        let (decision, candidate) = make_target_ctx(&state, rival_counter, counter_effect());
        let score = score_policy(&state, &decision, &candidate);
        assert!(
            score > 0.0,
            "A rival counter aimed at the AI's own spell is a legitimate counter \
             target, got {score}"
        );
    }

    const SWALLOWED_BY_LEVIATHAN: &str = "Choose target spell. Surveil 2, then counter the chosen spell unless its controller pays {1} for each card in your graveyard. (To surveil 2, look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)";
    const BOON_OF_EREBOS: &str =
        "Target creature gets +2/+0 until end of turn. Regenerate it. You lose 2 life.";
    const TAIL_SWIPE: &str = "Choose target creature you control and target creature you don't control. If you cast this spell during your main phase, the creature you control gets +1/+1 until end of turn. Then those creatures fight each other. (Each deals damage equal to its power to the other.)";
    const SELF_DESTRUCT: &str = "Target creature you control deals X damage to any other target and X damage to itself, where X is its power.";
    const ARC_TRAIL: &str =
        "Arc Trail deals 2 damage to any target and 1 damage to any other target.";
    const DISSECTION_PRACTICE: &str = "Target opponent loses 1 life and you gain 1 life.\nUp to one target creature gets +1/+1 until end of turn.\nUp to one target creature gets -1/-1 until end of turn.";
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    const TEFERIS_PROTECTION: &str = "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.";

    /// The chain of the stack entry `id`, node by node.
    fn chain_nodes(state: &GameState, id: ObjectId) -> Vec<ResolvedAbility> {
        let entry = state.stack.iter().find(|e| e.id == id).expect("entry");
        collect_resolved_abilities(entry.ability().expect("ability"))
            .into_iter()
            .cloned()
            .collect()
    }

    /// A stack object for a spell `controller` casts, with mana value `mana_value`.
    fn spell_object(state: &mut GameState, controller: PlayerId, mana_value: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(mana_value);
        id
    }

    /// Pushes `root` as a spell entry that shares its source object's id, as a
    /// cast spell's entry does.
    fn push_chain(state: &mut GameState, controller: PlayerId, root: ResolvedAbility) -> ObjectId {
        let id = root.source_id;
        state.stack.push_back(StackEntry {
            id,
            source_id: id,
            controller,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(root)),
                card_id: CardId(id.0),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        id
    }

    fn target_only(target: ObjectId, source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            source,
            controller,
        )
    }

    fn destroy_node(
        filter: TargetFilter,
        targets: Vec<TargetRef>,
        source: ObjectId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Destroy {
                target: filter,
                cant_regenerate: false,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    fn main_phase_runner(
        scenario: engine::game::scenario::GameScenario,
    ) -> engine::game::scenario::GameRunner {
        use engine::game::scenario::P0;
        use engine::types::phase::Phase;
        let mut runner = scenario.build();
        let s = runner.state_mut();
        s.turn_number = 3;
        s.active_player = P0;
        s.phase = Phase::PreCombatMain;
        s.waiting_for = WaitingFor::Priority { player: P0 };
        runner
    }

    #[test]
    fn swallowed_by_leviathan_still_answers_the_spell_it_chose() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario
            .add_creature_to_hand(P0, "Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let swallowed = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Swallowed by Leviathan",
                true,
                SWALLOWED_BY_LEVIATHAN,
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            })
            .id();
        for _ in 0..3 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        // The counter's unless cost counts these, so it is not {0}.
        for _ in 0..3 {
            scenario.add_creature_to_graveyard(P1, "Dead", 1, 1);
        }
        let mut runner = main_phase_runner(scenario);
        runner.cast(bear).commit();
        let bear_spell = runner.state().stack.back().expect("the bear spell").id;
        engine::game::engine::apply_as_current_for_simulation(
            runner.state_mut(),
            GameAction::PassPriority,
        )
        .expect("P0 passes priority");
        runner.cast(swallowed).target_object(bear_spell).commit();
        let state = runner.state();
        let entry = state.stack.back().expect("Swallowed by Leviathan").clone();
        let counter = chain_nodes(state, entry.id)
            .into_iter()
            .find(|node| matches!(node.effect, Effect::Counter { .. }))
            .expect("reach guard: the chain counters");
        assert!(
            counter.targets.is_empty()
                && counter.effect.target_filter() == Some(&TargetFilter::Any),
            "reach guard: the counter declares nothing and names no parent anaphor"
        );
        assert!(has_pending_removal(state, bear_spell));
        assert!(will_target_die_from_stack(state, bear_spell));
        assert!(foreign_counter_target_of_ai(state, &entry, P0).is_some_and(|worth| worth > 0.0));
    }

    #[test]
    fn boon_of_erebos_does_not_make_its_creature_pending_removal() {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
        let boon = scenario
            .add_spell_to_hand_from_oracle(P0, "Boon of Erebos", true, BOON_OF_EREBOS)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P0, ManaColor::Black);
        let mut runner = scenario.build();
        runner.cast(boon).target_object(bear).commit();
        let state = runner.state();
        let nodes = chain_nodes(state, state.stack.back().expect("Boon").id);
        assert!(
            nodes[0].targets.contains(&TargetRef::Object(bear)),
            "reach guard: the chain targets the creature"
        );
        assert!(
            nodes
                .iter()
                .any(|node| matches!(effect_polarity(&node.effect), EffectPolarity::Harmful)),
            "reach guard: the chain has a harmful node"
        );
        assert!(!has_pending_removal(state, bear));
    }

    #[test]
    fn tail_swipe_harms_both_fighters() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own = scenario.add_creature(P0, "Own", 3, 3).id();
        let theirs = scenario.add_creature(P1, "Theirs", 4, 4).id();
        let swipe = scenario
            .add_spell_to_hand_from_oracle(P0, "Tail Swipe", true, TAIL_SWIPE)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        runner.cast(swipe).target_objects(&[own, theirs]).commit();
        let state = runner.state();
        let nodes = chain_nodes(state, state.stack.back().expect("Tail Swipe").id);
        assert_eq!(
            nodes[0].targets,
            vec![TargetRef::Object(own)],
            "reach guard: the root declares only the caster's creature"
        );
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node.effect, Effect::Fight { .. }) && node.targets.is_empty()),
            "reach guard: the fight declares nothing"
        );
        assert!(has_pending_removal(state, own));
        assert!(has_pending_removal(state, theirs));
    }

    #[test]
    fn self_destruct_harms_the_target_its_later_node_declares() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own = scenario.add_creature(P0, "Own", 3, 3).id();
        let other = scenario.add_creature(P1, "Other", 3, 3).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Self-Destruct", true, SELF_DESTRUCT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            })
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        let mut runner = scenario.build();
        runner.cast(card).target_objects(&[own, other]).commit();
        let state = runner.state();
        let nodes = chain_nodes(state, state.stack.back().expect("Self-Destruct").id);
        assert!(
            !nodes[0].targets.contains(&TargetRef::Object(other)),
            "reach guard: the root does not target the other creature"
        );
        assert!(
            has_pending_removal(state, own),
            "control: the damage the creature deals itself is read"
        );
        assert!(has_pending_removal(state, other));
    }

    #[test]
    fn harm_aimed_elsewhere_in_the_chain_is_not_removal_of_the_roots_target() {
        let mut state = make_state();
        let aimed = add_creature(&mut state, PlayerId(1), 2, 2);
        let harmed = add_creature(&mut state, PlayerId(1), 2, 2);
        let source = spell_object(&mut state, PlayerId(0), 2);
        let mut root = target_only(aimed, source, PlayerId(0));
        root.sub_ability = Some(Box::new(destroy_node(
            TargetFilter::Any,
            vec![TargetRef::Object(harmed)],
            source,
        )));
        push_chain(&mut state, PlayerId(0), root);
        assert!(
            has_pending_removal(&state, harmed),
            "reach guard: the harmful child is read"
        );
        assert!(!has_pending_removal(&state, aimed));
    }

    #[test]
    fn a_harmful_child_inheriting_the_roots_target_is_removal_of_it() {
        let mut state = make_state();
        let aimed = add_creature(&mut state, PlayerId(1), 2, 2);
        let source = spell_object(&mut state, PlayerId(0), 2);
        let mut root = target_only(aimed, source, PlayerId(0));
        assert_eq!(
            effect_polarity(&root.effect),
            EffectPolarity::Contextual,
            "reach guard: the root is not harmful"
        );
        root.sub_ability = Some(Box::new(destroy_node(
            TargetFilter::ParentTarget,
            Vec::new(),
            source,
        )));
        push_chain(&mut state, PlayerId(0), root);
        assert!(has_pending_removal(&state, aimed));
        assert!(will_target_die_from_stack(&state, aimed));
    }

    #[test]
    fn arc_trail_kills_the_creature_its_second_node_damages() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let grazed = scenario.add_creature(P1, "Grazed", 3, 3).id();
        let killed = scenario.add_creature(P1, "Killed", 1, 1).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Arc Trail", true, ARC_TRAIL)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            })
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        let mut runner = scenario.build();
        runner.cast(card).target_objects(&[grazed, killed]).commit();
        let state = runner.state();
        let nodes = chain_nodes(state, state.stack.back().expect("Arc Trail").id);
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.targets.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![TargetRef::Object(grazed)],
                vec![TargetRef::Object(killed)]
            ],
            "reach guard: each damage node declares its own creature"
        );
        assert!(will_target_die_from_stack(state, killed));
        assert!(!will_target_die_from_stack(state, grazed));
    }

    #[test]
    fn dissection_practice_pump_below_its_life_loss_earns_no_pump_response() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let pumped = scenario.add_creature(P0, "Pumped", 2, 2).id();
        let shrunk = scenario.add_creature(P1, "Shrunk", 3, 3).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Dissection Practice", true, DISSECTION_PRACTICE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P0, ManaColor::Black);
        let mut runner = scenario.build();
        runner
            .cast(card)
            .target_player(P1)
            .target_objects(&[pumped, shrunk])
            .commit();
        let state = runner.state();
        let nodes = chain_nodes(state, state.stack.back().expect("Dissection Practice").id);
        assert!(
            !nodes[0].targets.contains(&TargetRef::Object(pumped)),
            "reach guard: the root does not target the pumped creature"
        );
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node.effect, Effect::Pump { .. })
                    && node.targets == vec![TargetRef::Object(pumped)]),
            "reach guard: a pump node declares it"
        );
        // The AI in `make_target_ctx` is `PlayerId(1)`; the pump is P0's.
        let pump_response = |state: &GameState| {
            let (decision, candidate) = make_target_ctx(
                state,
                pumped,
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            );
            score_policy(state, &decision, &candidate)
        };
        // The pump follows the life loss, which the engine does not answer
        // past.
        let score = pump_response(state);
        assert_eq!(score, 0.0);

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let pumped = scenario.add_creature(P0, "Pumped", 2, 2).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        runner.cast(card).target_object(pumped).commit();
        let (decision, candidate) = make_target_ctx(
            runner.state(),
            pumped,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let score = score_policy(runner.state(), &decision, &candidate);
        assert!(
            score > 0.0,
            "reach guard: a pump that is its entry's first instruction earns the response, got {score}"
        );
    }

    #[test]
    fn a_counter_aimed_elsewhere_in_the_chain_does_not_threaten_the_ais_spell() {
        let mut state = make_state();
        let ai_spell = spell_object(&mut state, PlayerId(1), 4);
        push_chain(
            &mut state,
            PlayerId(1),
            ResolvedAbility::new(Effect::NoOp, Vec::new(), ai_spell, PlayerId(1)),
        );
        let rival_spell = spell_object(&mut state, PlayerId(0), 4);
        push_chain(
            &mut state,
            PlayerId(0),
            ResolvedAbility::new(Effect::NoOp, Vec::new(), rival_spell, PlayerId(0)),
        );
        let worth = |state: &mut GameState, countered: ObjectId| {
            let source = spell_object(state, PlayerId(0), 2);
            let mut root = ResolvedAbility::new(
                Effect::TargetOnly {
                    target: TargetFilter::StackSpell,
                },
                vec![TargetRef::Object(ai_spell)],
                source,
                PlayerId(0),
            );
            root.sub_ability = Some(Box::new(ResolvedAbility::new(
                counter_effect(),
                vec![TargetRef::Object(countered)],
                source,
                PlayerId(0),
            )));
            let id = push_chain(state, PlayerId(0), root);
            let entry = state.stack.iter().find(|e| e.id == id).cloned().unwrap();
            let worth = foreign_counter_target_of_ai(state, &entry, PlayerId(1));
            state.stack.pop_back();
            worth
        };
        assert!(
            worth(&mut state, ai_spell).is_some_and(|worth| worth > 0.0),
            "reach guard: a counter node aimed at the AI's spell is read"
        );
        assert_eq!(worth(&mut state, rival_spell), Some(0.0));
    }

    #[test]
    fn an_entry_below_a_stack_object_does_not_answer_it() {
        let mut state = make_state();
        let spell = spell_object(&mut state, PlayerId(1), 4);
        push_chain(
            &mut state,
            PlayerId(1),
            ResolvedAbility::new(Effect::NoOp, Vec::new(), spell, PlayerId(1)),
        );
        let counter_source = spell_object(&mut state, PlayerId(0), 2);
        let counter = push_chain(
            &mut state,
            PlayerId(0),
            ResolvedAbility::new(
                counter_effect(),
                vec![TargetRef::Object(spell)],
                counter_source,
                PlayerId(0),
            ),
        );
        let counter_entry = |state: &GameState| {
            state
                .stack
                .iter()
                .find(|e| e.id == counter)
                .cloned()
                .unwrap()
        };
        assert!(
            has_pending_removal(&state, spell),
            "control: above the spell"
        );
        assert!(
            will_target_die_from_stack(&state, spell),
            "control: above the spell"
        );
        assert!(
            foreign_counter_target_of_ai(&state, &counter_entry(&state), PlayerId(1))
                .is_some_and(|worth| worth > 0.0),
            "control: above the spell"
        );
        state.stack.swap(0, 1);
        assert_eq!(
            state.stack.back().map(|e| e.id),
            Some(spell),
            "reach guard: the spell now resolves first"
        );
        assert!(!has_pending_removal(&state, spell));
        assert!(!will_target_die_from_stack(&state, spell));
        assert_eq!(
            foreign_counter_target_of_ai(&state, &counter_entry(&state), PlayerId(1)),
            Some(0.0)
        );
    }

    #[test]
    fn a_spell_that_exiles_itself_is_not_its_own_pending_removal() {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Teferi's Protection", true, TEFERIS_PROTECTION)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.add_basic_land(P0, ManaColor::White);
        let mut runner = scenario.build();
        runner.cast(card).commit();
        // The exile follows an instruction the engine does not answer past,
        // so it does not answer the exile; with the instructions before it
        // made `NoOp`s it does.
        let mut state = runner.state().clone();
        let entry_id = state.stack.back().expect("Teferi's Protection").id;
        let mut node = state
            .stack
            .back_mut()
            .and_then(StackEntry::ability_mut)
            .expect("Teferi's Protection");
        while node.sub_ability.is_some() {
            node.effect = Effect::NoOp;
            node = node.sub_ability.as_deref_mut().expect("a node below");
        }
        let entry = state.stack.back().expect("Teferi's Protection");
        assert!(
            stack_entry_node_reach(&state, entry).iter().any(|reach| {
                acts_on(reach, entry_id)
                    && matches!(effect_polarity(&reach.node.effect), EffectPolarity::Harmful)
            }),
            "reach guard: a harmful node of the spell acts on the spell itself"
        );
        assert!(!has_pending_removal(&state, entry_id));
        assert!(!will_target_die_from_stack(&state, entry_id));
    }

    const CHANCELLOR_OF_THE_ANNEX: &str = "You may reveal this card from your opening hand. If you do, when each opponent casts their first spell of the game, counter that spell unless that player pays {1}.\nFlying\nWhenever an opponent casts a spell, counter it unless that player pays {1}.";

    #[test]
    fn a_counter_trigger_threatens_the_spell_that_triggered_it() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let chancellor = scenario
            .add_creature_from_oracle(P0, "Chancellor of the Annex", 5, 6, CHANCELLOR_OF_THE_ANNEX)
            .id();
        let bear = scenario
            .add_creature_to_hand(P1, "Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Green);
        }
        let mut runner = scenario.build();
        let s = runner.state_mut();
        s.turn_number = 3;
        s.active_player = P1;
        s.waiting_for = WaitingFor::Priority { player: P1 };
        s.priority_player = P1;
        runner.cast(bear).commit();
        for _ in 0..6 {
            if runner.state().stack.back().map(|e| e.source_id) == Some(chancellor) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
        let state = runner.state();
        let entry = state
            .stack
            .back()
            .filter(|e| e.source_id == chancellor)
            .expect("reach guard: the Chancellor trigger is on top");
        assert!(
            entry.ability().is_some_and(|root| root.targets.is_empty()),
            "reach guard: the trigger declares no target"
        );
        assert!(foreign_counter_target_of_ai(state, entry, P1).is_some_and(|worth| worth > 0.0));
        assert!(has_pending_removal(state, bear));
    }

    const SHOCK: &str = "Shock deals 2 damage to any target.";
    const CANCEL: &str = "Counter target spell.";
    const SUPREME_VERDICT: &str = "This spell can't be countered.\nDestroy all creatures.";
    const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
    const KRAUL_HARPOONER: &str = "Reach\nUndergrowth — When this creature enters, choose up to one target creature you don't control with flying. This creature gets +X/+0 until end of turn, where X is the number of creature cards in your graveyard, then you may have this creature fight that creature.";

    /// P0 casts Shock at P1's creature with toughness `toughness` and
    /// `keywords`.
    fn shock_at(
        toughness: i32,
        keywords: &[Keyword],
    ) -> (engine::game::scenario::GameRunner, ObjectId) {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::{ManaColor, ManaCostShard};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = {
            let mut builder = scenario.add_creature(P1, "Victim", 2, toughness);
            for keyword in keywords {
                builder.with_keyword(keyword.clone());
            }
            builder.id()
        };
        let shock = scenario
            .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P0, ManaColor::Red);
        let mut runner = scenario.build();
        runner.cast(shock).target_object(victim).commit();
        (runner, victim)
    }

    #[test]
    fn indestructible_survives_lethal_damage_on_the_stack() {
        let (runner, victim) = shock_at(2, &[]);
        assert!(
            will_target_die_from_stack(runner.state(), victim),
            "reach guard: 2 damage kills a plain 2-toughness creature"
        );
        let (runner, victim) = shock_at(2, &[Keyword::Indestructible]);
        assert!(
            has_pending_removal(runner.state(), victim),
            "reach guard: the damage node acts on the creature"
        );
        assert!(!will_target_die_from_stack(runner.state(), victim));
    }

    /// A 1/1 of P1's with `keyword` pings P0's creature with toughness
    /// `toughness` and `victim_keywords`: the activation, on the stack.
    fn ping_at(
        keyword: Option<Keyword>,
        toughness: i32,
        victim_keywords: &[Keyword],
    ) -> (GameState, ObjectId) {
        use engine::game::ability_utils::build_resolved_from_def_with_targets;
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = {
            let mut builder = scenario.add_creature(P0, "Victim", 1, toughness);
            for keyword in victim_keywords {
                builder.with_keyword(keyword.clone());
            }
            builder.id()
        };
        let pinger = {
            let mut builder = scenario.add_creature_from_oracle(
                P1,
                "Prodigal Pyromancer",
                1,
                1,
                PRODIGAL_PYROMANCER,
            );
            if let Some(keyword) = keyword {
                builder.with_keyword(keyword);
            }
            builder.id()
        };
        let mut state = scenario.build().state().clone();
        let ability = build_resolved_from_def_with_targets(
            &state.objects[&pinger].abilities[0],
            pinger,
            P1,
            vec![TargetRef::Object(victim)],
        );
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.push_back(StackEntry {
            id,
            source_id: pinger,
            controller: P1,
            kind: StackEntryKind::ActivatedAbility {
                source_id: pinger,
                ability: Box::new(ability),
            },
        });
        (state, victim)
    }

    #[test]
    fn deathtouch_and_wither_are_read_from_the_damage_source() {
        let (state, victim) = ping_at(None, 5, &[]);
        assert!(
            has_pending_removal(&state, victim),
            "reach guard: the ping acts on the creature"
        );
        assert!(!will_target_die_from_stack(&state, victim));
        let (state, victim) = ping_at(Some(Keyword::Deathtouch), 5, &[]);
        assert!(will_target_die_from_stack(&state, victim));

        let (state, victim) = ping_at(None, 1, &[Keyword::Indestructible]);
        assert!(
            !will_target_die_from_stack(&state, victim),
            "reach guard: marked damage does not kill an indestructible creature"
        );
        let (state, victim) = ping_at(Some(Keyword::Wither), 1, &[Keyword::Indestructible]);
        assert!(will_target_die_from_stack(&state, victim));
    }

    #[test]
    fn damage_another_object_deals_is_not_read_from_the_nodes_source() {
        let (mut state, victim) = ping_at(Some(Keyword::Deathtouch), 5, &[]);
        let dealer = add_creature(&mut state, PlayerId(1), 1, 1);
        let node = state
            .stack
            .back_mut()
            .and_then(StackEntry::ability_mut)
            .expect("the ping");
        let Effect::DealDamage { damage_source, .. } = &mut node.effect else {
            panic!("the ping deals damage");
        };
        *damage_source = Some(engine::types::ability::DamageSource::Target);
        node.targets = vec![TargetRef::Object(dealer), TargetRef::Object(victim)];
        let entry = state.stack.back().expect("the ping");
        assert!(
            stack_entry_node_reach(&state, entry)
                .iter()
                .any(|reach| acts_on(reach, victim)),
            "reach guard: the damage acts on the creature"
        );
        assert!(!will_target_die_from_stack(&state, victim));
    }

    #[test]
    fn a_spell_that_cant_be_countered_is_not_threatened_by_a_counter() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        for uncounterable in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = if uncounterable {
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Supreme Verdict", false, SUPREME_VERDICT)
                    .with_mana_cost(ManaCost::generic(1))
                    .id()
            } else {
                scenario
                    .add_creature_to_hand(P0, "Bear", 2, 2)
                    .with_mana_cost(ManaCost::generic(1))
                    .id()
            };
            scenario.add_basic_land(P0, ManaColor::White);
            let cancel = scenario
                .add_spell_to_hand_from_oracle(P1, "Cancel", true, CANCEL)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            scenario.add_basic_land(P1, ManaColor::Blue);
            let mut runner = main_phase_runner(scenario);
            runner.cast(spell).commit();
            let spell = runner.state().stack.back().expect("the AI's spell").id;
            engine::game::engine::apply_as_current_for_simulation(
                runner.state_mut(),
                GameAction::PassPriority,
            )
            .expect("P0 passes priority");
            runner.cast(cancel).target_object(spell).commit();
            let state = runner.state();
            let entry = state.stack.back().expect("Cancel");
            assert!(
                stack_entry_node_reach(state, entry)
                    .iter()
                    .any(|reach| acts_on(reach, spell)),
                "reach guard: the counter acts on the spell"
            );
            let worth = foreign_counter_target_of_ai(state, entry, P0);
            if uncounterable {
                assert!(!will_target_die_from_stack(state, spell));
                assert_eq!(worth, Some(0.0));
            } else {
                assert!(will_target_die_from_stack(state, spell), "control");
                assert!(worth.is_some_and(|worth| worth > 0.0), "control");
            }
        }
    }

    /// `score_pump_response` for `PlayerId(1)` choosing `target` for a
    /// destroy spell.
    fn pump_response(state: &GameState, target: ObjectId) -> f64 {
        let (decision, candidate) = make_target_ctx(
            state,
            target,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(1),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        score_pump_response(&ctx, target)
    }

    #[test]
    fn kraul_harpooner_pumps_itself_not_the_creature_it_chose() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let flyer = scenario.add_creature(P1, "Flyer", 2, 2).flying().id();
        let harpooner = scenario
            .add_creature_to_hand_from_oracle(P0, "Kraul Harpooner", 3, 2, KRAUL_HARPOONER)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        scenario.add_basic_land(P0, ManaColor::Green);
        for _ in 0..2 {
            scenario.add_creature_to_graveyard(P0, "Dead", 1, 1);
        }
        let mut runner = scenario.build();
        runner.cast(harpooner).commit();
        runner.resolve_top();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(flyer)),
            })
            .expect("the trigger chooses the flyer");
        let state = runner.state();
        let entry = state.stack.back().expect("the Harpooner trigger");
        assert_eq!(
            entry.ability().map(|root| root.targets.clone()),
            Some(vec![TargetRef::Object(flyer)]),
            "reach guard: the root declares only the flyer"
        );
        assert!(
            stack_entry_node_reach(state, entry).iter().any(|reach| {
                matches!(reach.node.effect, Effect::Pump { .. }) && acts_on(reach, harpooner)
            }),
            "reach guard: the pump node acts on the Harpooner"
        );
        assert!(pump_response(state, harpooner) > 0.0);
        assert_eq!(pump_response(state, flyer), 0.0);
    }

    #[test]
    fn a_pumped_creature_already_dying_to_deathtouch_earns_no_pump_response() {
        use engine::game::scenario::P0;
        for deathtouch in [false, true] {
            // The AI's ping, answered by P0's pump.
            let (mut state, victim) = ping_at(deathtouch.then_some(Keyword::Deathtouch), 5, &[]);
            let pump_source = spell_object(&mut state, P0, 1);
            push_chain(
                &mut state,
                P0,
                ResolvedAbility::new(
                    Effect::Pump {
                        power: engine::types::ability::PtValue::Fixed(3),
                        toughness: engine::types::ability::PtValue::Fixed(3),
                        target: TargetFilter::Any,
                    },
                    vec![TargetRef::Object(victim)],
                    pump_source,
                    P0,
                ),
            );
            let score = pump_response(&state, victim);
            if deathtouch {
                assert_eq!(score, 0.0);
            } else {
                assert!(
                    score > 0.0,
                    "reach guard: the pump earns the response, got {score}"
                );
            }
        }
    }
}
