use crate::game::combat::{AttackTarget, CombatState, DamageAssignment, DamageTarget, TrampleKind};
use crate::game::effects::deal_damage::{apply_damage_to_target, DamageContext, DamageResult};
use crate::game::game_object::GameObject;
use crate::game::sba;
use crate::game::triggers;
use crate::types::ability::TargetRef;
use crate::types::events::GameEvent;
use crate::types::game_state::{CombatDamageAssignmentMode, DamageSlot, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;

/// CR 510.1a + CR 613: Returns the amount of combat damage a creature assigns.
/// Normally equal to power, but if `assigns_damage_from_toughness` is set (e.g. Doran),
/// uses toughness instead. If `assigns_no_combat_damage` is set, returns 0.
fn combat_damage_amount(obj: &GameObject) -> u32 {
    // CR 510.1a: "~ assigns no combat damage" — creature deals 0 combat damage.
    if obj.assigns_no_combat_damage {
        return 0;
    }
    if obj.assigns_damage_from_toughness {
        // CR 613 + CR 510.1: Continuous effect assigns combat damage equal to toughness rather than power.
        obj.toughness.unwrap_or(0).max(0) as u32
    } else {
        // CR 510.1a: Assign combat damage equal to power.
        obj.power.unwrap_or(0).max(0) as u32
    }
}

/// CR 603.2 + CR 704.3: Full trigger/SBA loop after combat damage.
///
/// 1. Collect triggers from damage events while source creatures are still on the battlefield
///    (e.g., DamageReceived for Jackal Pup).
/// 2. Run SBAs (destroy lethal-damage creatures → ZoneChanged events).
/// 3. Process triggers from SBA-generated events (e.g., dies triggers from graveyard scan).
/// 4. Repeat SBA/trigger cycle until stable (no new SBAs, no new triggers).
fn process_combat_damage_triggers(
    state: &mut GameState,
    damage_events: &[GameEvent],
    all_events: &mut Vec<GameEvent>,
) {
    // Step 1: Collect triggers from damage events while creatures are still alive.
    // CR 603.2: Triggers fire at the moment the event occurs — process_triggers
    // scans state.battlefield, so this must run before SBAs remove dying objects.
    triggers::process_triggers(state, damage_events);

    // Steps 2-4: SBA/trigger loop per CR 704.3.
    // SBAs may generate events (ZoneChanged for dying creatures) that need trigger
    // processing (dies triggers). Repeat until no new SBAs and no new triggers.
    loop {
        let events_before = all_events.len();
        sba::check_state_based_actions(state, all_events);

        // If SBAs generated new events, process triggers for those events.
        if all_events.len() > events_before {
            let new_events: Vec<_> = all_events[events_before..].to_vec();
            triggers::process_triggers(state, &new_events);
        } else {
            break;
        }
    }
}

/// Resolve combat damage with first strike / double strike support (CR 510.1).
/// CR 702.7b: If any creature has first strike or double strike, two damage sub-steps run.
/// Between sub-steps: SBAs are checked and triggers processed.
///
/// Returns `Some(WaitingFor)` when an attacker with 2+ blockers needs interactive
/// damage assignment. Returns `None` when all damage for the current phase is resolved.
/// Re-entrant: call again after the player submits `GameAction::AssignCombatDamage`.
pub fn resolve_combat_damage(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let combat = state.combat.as_ref()?.clone();

    // Guard: regular damage already applied (re-entry from triggers during regular step).
    if combat.regular_damage_done {
        return None;
    }

    let has_first_or_double = combat.attackers.iter().any(|a| {
        state
            .objects
            .get(&a.object_id)
            .map(|o| o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike))
            .unwrap_or(false)
    }) || combat.blocker_to_attacker.keys().any(|blocker_id| {
        state
            .objects
            .get(blocker_id)
            .map(|o| o.has_keyword(&Keyword::FirstStrike) || o.has_keyword(&Keyword::DoubleStrike))
            .unwrap_or(false)
    });

    // --- First strike sub-step ---
    if has_first_or_double && !combat.first_strike_done {
        if let Some(waiting) = collect_damage_assignments(state, SubStep::FirstStrike) {
            return Some(waiting);
        }
        // All first-strike assignments collected — apply simultaneously (CR 510.2).
        let pending = take_pending_damage(state);
        let damage_events = apply_combat_damage(state, &pending);
        events.extend(damage_events.iter().cloned());

        if let Some(c) = &mut state.combat {
            c.first_strike_done = true;
            c.damage_step_index = None;
        }

        // CR 510.4: SBAs and triggers run between first-strike and regular damage sub-steps.
        process_combat_damage_triggers(state, &damage_events, events);
    }

    // --- Regular damage sub-step ---
    if let Some(waiting) = collect_damage_assignments(state, SubStep::Regular) {
        return Some(waiting);
    }
    // All regular assignments collected — apply simultaneously (CR 510.2).
    let pending = take_pending_damage(state);
    let damage_events = apply_combat_damage(state, &pending);
    events.extend(damage_events.iter().cloned());

    if let Some(c) = &mut state.combat {
        c.regular_damage_done = true;
        c.damage_step_index = None;
    }

    process_combat_damage_triggers(state, &damage_events, events);
    None
}

/// Which sub-step of combat damage we're collecting assignments for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SubStep {
    FirstStrike,
    Regular,
}

/// Drain pending_damage from CombatState, resetting it to empty.
fn take_pending_damage(state: &mut GameState) -> Vec<(ObjectId, DamageAssignment)> {
    state
        .combat
        .as_mut()
        .map(|c| std::mem::take(&mut c.pending_damage))
        .unwrap_or_default()
}

/// Iterate attackers (and blockers) for a sub-step, collecting auto-assigned damage
/// into `combat.pending_damage`. Returns `Some(WaitingFor::AssignCombatDamage)` when
/// an attacker has 2+ blockers and needs interactive assignment.
fn collect_damage_assignments(state: &mut GameState, sub_step: SubStep) -> Option<WaitingFor> {
    let combat = state.combat.as_ref()?.clone();
    let start_index = combat.damage_step_index.unwrap_or(0);
    let first_strike_was_done = combat.first_strike_done;

    // --- Attackers ---
    for (i, attacker_info) in combat.attackers.iter().enumerate().skip(start_index) {
        let obj = match state.objects.get(&attacker_info.object_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        // Sub-step filter
        match sub_step {
            SubStep::FirstStrike => {
                if !obj.has_keyword(&Keyword::FirstStrike)
                    && !obj.has_keyword(&Keyword::DoubleStrike)
                {
                    continue;
                }
            }
            SubStep::Regular => {
                // Skip FirstStrike-only creatures that already dealt in first-strike step
                if first_strike_was_done
                    && obj.has_keyword(&Keyword::FirstStrike)
                    && !obj.has_keyword(&Keyword::DoubleStrike)
                {
                    continue;
                }
            }
        }

        let power = combat_damage_amount(obj);
        if power == 0 {
            continue;
        }

        let has_deathtouch = obj.has_keyword(&Keyword::Deathtouch);
        // CR 702.19c takes precedence when both present — it subsumes regular trample behavior
        let trample = if obj.has_keyword(&Keyword::TrampleOverPlaneswalkers) {
            Some(TrampleKind::OverPlaneswalkers)
        } else if obj.has_keyword(&Keyword::Trample) {
            Some(TrampleKind::Standard)
        } else {
            None
        };

        // CR 510.1c: Check if interactive assignment is needed (2+ blockers).
        if needs_interactive_assignment(obj, &combat, attacker_info) {
            // Pause iteration — player must choose damage division.
            if let Some(c) = &mut state.combat {
                c.damage_step_index = Some(i);
            }

            let blocker_ids = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .cloned()
                .unwrap_or_default();
            let blockers: Vec<DamageSlot> = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .into_iter()
                .flatten()
                .map(|&bid| DamageSlot {
                    blocker_id: bid,
                    lethal_minimum: lethal_damage_needed(state, bid, has_deathtouch),
                })
                .collect();
            let assignment_modes = combat_damage_assignment_modes(
                obj,
                attacker_info.blocked,
                !blocker_ids.is_empty(),
                trample,
            );

            // The player who assigns damage is the attacker's controller.
            let controller = state
                .objects
                .get(&attacker_info.object_id)
                .map(|o| o.controller)
                .unwrap_or(state.active_player);

            // CR 702.19c: Compute PW loyalty threshold for trample-over-PW spillover
            let (pw_loyalty, pw_controller) = if trample == Some(TrampleKind::OverPlaneswalkers) {
                compute_pw_loyalty_threshold(state, &attacker_info.attack_target)
            } else {
                (None, None)
            };

            return Some(WaitingFor::AssignCombatDamage {
                player: controller,
                attacker_id: attacker_info.object_id,
                total_damage: power,
                blockers,
                assignment_modes,
                trample,
                defending_player: attacker_info.defending_player,
                attack_target: attacker_info.attack_target,
                pw_loyalty,
                pw_controller,
            });
        }

        // Auto-assign for unblocked, single blocker, or blocked-but-no-current-blockers.
        let assignments = assign_attacker_damage(
            state,
            attacker_info,
            &combat,
            power,
            has_deathtouch,
            trample,
        );
        if let Some(c) = &mut state.combat {
            for a in assignments {
                c.pending_damage.push((attacker_info.object_id, a));
            }
        }
    }

    // --- Blockers ---
    // CR 510.1d: Blocker damage division among multiple blocked attackers.
    // Currently auto-assigned with even split (known simplification — multi-block is rare).
    for (blocker_id, attacker_ids) in &combat.blocker_to_attacker {
        let obj = match state.objects.get(blocker_id) {
            Some(o) if o.zone == crate::types::zones::Zone::Battlefield => o,
            _ => continue,
        };

        match sub_step {
            SubStep::FirstStrike => {
                if !obj.has_keyword(&Keyword::FirstStrike)
                    && !obj.has_keyword(&Keyword::DoubleStrike)
                {
                    continue;
                }
            }
            SubStep::Regular => {
                if first_strike_was_done
                    && obj.has_keyword(&Keyword::FirstStrike)
                    && !obj.has_keyword(&Keyword::DoubleStrike)
                {
                    continue;
                }
            }
        }

        let power = combat_damage_amount(obj);
        if power == 0 {
            continue;
        }
        let blocker_assignments = distribute_blocker_damage(*blocker_id, power, attacker_ids);
        if let Some(c) = &mut state.combat {
            c.pending_damage.extend(blocker_assignments);
        }
    }

    // All done for this sub-step — reset index.
    if let Some(c) = &mut state.combat {
        c.damage_step_index = None;
    }
    None
}

/// CR 510.1d: Distribute a blocker's combat damage among the attackers it blocks.
/// When blocking multiple attackers, damage is split evenly (first attacker gets remainder).
fn distribute_blocker_damage(
    blocker_id: ObjectId,
    power: u32,
    attacker_ids: &[ObjectId],
) -> Vec<(ObjectId, DamageAssignment)> {
    if attacker_ids.is_empty() {
        return Vec::new();
    }
    if attacker_ids.len() == 1 {
        return vec![(
            blocker_id,
            DamageAssignment {
                target: DamageTarget::Object(attacker_ids[0]),
                amount: power,
            },
        )];
    }
    // Split damage evenly; first attacker gets the remainder
    let n = attacker_ids.len() as u32;
    let base = power / n;
    let remainder = power % n;
    attacker_ids
        .iter()
        .enumerate()
        .filter_map(|(i, &aid)| {
            let amount = base + if (i as u32) < remainder { 1 } else { 0 };
            if amount == 0 {
                None
            } else {
                Some((
                    blocker_id,
                    DamageAssignment {
                        target: DamageTarget::Object(aid),
                        amount,
                    },
                ))
            }
        })
        .collect()
}

/// CR 510.1c: Check if an attacker needs interactive damage assignment.
/// Returns true when there are 2+ blockers — the attacking player should choose
/// how to divide damage. Single-blocker and unblocked scenarios are auto-assigned.
pub(crate) fn needs_interactive_assignment(
    obj: &GameObject,
    combat: &CombatState,
    attacker_info: &crate::game::combat::AttackerInfo,
) -> bool {
    let blocker_count = combat
        .blocker_assignments
        .get(&attacker_info.object_id)
        .map_or(0, Vec::len);

    if obj.assigns_damage_as_though_unblocked && attacker_info.blocked {
        let has_trample = obj.has_keyword(&Keyword::Trample)
            || obj.has_keyword(&Keyword::TrampleOverPlaneswalkers);
        return blocker_count > 0 || !has_trample;
    }

    blocker_count >= 2
}

/// CR 702.19c: Compute effective PW loyalty threshold for trample-over-PW,
/// accounting for pending damage from other attackers in the same step.
fn compute_pw_loyalty_threshold(
    state: &GameState,
    attack_target: &AttackTarget,
) -> (Option<u32>, Option<PlayerId>) {
    if let AttackTarget::Planeswalker(pw_id) = attack_target {
        // CR 306.8: PW loyalty is tracked via the `loyalty` field (authoritative),
        // synced with counters on damage application. Read the field directly.
        let base_loyalty = state
            .objects
            .get(pw_id)
            .and_then(|obj| obj.loyalty)
            .unwrap_or(0);
        // CR 702.19c: Account for pending damage from other attackers this step
        let pending_to_pw: u32 = state
            .combat
            .as_ref()
            .map(|c| {
                c.pending_damage
                    .iter()
                    .filter(|(_, da)| da.target == DamageTarget::Object(*pw_id))
                    .map(|(_, da)| da.amount)
                    .sum()
            })
            .unwrap_or(0);
        let effective = base_loyalty.saturating_sub(pending_to_pw);
        let controller = state.objects.get(pw_id).map(|obj| obj.controller);
        (Some(effective), controller)
    } else {
        (None, None)
    }
}

/// Assign trample excess damage when attacking a PW with trample-over-PW.
/// CR 702.19c: lethal to blocker(s) → loyalty-worth to PW → excess to PW controller.
fn assign_trample_over_pw_excess(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    excess: u32,
) -> Vec<DamageAssignment> {
    let mut result = Vec::new();
    if excess == 0 {
        return result;
    }
    let (pw_loyalty, _) = compute_pw_loyalty_threshold(state, &attacker_info.attack_target);
    let effective_loyalty = pw_loyalty.unwrap_or(0);
    let to_pw = excess.min(effective_loyalty);
    let to_controller = excess.saturating_sub(to_pw);

    if to_pw > 0 {
        // CR 702.19e: trample_over_pw=true so PW removal falls back to defending player.
        if let Some(target) = attacker_info.resolve_damage_target(state, true) {
            result.push(DamageAssignment {
                target,
                amount: to_pw,
            });
        }
    }
    if to_controller > 0 {
        result.push(DamageAssignment {
            target: DamageTarget::Player(attacker_info.defending_player),
            amount: to_controller,
        });
    }
    result
}

fn combat_damage_assignment_modes(
    obj: &GameObject,
    blocked: bool,
    has_blockers: bool,
    trample: Option<TrampleKind>,
) -> Vec<CombatDamageAssignmentMode> {
    if obj.assigns_damage_as_though_unblocked && blocked && (has_blockers || trample.is_none()) {
        vec![
            CombatDamageAssignmentMode::Normal,
            CombatDamageAssignmentMode::AsThoughUnblocked,
        ]
    } else {
        vec![CombatDamageAssignmentMode::Normal]
    }
}

pub(crate) fn assign_damage_as_though_unblocked(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    power: u32,
    trample: Option<TrampleKind>,
) -> Vec<DamageAssignment> {
    let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
    match attacker_info.resolve_damage_target(state, is_over_pw) {
        Some(target) => vec![DamageAssignment {
            target,
            amount: power,
        }],
        None => Vec::new(),
    }
}

/// Auto-assign damage for unblocked or single-blocker attackers.
/// Multi-blocker cases (2+) are handled interactively via WaitingFor::AssignCombatDamage.
fn assign_attacker_damage(
    state: &GameState,
    attacker_info: &crate::game::combat::AttackerInfo,
    combat: &CombatState,
    power: u32,
    has_deathtouch: bool,
    trample: Option<TrampleKind>,
) -> Vec<DamageAssignment> {
    let attacker_id = attacker_info.object_id;

    let blockers = combat
        .blocker_assignments
        .get(&attacker_id)
        .filter(|b| !b.is_empty());

    match blockers {
        None => {
            if attacker_info.blocked {
                // CR 702.19d: Trample (both variants) — blocked but no blockers remaining,
                // assign all damage to attack target as though lethal was assigned.
                if trample.is_some() {
                    let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
                    if is_over_pw
                        && matches!(attacker_info.attack_target, AttackTarget::Planeswalker(..))
                    {
                        // CR 702.19d + CR 702.19c: Trample-over-PW with no blockers attacking PW
                        return assign_trample_over_pw_excess(state, attacker_info, power);
                    }
                    // CR 702.19d: Standard trample with no blockers — all to attack target
                    match attacker_info.resolve_damage_target(state, false) {
                        Some(target) => {
                            return vec![DamageAssignment {
                                target,
                                amount: power,
                            }]
                        }
                        None => return Vec::new(),
                    }
                }
                // CR 509.1h + CR 510.1c: Non-trample blocked creature with all
                // blockers removed — still "blocked" and assigns no damage.
                return Vec::new();
            }
            // CR 510.1b: Unblocked creature assigns damage to the player/planeswalker/battle it's attacking.
            // CR 506.4c / CR 702.19e: If PW left, trample-over-PW falls back to defending player.
            let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
            match attacker_info.resolve_damage_target(state, is_over_pw) {
                Some(target) => vec![DamageAssignment {
                    target,
                    amount: power,
                }],
                None => Vec::new(),
            }
        }
        Some(blockers) => {
            if blockers.len() == 1 {
                if let Some(trample_kind) = trample {
                    // CR 702.19b: Trample — assign lethal to blocker, excess to attack target.
                    let lethal = lethal_damage_needed(state, blockers[0], has_deathtouch);
                    let to_blocker = power.min(lethal);
                    let excess = power.saturating_sub(to_blocker);
                    let mut result = vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: to_blocker,
                    }];
                    if excess > 0 {
                        if trample_kind == TrampleKind::OverPlaneswalkers
                            && matches!(attacker_info.attack_target, AttackTarget::Planeswalker(..))
                        {
                            // CR 702.19c: Trample-over-PW attacking PW — split excess
                            // between PW (up to loyalty) and PW controller.
                            result.extend(assign_trample_over_pw_excess(
                                state,
                                attacker_info,
                                excess,
                            ));
                        } else {
                            // CR 702.19f: Standard trample or trample-over-PW attacking
                            // non-PW — excess goes to the attack target directly.
                            if let Some(target) = attacker_info.resolve_damage_target(state, false)
                            {
                                result.push(DamageAssignment {
                                    target,
                                    amount: excess,
                                });
                            }
                        }
                    }
                    result
                } else {
                    // Single blocker without trample: all damage to blocker
                    vec![DamageAssignment {
                        target: DamageTarget::Object(blockers[0]),
                        amount: power,
                    }]
                }
            } else {
                // 2+ blockers: handled interactively via WaitingFor::AssignCombatDamage.
                // This branch should never be reached — needs_interactive_assignment
                // returns true for 2+ blockers and collect_damage_assignments pauses.
                debug_assert!(false, "multi-blocker auto-assignment should not be reached");
                Vec::new()
            }
        }
    }
}

/// How much damage is needed to kill this creature.
/// CR 702.2c: Deathtouch — any amount of damage from a deathtouch source is lethal.
fn lethal_damage_needed(
    state: &GameState,
    object_id: ObjectId,
    source_has_deathtouch: bool,
) -> u32 {
    if source_has_deathtouch {
        // CR 702.2c + CR 702.19b: With deathtouch, 1 damage is lethal.
        return 1;
    }
    state
        .objects
        .get(&object_id)
        .map(|obj| {
            let toughness = obj.toughness.unwrap_or(0).max(0) as u32;
            toughness.saturating_sub(obj.damage_marked)
        })
        .unwrap_or(1)
}

/// Apply all combat damage assignments simultaneously (CR 510.2).
/// All damage goes through replace_event for replacement effect interception.
/// Apply pre-built combat damage assignments through the shared damage pipeline.
/// Handles protection, replacement effects, lifelink, deathtouch, infect, and events.
/// Used by both the automatic assignment path and the interactive `AssignCombatDamage` handler.
pub(crate) fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut combat_damage_to_players: Vec<(crate::types::player::PlayerId, Vec<ObjectId>)> =
        Vec::new();

    for (source_id, assignment) in assignments {
        // Read commander flag before DamageContext borrows — both are immutable reads.
        let source_is_commander = state
            .objects
            .get(source_id)
            .map(|o| o.is_commander)
            .unwrap_or(false);
        // In practice, from_source always succeeds during combat (source is on battlefield).
        // Fallback uses PlayerId(0) — matches existing behavior.
        let ctx = DamageContext::from_source(state, *source_id)
            .unwrap_or_else(|| DamageContext::fallback(*source_id, PlayerId(0)));

        let target_ref = match &assignment.target {
            DamageTarget::Object(id) => TargetRef::Object(*id),
            DamageTarget::Player(id) => TargetRef::Player(*id),
        };

        // Delegate to shared damage pipeline — handles protection, replacement,
        // damage application, deathtouch, lifelink, and DamageDealt event.
        let actual_amount = match apply_damage_to_target(
            state,
            &ctx,
            target_ref,
            assignment.amount,
            true,
            &mut events,
        ) {
            Ok(DamageResult::Applied(amt)) => amt,
            // Combat damage NeedsChoice: skip this assignment (existing behavior).
            // The helper does NOT set waiting_for when is_combat == true.
            Ok(DamageResult::NeedsChoice) => 0,
            Err(_) => 0,
        };

        // Combat-only bookkeeping (not part of the shared damage pipeline):
        if let DamageTarget::Player(player_id) = &assignment.target {
            // Track CombatDamageDealtToPlayer source batching
            let player_sources = combat_damage_to_players
                .iter_mut()
                .find(|(damaged_player, _)| *damaged_player == *player_id)
                .map(|(_, source_ids)| source_ids);
            if let Some(source_ids) = player_sources {
                if !source_ids.contains(source_id) {
                    source_ids.push(*source_id);
                }
            } else {
                combat_damage_to_players.push((*player_id, vec![*source_id]));
            }

            // CR 704.6c: Track commander combat damage for the 21-damage loss condition.
            if source_is_commander && actual_amount > 0 {
                if let Some(entry) = state
                    .commander_damage
                    .iter_mut()
                    .find(|e| e.player == *player_id && e.commander == *source_id)
                {
                    entry.damage += actual_amount;
                } else {
                    state
                        .commander_damage
                        .push(crate::types::game_state::CommanderDamageEntry {
                            player: *player_id,
                            commander: *source_id,
                            damage: actual_amount,
                        });
                }
            }
        }
    }

    for (player_id, source_ids) in combat_damage_to_players {
        events.push(GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_ids,
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, Comparator, ContinuousModification, ControllerRef, Effect, QuantityExpr,
        QuantityRef, StaticCondition, StaticDefinition, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state
    }

    fn create_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
    }

    fn setup_combat(
        state: &mut GameState,
        attackers: Vec<ObjectId>,
        blocker_assignments: Vec<(ObjectId, Vec<ObjectId>)>,
    ) {
        let mut combat = CombatState {
            attackers: attackers
                .iter()
                .map(|&id| AttackerInfo::attacking_player(id, PlayerId(1)))
                .collect(),
            ..Default::default()
        };
        for (attacker_id, blockers) in blocker_assignments {
            // CR 509.1h: Mark the attacker as blocked.
            if let Some(info) = combat
                .attackers
                .iter_mut()
                .find(|a| a.object_id == attacker_id)
            {
                if !blockers.is_empty() {
                    info.blocked = true;
                }
            }
            for &blocker_id in &blockers {
                combat
                    .blocker_to_attacker
                    .entry(blocker_id)
                    .or_default()
                    .push(attacker_id);
            }
            combat.blocker_assignments.insert(attacker_id, blockers);
        }
        state.combat = Some(combat);
    }

    #[test]
    fn unblocked_attacker_deals_damage_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18); // 20 - 2
    }

    #[test]
    fn blocked_attacker_deals_damage_to_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Attacker dealt 2 to blocker
        assert_eq!(state.objects[&blocker].damage_marked, 2);
        // Blocker dealt 0 to attacker
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // No player damage
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn blocked_attacker_with_unblocked_option_waits_for_assignment_choice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .assigns_damage_as_though_unblocked = true;
        let blocker_a = create_creature(&mut state, PlayerId(1), "Wall A", 2, 2);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Wall B", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker_a, blocker_b])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                assignment_modes,
                ..
            }) => {
                assert_eq!(total_damage, 5);
                assert_eq!(blockers.len(), 2);
                assert_eq!(
                    assignment_modes,
                    vec![
                        CombatDamageAssignmentMode::Normal,
                        CombatDamageAssignmentMode::AsThoughUnblocked,
                    ]
                );
            }
            other => panic!("Expected AssignCombatDamage choice, got {other:?}"),
        }
    }

    #[test]
    fn single_blocker_with_unblocked_option_waits_for_assignment_choice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .assigns_damage_as_though_unblocked = true;
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);

        // CR 510.1c: Single blocker would normally auto-assign, but the
        // assigns-damage-as-though-unblocked flag forces interactive choice.
        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                assignment_modes,
                ..
            }) => {
                assert_eq!(total_damage, 5);
                assert_eq!(blockers.len(), 1);
                assert_eq!(
                    assignment_modes,
                    vec![
                        CombatDamageAssignmentMode::Normal,
                        CombatDamageAssignmentMode::AsThoughUnblocked,
                    ]
                );
            }
            other => panic!("Expected AssignCombatDamage choice, got {other:?}"),
        }
    }

    #[test]
    fn mutual_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.objects[&attacker].damage_marked, 2);
        assert_eq!(state.objects[&blocker].damage_marked, 2);
    }

    #[test]
    fn first_strike_kills_before_regular_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // First strike dealt 3 damage (lethal) to blocker
        // SBAs ran between steps -- blocker should have been destroyed
        // Blocker can't deal damage back in regular step (dead)
        // Attacker should have 0 damage
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        // Blocker should be in graveyard (SBAs ran between steps)
        assert!(!state.battlefield.contains(&blocker));
    }

    #[test]
    fn double_strike_deals_damage_twice() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Knight", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 + 3 = 6 damage to player
        assert_eq!(state.players[1].life, 14);
    }

    #[test]
    fn trample_assigns_lethal_then_excess_to_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 2 to blocker (lethal), 3 to player (trample excess)
        assert_eq!(state.objects[&blocker].damage_marked, 2);
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn trample_deathtouch_assigns_one_to_each_blocker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        // 2+ blockers → returns WaitingFor::AssignCombatDamage.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        assert!(matches!(
            waiting,
            Some(WaitingFor::AssignCombatDamage { .. })
        ));

        // Submit: 1 to each blocker (deathtouch lethal), 3 trample to player.
        if let Some(combat) = &mut state.combat {
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker1),
                    amount: 1,
                },
            ));
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker2),
                    amount: 1,
                },
            ));
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Player(PlayerId(1)),
                    amount: 3,
                },
            ));
            combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
        }
        let result = resolve_combat_damage(&mut state, &mut events);
        assert!(result.is_none(), "All damage should be resolved");

        // With deathtouch, 1 to each blocker is lethal; 3 excess tramples to player
        assert_eq!(state.objects[&blocker1].damage_marked, 1);
        assert_eq!(state.objects[&blocker2].damage_marked, 1);
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn lifelink_gains_life_on_combat_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 3 damage to defending player
        assert_eq!(state.players[1].life, 17);
        // 3 life gained by controller
        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn combat_no_combat_state_is_noop() {
        let mut state = setup();
        state.combat = None;
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn multiple_blockers_returns_waiting_for_assignment() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Fatty", 5, 5);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Bear1", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Bear2", 2, 2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        // CR 510.1c: 2+ blockers → interactive assignment required.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match &waiting {
            Some(WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                trample,
                ..
            }) => {
                assert_eq!(*total_damage, 5);
                assert_eq!(blockers.len(), 2);
                assert!(trample.is_none());
            }
            other => panic!("Expected AssignCombatDamage, got {:?}", other),
        }

        // Submit: free division — all 5 to blocker1, 0 to blocker2 (legal under current rules).
        if let Some(combat) = &mut state.combat {
            combat.pending_damage.push((
                attacker,
                DamageAssignment {
                    target: DamageTarget::Object(blocker1),
                    amount: 5,
                },
            ));
            combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
        }
        let result = resolve_combat_damage(&mut state, &mut events);
        assert!(result.is_none(), "All damage should be resolved");

        // All 5 to blocker1, none to blocker2
        assert_eq!(state.objects[&blocker1].damage_marked, 5);
        assert_eq!(state.objects[&blocker2].damage_marked, 0);
        // No damage to player
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn deathtouch_marks_flag_on_target() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT", 1, 1);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert!(state.objects[&blocker].dealt_deathtouch_damage);
    }

    #[test]
    fn wither_applies_minus_counters_to_creature_instead_of_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither: 3 -1/-1 counters instead of damage_marked
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn wither_to_player_deals_normal_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Wither", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Wither);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Wither does NOT give poison to players, just normal damage
        assert_eq!(state.players[1].life, 17);
        assert_eq!(state.players[1].poison_counters, 0);
    }

    #[test]
    fn infect_applies_minus_counters_to_creature() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: -1/-1 counters on creature
        assert_eq!(state.objects[&blocker].damage_marked, 0);
        assert_eq!(
            state.objects[&blocker]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            3
        );
    }

    #[test]
    fn infect_to_player_gives_poison_no_life_loss() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Infector", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect: poison counters, no life loss
        assert_eq!(state.players[1].life, 20);
        assert_eq!(state.players[1].poison_counters, 3);
    }

    #[test]
    fn lifelink_works_with_infect() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "InfectLinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Infect);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Infect gives poison, but lifelink still triggers
        assert_eq!(state.players[1].poison_counters, 3);
        assert_eq!(state.players[0].life, 23); // gained 3 life
    }

    #[test]
    fn commander_damage_tracked_when_commander_hits_player() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 5, 5);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Commander dealt 5 damage to player 1
        assert_eq!(state.players[1].life, 15);
        // Commander damage tracked
        assert_eq!(state.commander_damage.len(), 1);
        assert_eq!(state.commander_damage[0].player, PlayerId(1));
        assert_eq!(state.commander_damage[0].commander, attacker);
        assert_eq!(state.commander_damage[0].damage, 5);
    }

    #[test]
    fn commander_damage_accumulates_over_multiple_combats() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Commander", 3, 3);
        state.objects.get_mut(&attacker).unwrap().is_commander = true;
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(state.commander_damage[0].damage, 3);

        // Second combat
        state.combat = None;
        state.objects.get_mut(&attacker).unwrap().tapped = false;
        setup_combat(&mut state, vec![attacker], vec![]);
        events.clear();
        resolve_combat_damage(&mut state, &mut events);

        // Accumulated: 3 + 3 = 6
        assert_eq!(state.commander_damage[0].damage, 6);
    }

    #[test]
    fn non_commander_damage_not_tracked() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        // is_commander defaults to false
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 18);
        assert!(state.commander_damage.is_empty());
    }

    #[test]
    fn different_commanders_tracked_separately() {
        let mut state = setup();
        let cmd_a = create_creature(&mut state, PlayerId(0), "Cmd A", 3, 3);
        state.objects.get_mut(&cmd_a).unwrap().is_commander = true;
        let cmd_b = create_creature(&mut state, PlayerId(0), "Cmd B", 2, 2);
        state.objects.get_mut(&cmd_b).unwrap().is_commander = true;
        setup_combat(&mut state, vec![cmd_a, cmd_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Two separate entries
        assert_eq!(state.commander_damage.len(), 2);
        let entry_a = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_a)
            .unwrap();
        let entry_b = state
            .commander_damage
            .iter()
            .find(|e| e.commander == cmd_b)
            .unwrap();
        assert_eq!(entry_a.damage, 3);
        assert_eq!(entry_b.damage, 2);
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_once_per_damage_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Professional Face-Breaker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker_a = create_creature(&mut state, PlayerId(0), "Attacker A", 2, 2);
        let attacker_b = create_creature(&mut state, PlayerId(0), "Attacker B", 3, 3);
        setup_combat(&mut state, vec![attacker_a, attacker_b], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.stack.len(), 1);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CombatDamageDealtToPlayer {
                    player_id,
                    source_ids,
                } if *player_id == PlayerId(1)
                    && source_ids.len() == 2
                    && source_ids.contains(&attacker_a)
                    && source_ids.contains(&attacker_b)
            )
        }));
    }

    #[test]
    fn one_or_more_combat_damage_trigger_fires_in_each_double_strike_step() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(600),
            PlayerId(0),
            "Damage Watcher".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                    ));
                trigger.valid_source = Some(crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                ));
                trigger.valid_target = Some(crate::types::ability::TargetFilter::Player);
                trigger
            });

        let attacker = create_creature(&mut state, PlayerId(0), "Double Striker", 2, 2);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.stack.len(), 2);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            2
        );
    }

    /// Regression test: lifelink life gain during combat damage can activate a conditional
    /// static +2/+2 ability, increasing toughness before SBAs check for lethal damage.
    /// This validates the composition of three building blocks:
    /// - CR 702.15b: Lifelink life gain simultaneous with damage
    /// - CR 604.2: Static abilities create continuous effects while on the battlefield
    /// - CR 704.3: SBAs re-evaluate layers before checking lethal damage
    #[test]
    fn lifelink_conditional_static_saves_from_lethal() {
        let mut state = setup();
        state.format_config.starting_life = 20;
        // Player 0 at 26 life — one lifelink hit (gaining 2) pushes past 27 threshold.
        state.players[0].life = 26;

        // Attacker A: 3/3 — blocked by 3/3. Takes 3 damage (lethal without buff, survives with +2/+2).
        let attacker_a = create_creature(&mut state, PlayerId(0), "Tank", 3, 3);
        // Attacker B: 2/2 with lifelink — unblocked, gains 2 life for controller.
        let attacker_b = create_creature(&mut state, PlayerId(0), "Lifelinker", 2, 2);
        state
            .objects
            .get_mut(&attacker_b)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        // Blocker: 3/3 blocking Attacker A.
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 3, 3);

        // Enchantment with conditional static: "if life >= starting + 7, creatures you control get +2/+2"
        let ench_card_id = CardId(state.next_object_id);
        let ench_id = create_object(
            &mut state,
            ench_card_id,
            PlayerId(0),
            "Life Anthem".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ench_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        let static_def = StaticDefinition::continuous()
            .affected(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
            )
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            })
            .modifications(vec![
                ContinuousModification::AddPower { value: 2 },
                ContinuousModification::AddToughness { value: 2 },
            ]);
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);

        setup_combat(
            &mut state,
            vec![attacker_a, attacker_b],
            vec![(attacker_a, vec![blocker])],
        );
        state.layers_dirty = true;

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.15b: Lifelink gained 2 life (26 → 28).
        assert_eq!(state.players[0].life, 28);

        // CR 604.2 + CR 704.3: Static +2/+2 activated before SBA lethal check.
        // Attacker A survived — toughness 5 (3 base + 2 static), damage was only 3.
        assert!(
            state.battlefield.contains(&attacker_a),
            "Attacker A should survive: toughness 5 (3+2) > 3 damage"
        );
        assert_eq!(state.objects[&attacker_a].damage_marked, 3);

        // Blocker died — took 3 damage on 3 toughness (attacker dealt 3 at assignment time).
        assert!(
            !state.battlefield.contains(&blocker),
            "Blocker should be destroyed: 3 damage >= 3 toughness"
        );

        // Defending player took 2 from unblocked lifelinker.
        assert_eq!(state.players[1].life, 18);
    }

    fn create_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // CR 306.5b: loyalty field and counter map mirror each other.
        obj.loyalty = Some(loyalty);
        obj.counters
            .insert(crate::types::counter::CounterType::Loyalty, loyalty);
        id
    }

    // CR 510.1b: Unblocked creature attacking a planeswalker deals damage to the PW, not the player.
    #[test]
    fn unblocked_attacker_damages_planeswalker_not_player() {
        use crate::game::combat::AttackTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Test Planeswalker", 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        });

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // PW should have lost 2 loyalty (4 → 2), player life unchanged
        let pw_obj = state.objects.get(&pw).unwrap();
        assert_eq!(
            pw_obj.loyalty,
            Some(2),
            "PW should have 2 loyalty after 2 damage"
        );
        assert_eq!(state.players[1].life, 20, "Player life should be unchanged");
    }

    // CR 702.19f: Regular trample excess goes to the PW, not the defending player.
    #[test]
    fn trample_excess_goes_to_planeswalker_not_player() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Big Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Small Blocker", 1, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Test Planeswalker", 6);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        // Assign blocker
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        if let Some(info) = combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
        {
            info.blocked = true;
        }
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // Blocker has 2 toughness: 2 damage lethal, 3 excess to PW (not player)
        let pw_obj = state.objects.get(&pw).unwrap();
        assert_eq!(
            pw_obj.loyalty,
            Some(3),
            "PW should have 3 loyalty (6 - 3 trample excess)"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Player life should be unchanged — CR 702.19f"
        );
    }

    // CR 506.4c: If the PW leaves the battlefield before damage, attacker deals no damage.
    #[test]
    fn planeswalker_leaves_before_damage_no_damage_dealt() {
        use crate::game::combat::AttackTarget;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Doomed Planeswalker", 3);
        let pw_attack_target = AttackTarget::Planeswalker(pw);

        // Set up combat with attacker targeting the PW
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(attacker, pw_attack_target, PlayerId(1))],
            ..Default::default()
        });

        // Remove the PW from battlefield before damage
        if let Some(obj) = state.objects.get_mut(&pw) {
            obj.zone = Zone::Graveyard;
        }
        state.battlefield.retain(|&id| id != pw);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 506.4c: No damage to player OR planeswalker
        assert_eq!(
            state.players[1].life, 20,
            "Player should take no damage when PW left"
        );
    }

    // ── Trample Over Planeswalkers (CR 702.19c) ────────────────────────────

    // CR 702.19c: Single blocker + PW target + trample-over-PW → splits blocker/PW/controller.
    #[test]
    fn trample_over_pw_single_blocker_splits_damage() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        // 7/7 with trample over planeswalkers
        let attacker = create_creature(&mut state, PlayerId(0), "Big Trampler", 7, 7);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 7 power: 2 lethal to blocker, 3 to PW (loyalty), 2 to PW controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty (3 - 3)"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (7 - 2 blocker - 3 PW loyalty)"
        );
    }

    // CR 702.19f preserved: regular trample excess stays on PW, not controller.
    #[test]
    fn regular_trample_excess_stays_on_pw_not_controller() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 7, 7);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19f: All 5 excess (7 - 2 lethal) goes to PW, not player
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should lose all loyalty to excess"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Player should take NO damage — CR 702.19f"
        );
    }

    // CR 702.19e: PW removed + trample-over-PW → damage redirects to defending player.
    #[test]
    fn trample_over_pw_redirects_when_pw_removed() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Doomed PW", 4);

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        });

        // Remove PW before damage
        state.objects.get_mut(&pw).unwrap().zone = Zone::Graveyard;
        state.battlefield.retain(|&id| id != pw);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19e: All damage to defending player
        assert_eq!(
            state.players[1].life, 15,
            "5 damage should redirect to defending player — CR 702.19e"
        );
    }

    // CR 702.19b: Trample-over-PW attacking a player behaves like standard trample.
    #[test]
    fn trample_over_pw_attacking_player_behaves_as_standard() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 5 power: 2 lethal to blocker, 3 trample to player (same as standard trample)
        assert_eq!(
            state.players[1].life, 17,
            "3 trample damage to player — CR 702.19b"
        );
    }

    // CR 702.19d: Trample + blocked but no blockers remaining → damage to attack target.
    #[test]
    fn trample_blocked_no_blockers_damages_attack_target() {
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        // Set up combat with blocker, then remove the blocker
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);
        // Remove blocker from the assignment list (simulating it left before damage)
        if let Some(c) = &mut state.combat {
            c.blocker_assignments.insert(attacker, vec![]);
        }

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19d: All damage to defending player
        assert_eq!(
            state.players[1].life, 16,
            "4 trample damage to player — CR 702.19d"
        );
    }

    // CR 702.19d + 702.19c: Trample-over-PW + blocked but no blockers + attacking PW.
    #[test]
    fn trample_over_pw_blocked_no_blockers_splits_pw_controller() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        // Blocker was assigned but then removed
        combat.blocker_assignments.insert(attacker, vec![]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.19d + 702.19c: 3 to PW (loyalty), 2 to controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (5 - 3 PW loyalty)"
        );
    }

    // CR 702.19c + CR 702.2c: Deathtouch + trample-over-PW maximizes spillover.
    #[test]
    fn deathtouch_trample_over_pw_maximizes_spillover() {
        use crate::game::combat::AttackTarget;
        use crate::types::keywords::Keyword;

        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "DT Trampler", 6, 6);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::TrampleOverPlaneswalkers);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Jace", 3);

        let mut combat = CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        combat
            .attackers
            .iter_mut()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .blocked = true;
        state.combat = Some(combat);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // 6 power: 1 deathtouch lethal to blocker, 3 to PW (loyalty), 2 to controller
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(0),
            "PW should have 0 loyalty"
        );
        assert_eq!(
            state.players[1].life, 18,
            "Player should take 2 damage (6 - 1 deathtouch - 3 PW loyalty)"
        );
    }

    // Keyword FromStr round-trip.
    #[test]
    fn keyword_from_str_trample_over_planeswalkers() {
        use crate::types::keywords::Keyword;
        let kw: Keyword = "trample over planeswalkers".parse().unwrap();
        assert_eq!(kw, Keyword::TrampleOverPlaneswalkers);
        // "trample" must still parse to regular Trample
        let kw2: Keyword = "trample".parse().unwrap();
        assert_eq!(kw2, Keyword::Trample);
    }
}
