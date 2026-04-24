use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::combat::AttackTarget;
use engine::types::ability::{AbilityCondition, Effect, PtValue, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::policies::context::{collect_ability_effects, PolicyContext};
use crate::policies::effect_classify::{effect_polarity, targets_creatures_only, EffectPolarity};
use crate::policies::stack_awareness::{has_pending_removal, will_target_die_from_stack};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateDecision {
    Reject,
    Allow,
    AllowWithPenalty(f64),
}

#[derive(Debug, Clone)]
pub struct GatedCandidate {
    pub candidate: CandidateAction,
    pub penalty: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TacticalWindow {
    OwnPreCombatMain,
    OwnPostCombatMain,
    OpponentMain,
    CombatBeforeBlocks,
    CombatAfterBlocks,
    CombatDamage,
    StackResponse,
    EndStep,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct TacticalFacts {
    window: TacticalWindow,
    live_stack_response: bool,
    pass_preserves_stronger_window: bool,
}

impl TacticalFacts {
    fn derive(state: &GameState, ai_player: PlayerId) -> Self {
        let live_stack_response = !state.stack.is_empty();
        let own_turn = engine::game::turn_control::turn_decision_maker(state) == ai_player;
        let window = if live_stack_response {
            TacticalWindow::StackResponse
        } else {
            match state.phase {
                Phase::PreCombatMain if own_turn => TacticalWindow::OwnPreCombatMain,
                Phase::PostCombatMain if own_turn => TacticalWindow::OwnPostCombatMain,
                Phase::PreCombatMain | Phase::PostCombatMain => TacticalWindow::OpponentMain,
                Phase::BeginCombat | Phase::DeclareAttackers => TacticalWindow::CombatBeforeBlocks,
                Phase::DeclareBlockers | Phase::EndCombat => TacticalWindow::CombatAfterBlocks,
                Phase::CombatDamage => TacticalWindow::CombatDamage,
                Phase::End | Phase::Cleanup => TacticalWindow::EndStep,
                _ => TacticalWindow::Other,
            }
        };
        let pass_preserves_stronger_window = own_turn
            && state.stack.is_empty()
            && matches!(
                state.phase,
                Phase::PreCombatMain | Phase::BeginCombat | Phase::DeclareAttackers
            );

        Self {
            window,
            live_stack_response,
            pass_preserves_stronger_window,
        }
    }
}

pub fn gate_candidates(
    state: &GameState,
    decision: &AiDecisionContext,
    candidates: Vec<CandidateAction>,
    ai_player: PlayerId,
    config: &AiConfig,
    context: &AiContext,
) -> Vec<GatedCandidate> {
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let decision_result = {
                let policy_ctx = PolicyContext {
                    state,
                    decision,
                    candidate: &candidate,
                    ai_player,
                    config,
                    context,
                    cast_facts: None,
                };
                assess_candidate(&policy_ctx)
            };
            match decision_result {
                GateDecision::Reject => None,
                GateDecision::Allow => Some(GatedCandidate {
                    candidate,
                    penalty: 0.0,
                }),
                GateDecision::AllowWithPenalty(penalty) => {
                    Some(GatedCandidate { candidate, penalty })
                }
            }
        })
        .collect()
}

fn assess_candidate(ctx: &PolicyContext<'_>) -> GateDecision {
    match &ctx.candidate.action {
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => assess_pre_cast(ctx),
        GameAction::ChooseTarget {
            target: Some(target),
        } => {
            let penalty = target_choice_penalty(ctx, target);
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        GameAction::ChooseTarget { target: None } => GateDecision::Allow,
        GameAction::SelectTargets { targets } => {
            let penalty = targets
                .iter()
                .map(|target| target_choice_penalty(ctx, target))
                .sum::<f64>();
            if penalty < 0.0 {
                GateDecision::AllowWithPenalty(penalty)
            } else {
                GateDecision::Allow
            }
        }
        // CR 601.2: Announcing a spell commits the caster — the rules provide
        // no strategic rewind. CancelCast exists only as a mechanical escape
        // when the cast cannot be completed (no legal targets after a
        // replacement effect, unaffordable cost after a cost-increase static).
        // Removing it from the strategic pool prevents regret-based cast/cancel
        // loops — once a ChooseTarget or pay-cost option exists, the AI must
        // pick one. The genuine-escape cases fall through to
        // `search::fallback_action`, which emits CancelCast when the scored
        // pool is empty.
        GameAction::CancelCast => GateDecision::Reject,
        _ => GateDecision::Allow,
    }
}

fn assess_pre_cast(ctx: &PolicyContext<'_>) -> GateDecision {
    // CR 608.2c: Reject abilities whose source-type condition is known to fail.
    // E.g. Figure of Fable's "{1}{G/W}{G/W}: If this creature is a Scout, ..." when
    // the source is not currently a Scout. The ability is legal to activate but wastes mana.
    if let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    {
        if let Some(object) = ctx.state.objects.get(source_id) {
            if let Some(ability_def) = object.abilities.get(*ability_index) {
                if let Some(AbilityCondition::SourceMatchesFilter { ref filter }) =
                    ability_def.condition
                {
                    if !engine::game::filter::matches_target_filter(
                        ctx.state,
                        *source_id,
                        filter,
                        &engine::game::filter::FilterContext::from_source(ctx.state, *source_id),
                    ) {
                        return GateDecision::Reject;
                    }
                }
            }
        }
    }

    let effects = ctx.effects();
    if effects.is_empty() {
        return GateDecision::Allow;
    }

    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::Counter { .. }))
        && (ctx.state.stack.is_empty()
            || ctx
                .state
                .stack
                .iter()
                .all(|entry| entry.controller == ctx.ai_player))
    {
        return GateDecision::Reject;
    }

    if is_redundant_creature_only_removal(ctx, &effects) {
        return GateDecision::Reject;
    }

    if let Some((power_bonus, toughness_bonus)) = pure_fixed_pump_bonus(&effects) {
        let source_is_spell = ctx.source_object().is_some_and(|source| {
            source.card_types.core_types.contains(&CoreType::Instant)
                || source.card_types.core_types.contains(&CoreType::Sorcery)
        });
        if source_is_spell {
            let facts = TacticalFacts::derive(ctx.state, ctx.ai_player);
            if should_reject_pump_window(ctx, &facts, power_bonus, toughness_bonus) {
                return GateDecision::Reject;
            }
            if facts.pass_preserves_stronger_window && !facts.live_stack_response {
                return GateDecision::AllowWithPenalty(-1.0);
            }
        }
    }

    GateDecision::Allow
}

fn target_choice_penalty(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let TargetRef::Object(object_id) = target else {
        return 0.0;
    };
    let harmful = ctx
        .effects()
        .iter()
        .any(|effect| matches!(effect_polarity(effect), EffectPolarity::Harmful));
    if harmful
        && has_pending_removal(ctx.state, *object_id)
        && will_target_die_from_stack(ctx.state, *object_id)
    {
        -10.0
    } else {
        0.0
    }
}

fn is_redundant_creature_only_removal(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> bool {
    let has_creature_only_harm = effects.iter().any(|effect| {
        matches!(effect_polarity(effect), EffectPolarity::Harmful) && targets_creatures_only(effect)
    });
    if !has_creature_only_harm {
        return false;
    }

    !ctx.state.battlefield.iter().any(|&object_id| {
        let Some(object) = ctx.state.objects.get(&object_id) else {
            return false;
        };
        object.controller != ctx.ai_player
            && object.card_types.core_types.contains(&CoreType::Creature)
            && !will_target_die_from_stack(ctx.state, object_id)
            // CR 702.11b + CR 702.18a: Hexproof/Shroud prevent targeting.
            && !object.has_keyword(&Keyword::Hexproof)
            && !object.has_keyword(&Keyword::Shroud)
    })
}

fn pure_fixed_pump_bonus(effects: &[&Effect]) -> Option<(i32, i32)> {
    if effects.is_empty()
        || !effects
            .iter()
            .all(|effect| matches!(effect, Effect::Pump { .. }))
    {
        return None;
    }

    let mut power_bonus = 0;
    let mut toughness_bonus = 0;
    for effect in effects {
        let Effect::Pump {
            power, toughness, ..
        } = effect
        else {
            return None;
        };
        let PtValue::Fixed(power) = power else {
            return None;
        };
        let PtValue::Fixed(toughness) = toughness else {
            return None;
        };
        power_bonus += *power;
        toughness_bonus += *toughness;
    }
    Some((power_bonus, toughness_bonus))
}

fn should_reject_pump_window(
    ctx: &PolicyContext<'_>,
    facts: &TacticalFacts,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    if facts.live_stack_response
        && pump_can_save_from_hostile_stack(ctx.state, ctx.ai_player, toughness_bonus)
    {
        return false;
    }

    match facts.window {
        TacticalWindow::OwnPostCombatMain
        | TacticalWindow::OpponentMain
        | TacticalWindow::EndStep => {
            return true;
        }
        TacticalWindow::OwnPreCombatMain | TacticalWindow::CombatBeforeBlocks => {
            return facts.pass_preserves_stronger_window;
        }
        TacticalWindow::Other => return true,
        TacticalWindow::CombatAfterBlocks
        | TacticalWindow::CombatDamage
        | TacticalWindow::StackResponse => {}
    }

    !pump_changes_combat_outcome(ctx.state, ctx.ai_player, power_bonus, toughness_bonus)
}

/// Check if pumping can actually save a creature from hostile stack effects.
/// Destroy/Exile/Counter/Bounce kill regardless of stats — pump doesn't help.
/// Only damage-based removal can be survived with a toughness boost.
fn pump_can_save_from_hostile_stack(
    state: &GameState,
    ai_player: PlayerId,
    toughness_bonus: i32,
) -> bool {
    use engine::types::ability::QuantityExpr;

    state.stack.iter().any(|entry| {
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|target| {
            let TargetRef::Object(object_id) = target else {
                return false;
            };
            let Some(object) = state.objects.get(object_id) else {
                return false;
            };
            if object.controller != ai_player
                || !object.card_types.core_types.contains(&CoreType::Creature)
            {
                return false;
            }

            let effects = collect_ability_effects(ability);
            for effect in &effects {
                match effect {
                    // Destroy/Exile/Counter/Bounce — pump doesn't save
                    Effect::Destroy { .. } | Effect::Counter { .. } | Effect::Bounce { .. } => {
                        return false
                    }
                    Effect::ChangeZone { .. } => return false,
                    // Damage — pump saves if toughness + bonus > damage
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value },
                        ..
                    } => {
                        let toughness = object.toughness.unwrap_or(0);
                        let remaining = toughness - object.damage_marked as i32;
                        if remaining + toughness_bonus > *value {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
            false
        })
    })
}

fn pump_changes_combat_outcome(
    state: &GameState,
    ai_player: PlayerId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(combat) = &state.combat else {
        return false;
    };

    let mut total_unblocked_damage = 0;
    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        if attacker_obj.controller != ai_player {
            continue;
        }
        let blocked = attacker.blocked
            || combat
                .blocker_assignments
                .get(&attacker.object_id)
                .is_some_and(|blockers| !blockers.is_empty());
        if !blocked {
            total_unblocked_damage += attacker_obj.power.unwrap_or(0);
        }
    }

    for attacker in &combat.attackers {
        let Some(attacker_obj) = state.objects.get(&attacker.object_id) else {
            continue;
        };
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .cloned()
            .unwrap_or_default();

        if attacker_obj.controller == ai_player {
            if blockers.is_empty() {
                if unblocked_attack_becomes_lethal(
                    state,
                    attacker,
                    total_unblocked_damage,
                    power_bonus,
                ) {
                    return true;
                }
                continue;
            }

            if blockers.len() == 1
                && combat_trade_improves(
                    state,
                    attacker.object_id,
                    blockers[0],
                    power_bonus,
                    toughness_bonus,
                )
            {
                return true;
            }
        } else {
            for blocker_id in
                combat
                    .blocker_to_attacker
                    .iter()
                    .filter_map(|(blocker_id, attacker_ids)| {
                        attacker_ids
                            .contains(&attacker.object_id)
                            .then_some(*blocker_id)
                    })
            {
                let Some(blocker_obj) = state.objects.get(&blocker_id) else {
                    continue;
                };
                if blocker_obj.controller == ai_player
                    && combat_trade_improves(
                        state,
                        blocker_id,
                        attacker.object_id,
                        power_bonus,
                        toughness_bonus,
                    )
                {
                    return true;
                }
            }
        }
    }

    false
}

fn combat_trade_improves(
    state: &GameState,
    my_creature_id: ObjectId,
    opposing_creature_id: ObjectId,
    power_bonus: i32,
    toughness_bonus: i32,
) -> bool {
    let Some(my_creature) = state.objects.get(&my_creature_id) else {
        return false;
    };
    let Some(opposing_creature) = state.objects.get(&opposing_creature_id) else {
        return false;
    };

    let my_power = my_creature.power.unwrap_or(0);
    let my_toughness = my_creature.toughness.unwrap_or(0) - my_creature.damage_marked as i32;
    let opposing_power = opposing_creature.power.unwrap_or(0);
    let opposing_toughness =
        opposing_creature.toughness.unwrap_or(0) - opposing_creature.damage_marked as i32;

    let dies_without_pump = my_toughness <= opposing_power;
    let survives_with_pump = my_toughness + toughness_bonus > opposing_power;
    if dies_without_pump && survives_with_pump {
        return true;
    }

    let fails_to_kill_without_pump = my_power < opposing_toughness;
    let kills_with_pump = my_power + power_bonus >= opposing_toughness;
    fails_to_kill_without_pump && kills_with_pump
}

fn unblocked_attack_becomes_lethal(
    state: &GameState,
    attacker: &engine::game::combat::AttackerInfo,
    total_unblocked_damage: i32,
    power_bonus: i32,
) -> bool {
    let AttackTarget::Player(defending_player) = attacker.attack_target else {
        return false;
    };
    let life = state.players[defending_player.0 as usize].life;
    total_unblocked_damage < life && total_unblocked_damage + power_bonus >= life
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{create_config, AiDifficulty, Platform};
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::combat::{AttackerInfo, CombatState};
    use engine::game::scenario::{GameScenario, P0, P1};
    use engine::types::ability::{ResolvedAbility, TargetFilter};
    use engine::types::game_state::{
        PendingCast, StackEntry, StackEntryKind, TargetSelectionProgress, TargetSelectionSlot,
        WaitingFor,
    };
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;

    #[test]
    fn rejects_pump_after_combat_without_live_threat() {
        let mut scenario = GameScenario::new();
        scenario.add_creature(P0, "Bear", 2, 2);
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::PostCombatMain;
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(P0),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn allows_pump_that_wins_combat() {
        let mut scenario = GameScenario::new();
        let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
        let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
        let growth = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Giant Growth",
                true,
                "Target creature gets +3/+3 until end of turn.",
            )
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            blocker_assignments: [(attacker, vec![blocker])].into_iter().collect(),
            blocker_to_attacker: [(blocker, vec![attacker])].into_iter().collect(),
            ..Default::default()
        });

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: growth,
                card_id: state.objects.get(&growth).unwrap().card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(P0),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_ne!(assess_candidate(&ctx), GateDecision::Reject);
    }

    #[test]
    fn penalizes_targeting_already_dead_creature() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Target", 2, 2).id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.stack.push_back(StackEntry {
            id: ObjectId(200),
            source_id: ObjectId(201),
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(ResolvedAbility::new(
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                    vec![TargetRef::Object(creature)],
                    ObjectId(201),
                    P0,
                )),
                card_id: CardId(201),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: P0,
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(202),
                    CardId(202),
                    ResolvedAbility::new(
                        Effect::Destroy {
                            target: TargetFilter::Any,
                            cant_regenerate: false,
                        },
                        Vec::new(),
                        ObjectId(202),
                        P0,
                    ),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                }],
                selection: TargetSelectionProgress::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata {
                actor: Some(P0),
                tactical_class: TacticalClass::Target,
            },
        };
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_eq!(
            assess_candidate(&ctx),
            GateDecision::AllowWithPenalty(-10.0)
        );
    }

    /// CR 601.2: A spell is cast the moment it's announced — the rules provide
    /// no strategic rewind. The AI's strategic pool must reject CancelCast so
    /// that pre-cast commitment stays coherent with targeting and payment.
    /// (The fallback_action escape in `search.rs` still supplies CancelCast
    /// when the scored pool is empty, covering genuine "can't complete cast"
    /// cases like unaffordable post-cost-increase mana or all targets gone.)
    #[test]
    fn rejects_cancel_cast_as_strategic_candidate() {
        let mut scenario = GameScenario::new();
        let creature = scenario.add_creature(P1, "Elvish Mystic", 1, 1).id();
        let unsummon = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .id();
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.waiting_for = WaitingFor::TargetSelection {
            player: P0,
            pending_cast: Box::new(PendingCast::new(
                unsummon,
                state.objects.get(&unsummon).unwrap().card_id,
                ResolvedAbility::new(
                    Effect::Bounce {
                        target: TargetFilter::Any,
                        destination: None,
                    },
                    Vec::new(),
                    unsummon,
                    P0,
                ),
                ManaCost::zero(),
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(creature)],
                optional: false,
            }],
            selection: TargetSelectionProgress::default(),
        };

        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CancelCast,
            metadata: ActionMetadata {
                actor: Some(P0),
                tactical_class: TacticalClass::Pass,
            },
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: P0,
            config: &config,
            context: &AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_eq!(assess_candidate(&ctx), GateDecision::Reject);
    }
}
