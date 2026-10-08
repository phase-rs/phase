//! Reducer-backed combat witness for a bounded, two-player swarm attack.
//!
//! This facade deliberately certifies only the narrow path it can replay through
//! the normal engine reducer without making a choice for either player. Every
//! omitted combat branch returns [`SwarmWitnessResult::Indeterminate`].

use std::ops::ControlFlow;

use crate::game::combat::{self, AttackTarget};
use crate::game::engine::apply_as_current_for_simulation;
use crate::game::functioning_abilities::active_static_definitions;
use crate::game::replacement::{find_applicable_replacements, replacement_registry};
use crate::types::ability::TargetRef;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::StaticMode;

/// Bound the declaration cartesian product before cloning reducer states.
pub const SWARM_WITNESS_MAX_DECLARATIONS: usize = 4_096;
const SWARM_WITNESS_MAX_REDUCER_STEPS: usize = 12;

/// Why a swarm witness declined to make a combat claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwarmWitnessIndeterminate {
    UnsupportedTopology,
    InvalidAttack,
    NonPlayerTarget,
    DeclarationCap,
    MultiBlockChoice,
    DamageChoice,
    CostOrPrompt,
    TriggerOrReplacement,
}

/// Exact, reducer-observed worst legal block declaration for one player attack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmCombatWitness {
    pub attacking_player: PlayerId,
    pub defending_player: PlayerId,
    /// Ordered attacker/player-target declaration reducer-replayed by this witness.
    /// The incarnation ref prevents a recycled object ID from inheriting a claim.
    pub declaration: Vec<(ObjectIncarnationRef, PlayerId)>,
    pub attackers: Vec<ObjectIncarnationRef>,
    pub blocker_candidates: Vec<ObjectIncarnationRef>,
    pub worst_declaration: Vec<(ObjectIncarnationRef, ObjectIncarnationRef)>,
    pub defending_life_before: i32,
    pub resulting_life_loss: u32,
    pub is_lethal: bool,
}

impl SwarmCombatWitness {
    /// Returns whether `attacks` is exactly the declaration this witness replayed.
    ///
    /// The bounded witness only certifies attacks at one player, so retaining the
    /// ordered `(attacker incarnation, player target)` pairs is sufficient to
    /// reject later unions, removals, reordering, or planeswalker redirection.
    pub fn binds_declaration(
        &self,
        state: &GameState,
        attacks: &[(crate::types::identifiers::ObjectId, AttackTarget)],
    ) -> bool {
        self.declaration.len() == attacks.len()
            && self.declaration.iter().zip(attacks).all(
                |((attacker, defending_player), (attacker_id, target))| {
                    state.objects.get(attacker_id).is_some_and(|object| {
                        ObjectIncarnationRef::from_object(object) == *attacker
                    }) && *defending_player == self.defending_player
                        && *target == AttackTarget::Player(*defending_player)
                },
            )
    }
}

/// A complete witness or an explicit conservative abstention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwarmWitnessResult {
    Certified(SwarmCombatWitness),
    Indeterminate(SwarmWitnessIndeterminate),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SwarmWitnessCounters {
    pub root_clone_applies: usize,
    pub raw_leaves: usize,
    pub legal_leaves: usize,
    pub candidate_clone_applies: usize,
}

/// CR 508.1 / CR 509.1 / CR 510.1: replay one exact attack through every bounded
/// legal blocker declaration and retain the declaration that minimizes the
/// defending player's actual life loss.
///
/// The witness is intentionally narrower than combat itself: two individual
/// players, player-only attack targets, no trample, no extra-block capacity,
/// no triggered/replacement continuation, and no damage-assignment choice.
/// Those exclusions are soundness boundaries, not heuristic shortcuts.
pub fn adversarial_swarm_witness(
    state: &GameState,
    attacking_player: PlayerId,
    attacks: &[(crate::types::identifiers::ObjectId, AttackTarget)],
) -> SwarmWitnessResult {
    swarm_witness_inner(state, attacking_player, attacks, None)
}

#[cfg(feature = "test-support")]
pub fn adversarial_swarm_witness_with_counters(
    state: &GameState,
    attacking_player: PlayerId,
    attacks: &[(crate::types::identifiers::ObjectId, AttackTarget)],
    counters: &mut SwarmWitnessCounters,
) -> SwarmWitnessResult {
    swarm_witness_inner(state, attacking_player, attacks, Some(counters))
}

fn swarm_witness_inner(
    state: &GameState,
    attacking_player: PlayerId,
    attacks: &[(crate::types::identifiers::ObjectId, AttackTarget)],
    #[allow(unused_mut, unused_variables)] mut counters: Option<&mut SwarmWitnessCounters>,
) -> SwarmWitnessResult {
    if state.players.len() != 2 || state.format_config.team_based {
        return indeterminate(SwarmWitnessIndeterminate::UnsupportedTopology);
    }
    let WaitingFor::DeclareAttackers { player, .. } = &state.waiting_for else {
        return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
    };
    if *player != attacking_player || state.active_player != attacking_player || attacks.is_empty()
    {
        return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
    }

    let Some(defending_player) = attacks.first().and_then(|(_, target)| match target {
        AttackTarget::Player(player) => Some(*player),
        AttackTarget::Planeswalker(_) | AttackTarget::Battle(_) => None,
    }) else {
        return indeterminate(SwarmWitnessIndeterminate::NonPlayerTarget);
    };
    if defending_player == attacking_player
        || attacks
            .iter()
            .any(|(_, target)| *target != AttackTarget::Player(defending_player))
    {
        return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
    }

    let mut declaration = Vec::with_capacity(attacks.len());
    let mut attackers = Vec::with_capacity(attacks.len());
    for (attacker_id, _) in attacks {
        let Some(attacker) = state.objects.get(attacker_id) else {
            return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
        };
        if attacker.has_keyword(&Keyword::Trample)
            || attacker.has_keyword(&Keyword::TrampleOverPlaneswalkers)
        {
            return indeterminate(SwarmWitnessIndeterminate::DamageChoice);
        }
        let attacker_ref = ObjectIncarnationRef::from_object(attacker);
        declaration.push((attacker_ref, defending_player));
        attackers.push(attacker_ref);
    }
    attackers.sort_unstable();
    attackers.dedup();
    if attackers.len() != attacks.len() {
        return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
    }

    #[cfg(feature = "test-support")]
    if let Some(counters) = &mut counters {
        counters.root_clone_applies += 1;
    }
    let mut after_attack = state.clone();
    let attack_events = match apply_as_current_for_simulation(
        &mut after_attack,
        GameAction::DeclareAttackers {
            attacks: attacks.to_vec(),
            bands: vec![],
        },
    ) {
        Ok(result) => result.events,
        // This action was formed from the caller's exact declaration; an engine
        // rejection cannot authorize a swarm claim.
        Err(_) => return indeterminate(SwarmWitnessIndeterminate::InvalidAttack),
    };
    if has_unmodeled_combat_event(&attack_events) || !after_attack.stack.is_empty() {
        return indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement);
    }
    // CR 510.1 + CR 614.1a: combat damage is assigned after blockers, and an
    // applicable replacement is a reducer boundary. Decline early rather than
    // driving priority past a damage replacement the witness cannot choose.
    if has_applicable_combat_damage_replacement(&after_attack) {
        return indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement);
    }
    if let Err(reason) = advance_to_blocker_declaration(&mut after_attack, defending_player) {
        return indeterminate(reason);
    }
    // CR 509.1a: The reducer may have submitted the unique empty declaration
    // while advancing from the attackers step. That is a complete defender
    // choice, not an unsupported prompt.
    let blockers_auto_declared = after_attack
        .combat
        .as_ref()
        .is_some_and(|combat| combat.blockers_declared_by.contains(&defending_player))
        || (after_attack.phase == Phase::DeclareBlockers
            && matches!(after_attack.waiting_for, WaitingFor::Priority { .. })
            && combat::get_valid_block_targets_for_player(&after_attack, defending_player)
                .is_empty());

    let (valid_blockers, valid_targets) = match &after_attack.waiting_for {
        WaitingFor::DeclareBlockers {
            player,
            valid_blocker_ids,
            valid_block_targets,
            ..
        } if *player == defending_player => {
            (valid_blocker_ids.clone(), valid_block_targets.clone())
        }
        _ if blockers_auto_declared => (Vec::new(), Default::default()),
        _ => return indeterminate(SwarmWitnessIndeterminate::CostOrPrompt),
    };
    if valid_blockers.iter().any(|blocker_id| {
        after_attack.objects.get(blocker_id).is_some_and(|blocker| {
            active_static_definitions(&after_attack, blocker)
                .any(|static_def| matches!(static_def.mode, StaticMode::ExtraBlockers { .. }))
        })
    }) {
        return indeterminate(SwarmWitnessIndeterminate::MultiBlockChoice);
    }
    let blocker_candidates = valid_blockers
        .iter()
        .filter_map(|blocker_id| after_attack.objects.get(blocker_id))
        .map(ObjectIncarnationRef::from_object)
        .collect();

    if checked_declaration_product(&valid_blockers, &valid_targets).is_none() {
        return indeterminate(SwarmWitnessIndeterminate::DeclarationCap);
    }
    let defending_life_before = player_life(&after_attack, defending_player);

    let mut worst: Option<(u32, Vec<(ObjectIncarnationRef, ObjectIncarnationRef)>)> = None;
    let mut failure = None;
    let mut scratch = Vec::new();
    let _ = stream_declarations(
        &valid_blockers,
        &valid_targets,
        0,
        &mut scratch,
        &mut |declaration| {
            #[cfg(feature = "test-support")]
            if let Some(counters) = &mut counters {
                counters.raw_leaves += 1;
            }
            if combat::validate_blockers_for_player(&after_attack, defending_player, declaration)
                .is_err()
            {
                return ControlFlow::Continue(());
            }
            #[cfg(feature = "test-support")]
            if let Some(counters) = &mut counters {
                counters.legal_leaves += 1;
                counters.candidate_clone_applies += 1;
            }
            let mut branch = after_attack.clone();
            if !blockers_auto_declared {
                let result = match apply_as_current_for_simulation(
                    &mut branch,
                    GameAction::DeclareBlockers {
                        assignments: declaration.to_vec(),
                    },
                ) {
                    Ok(result) => result,
                    Err(_) => {
                        failure = Some(SwarmWitnessIndeterminate::InvalidAttack);
                        return ControlFlow::Break(());
                    }
                };
                if has_unmodeled_combat_event(&result.events) || !branch.stack.is_empty() {
                    failure = Some(SwarmWitnessIndeterminate::TriggerOrReplacement);
                    return ControlFlow::Break(());
                }
            }
            if has_applicable_combat_damage_replacement(&branch) {
                failure = Some(SwarmWitnessIndeterminate::TriggerOrReplacement);
                return ControlFlow::Break(());
            }
            let life_before = player_life(&branch, defending_player);
            if let Err(reason) = advance_to_damage_completion(&mut branch, attacking_player) {
                failure = Some(reason);
                return ControlFlow::Break(());
            }
            let life_loss = life_before
                .saturating_sub(player_life(&branch, defending_player))
                .max(0) as u32;
            let mut bound_declaration = Vec::with_capacity(declaration.len());
            for (blocker, attacker) in declaration {
                let (Some(blocker), Some(attacker)) = (
                    after_attack.objects.get(blocker),
                    after_attack.objects.get(attacker),
                ) else {
                    failure = Some(SwarmWitnessIndeterminate::InvalidAttack);
                    return ControlFlow::Break(());
                };
                bound_declaration.push((
                    ObjectIncarnationRef::from_object(blocker),
                    ObjectIncarnationRef::from_object(attacker),
                ));
            }
            if worst
                .as_ref()
                .is_none_or(|(least_loss, _)| life_loss < *least_loss)
            {
                worst = Some((life_loss, bound_declaration));
            }
            ControlFlow::Continue(())
        },
    );
    if let Some(reason) = failure {
        return indeterminate(reason);
    }

    let Some((resulting_life_loss, worst_declaration)) = worst else {
        return indeterminate(SwarmWitnessIndeterminate::InvalidAttack);
    };
    SwarmWitnessResult::Certified(SwarmCombatWitness {
        attacking_player,
        defending_player,
        declaration,
        attackers,
        blocker_candidates,
        worst_declaration,
        defending_life_before,
        resulting_life_loss,
        is_lethal: defending_life_before > 0 && resulting_life_loss >= defending_life_before as u32,
    })
}

fn indeterminate(reason: SwarmWitnessIndeterminate) -> SwarmWitnessResult {
    SwarmWitnessResult::Indeterminate(reason)
}

fn checked_declaration_product(
    blockers: &[crate::types::identifiers::ObjectId],
    targets: &std::collections::HashMap<
        crate::types::identifiers::ObjectId,
        Vec<crate::types::identifiers::ObjectId>,
    >,
) -> Option<usize> {
    let mut count = 1usize;
    for blocker in blockers {
        count = count.checked_mul(targets.get(blocker).map_or(1, |choices| choices.len() + 1))?;
        if count > SWARM_WITNESS_MAX_DECLARATIONS {
            return None;
        }
    }

    Some(count)
}

fn stream_declarations(
    blockers: &[crate::types::identifiers::ObjectId],
    targets: &std::collections::HashMap<
        crate::types::identifiers::ObjectId,
        Vec<crate::types::identifiers::ObjectId>,
    >,
    index: usize,
    scratch: &mut Vec<(
        crate::types::identifiers::ObjectId,
        crate::types::identifiers::ObjectId,
    )>,
    visit: &mut impl FnMut(
        &[(
            crate::types::identifiers::ObjectId,
            crate::types::identifiers::ObjectId,
        )],
    ) -> ControlFlow<()>,
) -> ControlFlow<()> {
    if index == blockers.len() {
        return visit(scratch);
    }
    let blocker = blockers[index];
    if matches!(
        stream_declarations(blockers, targets, index + 1, scratch, visit),
        ControlFlow::Break(())
    ) {
        return ControlFlow::Break(());
    }
    for &attacker in targets.get(&blocker).into_iter().flatten() {
        scratch.push((blocker, attacker));
        let flow = stream_declarations(blockers, targets, index + 1, scratch, visit);
        scratch.pop();
        if matches!(flow, ControlFlow::Break(())) {
            return ControlFlow::Break(());
        }
    }
    ControlFlow::Continue(())
}

fn advance_to_blocker_declaration(
    state: &mut GameState,
    defender: PlayerId,
) -> Result<(), SwarmWitnessIndeterminate> {
    for _ in 0..SWARM_WITNESS_MAX_REDUCER_STEPS {
        if matches!(state.waiting_for, WaitingFor::DeclareBlockers { player, .. } if player == defender)
        {
            return Ok(());
        }
        if state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blockers_declared_by.contains(&defender))
        {
            return Ok(());
        }
        if state.phase == Phase::DeclareBlockers
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
            && combat::get_valid_block_targets_for_player(state, defender).is_empty()
        {
            return Ok(());
        }
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || !state.stack.is_empty() {
            return Err(SwarmWitnessIndeterminate::CostOrPrompt);
        }
        let Ok(result) = apply_as_current_for_simulation(state, GameAction::PassPriority) else {
            return Err(SwarmWitnessIndeterminate::CostOrPrompt);
        };
        if has_unmodeled_combat_event(&result.events) {
            return Err(SwarmWitnessIndeterminate::TriggerOrReplacement);
        }
    }
    Err(SwarmWitnessIndeterminate::CostOrPrompt)
}

fn advance_to_damage_completion(
    state: &mut GameState,
    attacking_player: PlayerId,
) -> Result<(), SwarmWitnessIndeterminate> {
    for _ in 0..SWARM_WITNESS_MAX_REDUCER_STEPS {
        // CR 104.1 / CR 104.2a: A combat terminal state completes this bounded
        // replay only when the attacking player is the reducer-declared winner.
        // A draw or any other winner never authorizes a life-loss certificate.
        if matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(winner) } if winner == attacking_player)
        {
            return Ok(());
        }
        if state
            .combat
            .as_ref()
            .is_some_and(|combat| combat.regular_damage_done)
        {
            return (state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. }))
            .then_some(())
            .ok_or(SwarmWitnessIndeterminate::DamageChoice);
        }
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || !state.stack.is_empty() {
            return Err(SwarmWitnessIndeterminate::DamageChoice);
        }
        let Ok(result) = apply_as_current_for_simulation(state, GameAction::PassPriority) else {
            return Err(SwarmWitnessIndeterminate::DamageChoice);
        };
        if has_unmodeled_combat_event(&result.events) {
            return Err(SwarmWitnessIndeterminate::TriggerOrReplacement);
        }
    }
    Err(SwarmWitnessIndeterminate::DamageChoice)
}

fn has_unmodeled_combat_event(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::DamagePrevented { .. } | GameEvent::ReplacementApplied { .. }
        )
    })
}

fn has_applicable_combat_damage_replacement(state: &GameState) -> bool {
    if !state.pending_damage_replacements.is_empty() {
        return true;
    }
    let Some(combat) = state.combat.as_ref() else {
        return true;
    };
    combat.attackers.iter().any(|attacker| {
        let Some(attacker_object) = state.objects.get(&attacker.object_id) else {
            return true;
        };
        let damage = attacker_object.power.unwrap_or(0).max(0) as u32;
        let blockers = combat
            .blocker_assignments
            .get(&attacker.object_id)
            .filter(|blockers| !blockers.is_empty());
        let attacker_target = match blockers {
            None if !attacker.blocked => TargetRef::Player(attacker.defending_player),
            Some(blockers) if blockers.len() == 1 => TargetRef::Object(blockers[0]),
            _ => return true,
        };
        replacement_applies(state, attacker.object_id, attacker_target, damage)
            || blockers.is_some_and(|blockers| {
                let blocker_id = blockers[0];
                state.objects.get(&blocker_id).is_none_or(|blocker| {
                    replacement_applies(
                        state,
                        blocker_id,
                        TargetRef::Object(attacker.object_id),
                        blocker.power.unwrap_or(0).max(0) as u32,
                    )
                })
            })
    })
}

fn replacement_applies(
    state: &GameState,
    source_id: crate::types::identifiers::ObjectId,
    target: TargetRef,
    amount: u32,
) -> bool {
    if amount == 0 {
        return false;
    }
    !find_applicable_replacements(
        state,
        &ProposedEvent::Damage {
            source_id,
            target,
            amount,
            is_combat: true,
            applied: Default::default(),
        },
        replacement_registry(),
    )
    .is_empty()
}

fn player_life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .map_or(0, |candidate| candidate.life)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use std::collections::HashMap;

    /// The refusal is `count > SWARM_WITNESS_MAX_DECLARATIONS`, so a product landing exactly on
    /// the cap is affordable and must be admitted. The integration fixture only exercises a
    /// product above the bound, which a `>=` here would still refuse — leaving which side of the
    /// comparison the boundary falls on unpinned.
    #[test]
    fn the_declaration_cap_is_admitted_and_only_the_count_past_it_is_refused() {
        let blocker = ObjectId(1);
        // Each blocker contributes `legal targets + 1`, the +1 being "does not block".
        let product_with = |choices: usize| {
            let targets = HashMap::from([(
                blocker,
                (0..choices)
                    .map(|i| ObjectId(i as u64 + 2))
                    .collect::<Vec<_>>(),
            )]);
            checked_declaration_product(&[blocker], &targets)
        };

        assert_eq!(
            product_with(SWARM_WITNESS_MAX_DECLARATIONS - 1),
            Some(SWARM_WITNESS_MAX_DECLARATIONS),
            "a declaration product equal to the cap is affordable and must be admitted"
        );
        assert_eq!(
            product_with(SWARM_WITNESS_MAX_DECLARATIONS),
            None,
            "one declaration past the cap must be refused"
        );
    }
}
