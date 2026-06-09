use crate::game::combat::{AttackTarget, CombatState, DamageAssignment, DamageTarget, TrampleKind};
use crate::game::effects::deal_damage::{
    apply_damage_after_replacement, pre_replacement_damage_gate, DamageContext, DamageResult,
};
use crate::game::game_object::GameObject;
use crate::game::replacement;
use crate::game::sba;
use crate::game::triggers;
use crate::types::ability::TargetRef;
use crate::types::events::GameEvent;
use crate::types::game_state::{CombatDamageAssignmentMode, DamageSlot, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// CR 510.1a + CR 613.11: Returns the amount of combat damage a creature assigns.
/// Normally equal to power, but if `assigns_damage_from_toughness` is set (e.g. Doran),
/// uses toughness instead. If `assigns_no_combat_damage` is set, returns 0.
fn combat_damage_amount(obj: &GameObject) -> u32 {
    // CR 510.1a: "~ assigns no combat damage" — creature deals 0 combat damage.
    if obj.assigns_no_combat_damage {
        return 0;
    }
    if obj.assigns_damage_from_toughness {
        // CR 613.11 + CR 510.1a: Rule effect uses toughness rather than power.
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
            // CR 510.4: The regular combat-damage sub-step is a fresh assignment.
            // `damage_assignments` is the per-sub-step blocker resume-skip key
            // (CR 702.22k / CR 510.1d), so it MUST be reset between sub-steps —
            // otherwise a double-strike blocker that divided its first-strike
            // damage would be wrongly skipped in the regular sub-step.
            c.damage_assignments.clear();
        }

        // CR 510.4: SBAs and triggers run between first-strike and regular damage sub-steps.
        process_combat_damage_triggers(state, &damage_events, events);

        // CR 510.4 + CR 603.3b: if the first-strike sub-step produced a same-
        // controller trigger-ordering prompt, surface it now — before the regular
        // sub-step's own trigger processing clobbers/orphans it. Returning here
        // leaves `regular_damage_done == false`; the mandatory second (regular)
        // combat-damage sub-step is resumed by the priority-pass completeness gate
        // in priority.rs, which re-enters this function once the order is submitted
        // and the resulting triggers resolve.
        if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
            return Some(state.waiting_for.clone());
        }

        // CR 510.3 + CR 510.3a + CR 510.4: The first-strike combat-damage step is a
        // complete step. Abilities that triggered on first-strike damage (or on SBAs
        // taken afterward) are put on the stack, and THEN the active player receives
        // priority — players must finish with the stack before the second (regular)
        // combat-damage sub-step begins. If the first-strike sub-step placed anything
        // on the stack (e.g. No Mercy's "destroy that creature", a damage trigger that
        // bounces/exiles the attacker), grant priority now so that object resolves
        // first. Skipping this would let a now-doomed double-strike attacker deal its
        // regular-sub-step damage before the trigger that removes it resolves (#692).
        // Returning here leaves `regular_damage_done == false`; the mandatory regular
        // sub-step is resumed once the stack drains and all players pass, via the
        // empty-stack completeness gate in priority.rs.
        if !state.stack.is_empty() {
            // reset_priority here is defensive — unlike the sibling regular-substep entry in
            // turns.rs, this returns mid-step after the first-strike substep, so we explicitly
            // clear any stale passes before the CR 510.3 priority window (harmless if already clear).
            crate::game::priority::reset_priority(state);
            return Some(WaitingFor::Priority {
                player: state.active_player,
            });
        }
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

        // CR 510.1c + CR 702.19b: Check if interactive assignment is needed
        // (2+ blockers, or a single-blocker trample attacker with excess damage).
        if needs_interactive_assignment(
            state,
            obj,
            &combat,
            attacker_info,
            power,
            has_deathtouch,
            trample,
        ) {
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

            // The player who assigns damage is normally the attacker's controller.
            let mut controller = state
                .objects
                .get(&attacker_info.object_id)
                .map(|o| o.controller)
                .unwrap_or(state.active_player);

            // CR 702.22j: During the combat damage step, if an attacking creature
            // is being blocked by a creature with banding or a qualifying "bands
            // with other" relation, the DEFENDING player (rather than the active
            // player) chooses how the attacking creature's damage is assigned.
            // We reach this branch whenever an interactive assignment is required
            // — both the multi-blocker case and the single banded-blocker-with-
            // trample case.
            let blocked_by_banding = combat
                .blocker_assignments
                .get(&attacker_info.object_id)
                .into_iter()
                .flatten()
                .any(|&bid| crate::game::combat::has_banding(state, bid))
                || crate::game::combat::has_bands_with_other_damage_assignment_group(
                    state,
                    &blocker_ids,
                );
            if blocked_by_banding {
                controller = attacker_info.defending_player;
            }

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

    // CR 510.1c/d: The attacker loop has fully processed every attacker for this
    // sub-step. Advance the cursor past the end so that a later re-entry (e.g.
    // after an interactive CR 702.22k blocker-division prompt below) skips the
    // attacker loop and does NOT re-push attacker auto-assignments — which would
    // double-count attacker damage. Without an interactive blocker this is a
    // no-op (the function returns `None` at the bottom and the sub-step applies).
    if let Some(c) = &mut state.combat {
        c.damage_step_index = Some(c.attackers.len());
    }

    // --- Blockers ---
    // CR 510.1d: A blocking creature assigns its combat damage to the creatures
    // it's blocking, divided as its controller chooses (CR 702.22k re-routes the
    // chooser to the active player when a blocked attacker has banding).
    //
    // Re-entrancy: unlike attackers (a `Vec` indexed by `damage_step_index`),
    // blockers live in a `HashMap` with no positional cursor. The resume-skip key
    // is membership in `combat.damage_assignments` (keyed by blocker id), which is
    // recorded for EVERY processed blocker — both the auto-even-split path and the
    // interactive CR 702.22k path. This keeps an already-divided blocker from being
    // re-pushed when we re-enter after an interactive prompt resolves. The
    // skip-key set is reset per sub-step (see `resolve_combat_damage`), so a
    // double-strike blocker is correctly re-processed in the regular sub-step.
    //
    // Deterministic order: `ObjectId(pub u64)` is not `Ord`, so we sort the
    // blocker ids by their inner `u64` to guarantee a stable prompt sequence
    // across AI clones and save/reload.
    let mut blocker_ids: Vec<ObjectId> = combat.blocker_to_attacker.keys().copied().collect();
    blocker_ids.sort_by_key(|id| id.0);

    for blocker_id in blocker_ids {
        // Resume-skip: this blocker's division was already recorded this sub-step.
        if combat
            .damage_assignments
            .get(&blocker_id)
            .is_some_and(|v| !v.is_empty())
        {
            continue;
        }
        let attacker_ids = match combat.blocker_to_attacker.get(&blocker_id) {
            Some(ids) => ids,
            None => continue,
        };
        let obj = match state.objects.get(&blocker_id) {
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

        // CR 702.22k: During the combat damage step, if a blocking creature is
        // blocking a creature with banding or a qualifying "bands with other"
        // relation, the ACTIVE player (rather than the defending player) chooses
        // how the blocking creature's damage is divided among the attackers it's
        // blocking. This only matters when the blocker is blocking 2+ attackers.
        let active_player_divides = attacker_ids.len() >= 2
            && (attacker_ids
                .iter()
                .any(|&aid| crate::game::combat::has_banding(state, aid))
                || crate::game::combat::has_bands_with_other_damage_assignment_group(
                    state,
                    attacker_ids,
                ));

        if active_player_divides {
            return Some(WaitingFor::AssignBlockerDamage {
                player: state.active_player,
                blocker_id,
                total_damage: power,
                attackers: attacker_ids.clone(),
            });
        }

        // CR 510.1d: default — controller's blocker divides evenly (auto-split).
        let blocker_assignments = distribute_blocker_damage(blocker_id, power, attacker_ids);
        if let Some(c) = &mut state.combat {
            c.pending_damage.extend(blocker_assignments);
            // Record into the resume-skip key so re-entry after an interactive
            // CR 702.22k prompt does not re-push this blocker's division.
            c.damage_assignments.insert(
                blocker_id,
                vec![DamageAssignment {
                    target: DamageTarget::Object(attacker_ids[0]),
                    amount: power,
                }],
            );
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
/// how to divide damage. Single-blocker and unblocked scenarios are auto-assigned,
/// except a single-blocker trample attacker with excess damage (see CR 702.19b below).
pub(crate) fn needs_interactive_assignment(
    state: &GameState,
    obj: &GameObject,
    combat: &CombatState,
    attacker_info: &crate::game::combat::AttackerInfo,
    power: u32,
    has_deathtouch: bool,
    trample: Option<TrampleKind>,
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

    // CR 702.19b: A trample attacker blocked by exactly one creature still gets an
    // interactive choice when it has excess damage — the controller may keep the
    // excess on the blocker or assign it to the player (assigning none to the player
    // is legal). With no excess (power <= lethal) all damage goes to the blocker
    // (CR 510.1c), so there is no choice → auto-assign.
    if trample.is_some() && blocker_count == 1 {
        if let Some(&blocker_id) = combat
            .blocker_assignments
            .get(&attacker_info.object_id)
            .and_then(|b| b.first())
        {
            return power > lethal_damage_needed(state, blocker_id, has_deathtouch);
        }
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
                    // CR 702.19b: Single-blocker trample with excess is now routed to the
                    // interactive WaitingFor::AssignCombatDamage prompt by
                    // needs_interactive_assignment, so this auto-assign branch is reached only
                    // when power <= lethal (excess == 0). The excess sub-block below stays as
                    // the shared lethal-then-excess logic used by the no-blockers and PW paths.
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

/// CR 510.2: One source's place in a simultaneous combat-damage batch — the
/// damage context, commander flag, and original assignment, carried alongside
/// the source's `ProposedEvent` so Phase C can apply the post-replacement
/// survivor and run combat-only bookkeeping.
struct BatchEntry<'a> {
    ctx: DamageContext,
    source_is_commander: bool,
    assignment: &'a DamageAssignment,
}

/// Apply all combat damage assignments simultaneously (CR 510.2).
///
/// Combat damage is one simultaneous event batch (CR 510.2). To keep prevention
/// shields (e.g. Inkshield's "prevent all combat damage ... for each 1 damage
/// prevented, create a token") rules-correct (CR 615.7 — count the amount, not
/// the sources; CR 615.13 — the rider fires once per batch), this runs in
/// three phases:
///
/// - **Phase A (Collect):** build a `ProposedEvent::Damage` per assignment
///   through `pre_replacement_damage_gate` (0-damage / protection / prohibition
///   gates) WITHOUT applying any damage yet.
/// - **Phase B (Batch replace):** pass the whole proposed-event slice through
///   `replace_combat_damage_batch`, which runs the replacement pipeline per
///   event but aggregates each `Prevention::All` shield's prevented amount.
/// - **Phase C (Apply + bookkeeping):** apply each surviving post-replacement
///   event via `apply_damage_after_replacement`, then run the player-batching
///   and commander-damage bookkeeping. Afterward, fire each prevention shield's
///   `runtime_execute` rider exactly once against the aggregate prevented
///   amount (CR 615.5 + CR 615.13).
///
/// Used by both the automatic assignment path and the interactive
/// `AssignCombatDamage` handler.
pub(crate) fn apply_combat_damage(
    state: &mut GameState,
    assignments: &[(ObjectId, DamageAssignment)],
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    // CR 510.2: accumulates per-player, per-source damage for this step only.
    // `(player, [(source_id, amount)], step_total)`.
    type PerPlayerCombatDamage = (crate::types::player::PlayerId, Vec<(ObjectId, u32)>, u32);
    let mut combat_damage_to_players: Vec<PerPlayerCombatDamage> = Vec::new();

    // --- Phase A: Collect proposed damage events (CR 510.2) ---
    // Gated assignments (0-damage, protection, prohibition) contribute nothing
    // and are dropped here; the gate already emitted any required DamagePrevented.
    let mut entries: Vec<BatchEntry> = Vec::with_capacity(assignments.len());
    let mut proposed_events: Vec<ProposedEvent> = Vec::with_capacity(assignments.len());
    for (source_id, assignment) in assignments {
        // Read commander flag before DamageContext borrows — both are immutable reads.
        let source_is_commander = state
            .objects
            .get(source_id)
            .map(|o| o.is_commander)
            .unwrap_or(false);
        // In practice, from_source always succeeds during combat (source is on battlefield).
        // CR 702.15c: Fallback uses LKI controller when the source is gone.
        let ctx = DamageContext::from_source(state, *source_id).unwrap_or_else(|| {
            let controller = state
                .lki_cache
                .get(source_id)
                .map(|lki| lki.controller)
                .unwrap_or(state.active_player);
            DamageContext::fallback(*source_id, controller)
        });

        let target_ref = match &assignment.target {
            DamageTarget::Object(id) => TargetRef::Object(*id),
            DamageTarget::Player(id) => TargetRef::Player(*id),
        };

        if let Some(proposed) = pre_replacement_damage_gate(
            state,
            &ctx,
            &target_ref,
            assignment.amount,
            true,
            &mut events,
        ) {
            entries.push(BatchEntry {
                ctx,
                source_is_commander,
                assignment,
            });
            proposed_events.push(proposed);
        }
    }

    // --- Phase B: Batch replacement (CR 510.2 + CR 615.7) ---
    let (survivors, prevention_tally) =
        replacement::replace_combat_damage_batch(state, &mut events, proposed_events);

    // --- Phase C: Apply survivors + combat bookkeeping (CR 120.3 + CR 510.2) ---
    debug_assert_eq!(
        entries.len(),
        survivors.len(),
        "batch survivor count must align with collected entries"
    );
    for (entry, survivor) in entries.iter().zip(survivors) {
        let actual_amount = match survivor {
            // CR 120.3 + CR 120.4b: Apply the post-replacement event WITHOUT
            // re-running the replacement pipeline.
            Some(survivor_event) => match apply_damage_after_replacement(
                state,
                &entry.ctx,
                survivor_event,
                true,
                &mut events,
            ) {
                DamageResult::Applied(amt) => amt,
                // CR 510.2: Life-loss/lifelink replacement needs a choice, but no
                // player gets priority between combat damage being assigned and
                // dealt — combat cannot pause, so the deferred portion is dropped
                // (mirrors the legacy `apply_damage_to_target` combat behavior).
                DamageResult::NeedsChoice => 0,
            },
            // Fully prevented or skipped — no damage applied.
            None => 0,
        };

        // Combat-only bookkeeping (not part of the shared damage pipeline):
        if let DamageTarget::Player(player_id) = &entry.assignment.target {
            let source_id = entry.ctx.source_id;
            // CR 510.2: Track per-source amounts for this step. Each source
            // appears at most once per player per step; dedup guards any edge
            // where the same source is re-applied (e.g. split-damage riders).
            if let Some((_, sources, total)) = combat_damage_to_players
                .iter_mut()
                .find(|(damaged_player, _, _)| *damaged_player == *player_id)
            {
                if let Some((_, amt)) = sources.iter_mut().find(|(id, _)| *id == source_id) {
                    *amt += actual_amount;
                } else {
                    sources.push((source_id, actual_amount));
                }
                *total += actual_amount;
            } else {
                combat_damage_to_players.push((
                    *player_id,
                    vec![(source_id, actual_amount)],
                    actual_amount,
                ));
            }

            // CR 704.6c: Track commander combat damage for the 21-damage loss condition.
            if entry.source_is_commander && actual_amount > 0 {
                if let Some(entry) = state
                    .commander_damage
                    .iter_mut()
                    .find(|e| e.player == *player_id && e.commander == source_id)
                {
                    entry.damage += actual_amount;
                } else {
                    state
                        .commander_damage
                        .push(crate::types::game_state::CommanderDamageEntry {
                            player: *player_id,
                            commander: source_id,
                            damage: actual_amount,
                        });
                }
            }
        }
    }

    for (player_id, source_amounts, total_damage) in combat_damage_to_players {
        events.push(GameEvent::CombatDamageDealtToPlayer {
            player_id,
            source_amounts,
            total_damage,
        });
    }

    // --- Phase D: Fire prevention riders once per shield (CR 615.5 + CR 615.13) ---
    fire_combat_prevention_riders(state, &prevention_tally, &mut events);

    events
}

/// CR 615.5 + CR 615.13: After a simultaneous combat-damage batch, fire each
/// `Prevention::All` shield's `runtime_execute` rider exactly once against the
/// aggregate prevented amount.
///
/// Each `DamagePrevented` event for the batch is emitted here (the per-source
/// applier suppressed them so the rider sees one un-fragmented amount). The
/// aggregate prevented amount is stamped into `last_effect_count` immediately
/// before the single rider call so the rider's `QuantityRef::EventContextAmount`
/// (e.g. Inkshield's "for each 1 damage prevented this way") resolves against
/// the whole batch total.
fn fire_combat_prevention_riders(
    state: &mut GameState,
    prevention_tally: &std::collections::HashMap<crate::types::proposed_event::ReplacementId, i32>,
    events: &mut Vec<GameEvent>,
) {
    for (rid, &total_prevented) in prevention_tally {
        if total_prevented <= 0 {
            continue;
        }

        if replacement::is_shield_counter_damage_replacement(*rid) {
            replacement::consume_shield_counter(state, rid.source, events);
            events.push(GameEvent::DamagePrevented {
                source_id: rid.source,
                target: TargetRef::Object(rid.source),
                amount: total_prevented as u32,
            });
            continue;
        }

        // CR 615.3: Pending shields use sentinel ObjectId(0); object-hosted
        // shields are found in the host's replacement_definitions.
        let repl_def = if rid.source == ObjectId(0) {
            state.pending_damage_replacements.get(rid.index)
        } else {
            state
                .objects
                .get(&rid.source)
                .and_then(|obj| obj.replacement_definitions.get(rid.index))
        };
        let Some(repl_def) = repl_def else {
            continue;
        };

        // CR 615.13: The `DamagePrevented` event for the whole batch — informational
        // (no trigger consumes it yet). Target derived from the shield's player scope.
        let prevented_target = match &repl_def.damage_target_filter {
            Some(crate::types::ability::DamageTargetFilter::Player {
                player: crate::types::ability::DamageTargetPlayerScope::Specific(player),
            }) => TargetRef::Player(*player),
            _ => TargetRef::Object(rid.source),
        };
        events.push(GameEvent::DamagePrevented {
            source_id: rid.source,
            target: prevented_target,
            amount: total_prevented as u32,
        });

        // CR 615.5: Resolve the prevention's additional effect ("for each 1
        // damage prevented this way, create a token"). Stamp the aggregate
        // prevented amount so `EventContextAmount` resolves against the batch
        // total, then run the rider continuation exactly once.
        let Some(runtime) = repl_def.runtime_execute.clone() else {
            continue;
        };
        state.last_effect_count = Some(total_prevented);
        state.post_replacement_continuation =
            Some(crate::types::ability::PostReplacementContinuation::Resolved(runtime));
        let _ = crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state, None, None, None, events,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, Comparator, ContinuousModification, ControllerRef, Effect, QuantityExpr,
        QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TriggerDefinition,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, TrackedSetId};
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

    fn add_wolf_subtype(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push("Wolf".to_string());
    }

    fn grant_bands_with_other_wolves(state: &mut GameState, id: ObjectId) {
        add_wolf_subtype(state, id);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::BandsWithOther("Wolf".to_string()));
    }

    /// Drive `resolve_combat_damage` and, whenever it pauses on an
    /// `AssignCombatDamage` prompt (now reachable for single-blocker trample with
    /// excess per CR 702.19b, as well as the existing 2+ blocker case), submit the
    /// canonical greedy split through the real apply path: lethal to each blocker
    /// in order, then trample the remainder to the attack target (splitting
    /// loyalty-worth to the PW and the rest to its controller for trample-over-PW).
    /// This mirrors `run_combat` in the integration harness and the AI default, so
    /// tests that previously relied on auto-assignment keep asserting that split.
    fn resolve_combat_with_greedy_assignment(state: &mut GameState, events: &mut Vec<GameEvent>) {
        if let Some(waiting) = resolve_combat_damage(state, events) {
            state.waiting_for = waiting;
        } else {
            return;
        }
        while let WaitingFor::AssignCombatDamage {
            attacker_id,
            total_damage,
            ref blockers,
            ref trample,
            pw_loyalty,
            ref attack_target,
            ..
        } = state.waiting_for
        {
            let _ = attacker_id;
            let mut remaining = total_damage;
            let mut assignments: Vec<(ObjectId, u32)> = Vec::new();
            for slot in blockers {
                let assign = remaining.min(slot.lethal_minimum);
                assignments.push((slot.blocker_id, assign));
                remaining = remaining.saturating_sub(assign);
            }
            if trample.is_none() && remaining > 0 {
                if let Some(last) = assignments.last_mut() {
                    last.1 += remaining;
                    remaining = 0;
                }
            }
            let (trample_damage, controller_damage) = if *trample
                == Some(TrampleKind::OverPlaneswalkers)
                && matches!(attack_target, AttackTarget::Planeswalker(_))
            {
                let loyalty = pw_loyalty.unwrap_or(0);
                let to_pw = remaining.min(loyalty);
                (to_pw, remaining.saturating_sub(to_pw))
            } else {
                (if trample.is_some() { remaining } else { 0 }, 0)
            };
            crate::game::engine::apply_as_current(
                state,
                crate::types::actions::GameAction::AssignCombatDamage {
                    mode: CombatDamageAssignmentMode::Normal,
                    assignments,
                    trample_damage,
                    controller_damage,
                },
            )
            .expect("greedy combat damage assignment must be legal");
        }
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

    // VALIDATION repro for Discord #766/#767: a creature buffed by a transient
    // +1/+1 (Thoughtweft Lieutenant's ETB pump, Blossoming Defense, etc.) must
    // have lethal combat damage measured against its MODIFIED toughness, not its
    // base toughness. CR 510.1c + CR 704.5g: a creature is destroyed only when
    // damage marked on it is >= its current toughness.
    #[test]
    fn buffed_attacker_survives_lethal_against_base_stat_blocker() {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};
        use crate::types::game_state::TransientContinuousEffect;

        let mut state = setup();

        // Attacker: printed 2/2, buffed +1/+1 → effective 3/3.
        let attacker = create_creature(&mut state, PlayerId(0), "Kithkin", 2, 2);
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        let ts = state.next_timestamp();
        state
            .transient_continuous_effects
            .push_back(TransientContinuousEffect {
                id: 1,
                source_id: attacker,
                controller: PlayerId(0),
                timestamp: ts,
                duration: Duration::UntilEndOfTurn,
                affected: TargetFilter::SelfRef,
                modifications: vec![
                    ContinuousModification::AddPower { value: 1 },
                    ContinuousModification::AddToughness { value: 1 },
                ],
                condition: None,
                source_name: String::new(),
            });

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&attacker].toughness,
            Some(3),
            "buff must apply: 2/2 + 1/+1 = 3/3"
        );

        // Blocker equal to the attacker's BASE stats (2/2).
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.objects[&attacker].damage_marked, 2,
            "blocker deals 2 to attacker"
        );

        sba::check_state_based_actions(&mut state, &mut events);

        // The buffed 3/3 took only 2 damage → it must SURVIVE.
        assert_eq!(
            state.objects[&attacker].zone,
            Zone::Battlefield,
            "buffed 3/3 must survive 2 damage from a 2/2 blocker (#766/#767)"
        );
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

    /// CR 122.1c + CR 510.2: a single shield counter prevents ALL combat damage
    /// dealt to the permanent in one simultaneous batch and is removed exactly
    /// once, even when multiple sources deal damage to it.
    #[test]
    fn shield_counter_prevents_all_simultaneous_combat_damage_once() {
        let mut state = setup();
        let shielded = create_creature(&mut state, PlayerId(1), "Shielded Bear", 2, 2);
        state
            .objects
            .get_mut(&shielded)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 1);
        let atk1 = create_creature(&mut state, PlayerId(0), "Attacker A", 3, 3);
        let atk2 = create_creature(&mut state, PlayerId(0), "Attacker B", 3, 3);

        let assignments = vec![
            (
                atk1,
                DamageAssignment {
                    target: DamageTarget::Object(shielded),
                    amount: 3,
                },
            ),
            (
                atk2,
                DamageAssignment {
                    target: DamageTarget::Object(shielded),
                    amount: 3,
                },
            ),
        ];
        let events = apply_combat_damage(&mut state, &assignments);

        assert_eq!(
            state.objects[&shielded].damage_marked, 0,
            "all simultaneous combat damage must be prevented"
        );
        assert_eq!(
            state.objects[&shielded].counters.get(&CounterType::Shield),
            None,
            "exactly one shield counter consumed for the whole batch"
        );
        let removed = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::CounterRemoved {
                        counter_type: CounterType::Shield,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(removed, 1, "shield counter removed exactly once");
        let prevented = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
            .count();
        assert_eq!(
            prevented, 1,
            "one aggregate DamagePrevented event emitted for the simultaneous batch"
        );
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
        // CR 702.19b: 5/5 trample vs a single 2/2 has excess → interactive prompt
        // (the controller may keep the excess on the blocker or trample it through).
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess prompts for assignment");
        state.waiting_for = waiting;
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 2)],
                trample_damage: 3,
                controller_damage: 0,
            },
        )
        .expect("lethal-to-blocker + trample-through is legal");

        // 2 lethal to blocker (dies via SBA), 3 trample excess to player.
        assert!(!state.battlefield.contains(&blocker));
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

    /// CR 702.15b: Damage dealt by a source with lifelink causes that source's
    /// controller to gain that much life — regardless of whether the damage is dealt
    /// to a player or to a blocking creature. Regression test for GH #324: user
    /// reported lifelink did not credit life when the attacker was blocked.
    #[test]
    fn lifelink_gains_life_when_attacker_is_blocked() {
        let mut state = setup();
        // 3/3 attacker with lifelink, 2/2 vanilla blocker.
        let attacker = create_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.15b: Controller gains life equal to damage dealt to the blocker (3).
        assert_eq!(
            state.players[0].life, 23,
            "Lifelink attacker should gain life from damage dealt to the blocker"
        );
        // Defending player took no damage (attacker was blocked).
        assert_eq!(state.players[1].life, 20);
        // Blocker took 3 damage (and dies via SBA).
        // Either damage_marked is 3 or it's already in the graveyard.
        let blocker_dead = state
            .objects
            .get(&blocker)
            .map(|o| o.zone != Zone::Battlefield)
            .unwrap_or(true);
        assert!(blocker_dead, "Blocker should have died from 3 damage");
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
    fn bands_with_other_blocker_reroutes_attacker_damage_assignment() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", 5, 5);
        let blocker1 = create_creature(&mut state, PlayerId(1), "Wolf Blocker", 2, 2);
        let blocker2 = create_creature(&mut state, PlayerId(1), "Plain Wolf Blocker", 2, 2);
        grant_bands_with_other_wolves(&mut state, blocker1);
        add_wolf_subtype(&mut state, blocker2);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker1, blocker2])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match waiting {
            Some(WaitingFor::AssignCombatDamage { player, .. }) => {
                assert_eq!(
                    player,
                    PlayerId(1),
                    "CR 702.22j: defending player assigns attacker damage"
                );
            }
            other => panic!("expected AssignCombatDamage, got {other:?}"),
        }
    }

    #[test]
    fn bands_with_other_attackers_reroute_blocker_damage_assignment() {
        let mut state = setup();
        let attacker1 = create_creature(&mut state, PlayerId(0), "Wolf A", 2, 2);
        let attacker2 = create_creature(&mut state, PlayerId(0), "Wolf B", 2, 2);
        let blocker = create_creature(&mut state, PlayerId(1), "Guard", 3, 3);
        grant_bands_with_other_wolves(&mut state, attacker1);
        add_wolf_subtype(&mut state, attacker2);
        setup_combat(
            &mut state,
            vec![attacker1, attacker2],
            vec![(attacker1, vec![blocker]), (attacker2, vec![blocker])],
        );

        let mut events = Vec::new();
        let waiting = resolve_combat_damage(&mut state, &mut events);
        match waiting {
            Some(WaitingFor::AssignBlockerDamage { player, .. }) => {
                assert_eq!(
                    player,
                    PlayerId(0),
                    "CR 702.22k: active player assigns blocker damage"
                );
            }
            other => panic!("expected AssignBlockerDamage, got {other:?}"),
        }
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
    fn toxic_to_player_adds_poison_and_still_deals_damage() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Toxic", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Toxic(2));
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 17);
        assert_eq!(state.players[1].poison_counters, 2);
    }

    #[test]
    fn toxic_to_creature_does_not_add_poison() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Toxic", 3, 3);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Toxic(2));
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 4);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.objects[&blocker].damage_marked, 3);
        assert_eq!(state.players[1].poison_counters, 0);
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
                    source_amounts,
                    ..
                } if *player_id == PlayerId(1)
                    && source_amounts.len() == 2
                    && source_amounts.iter().any(|(id, _)| *id == attacker_a)
                    && source_amounts.iter().any(|(id, _)| *id == attacker_b)
            )
        }));
    }

    #[test]
    fn one_or_more_combat_damage_trigger_tracks_matching_sources_for_those_creatures() {
        let mut state = setup();
        let watcher = create_object(
            &mut state,
            CardId(550),
            PlayerId(0),
            "Heroes in a Half Shell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&watcher)
            .unwrap()
            .trigger_definitions
            .push({
                let valid_source = TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Mutant".to_string())
                                .controller(ControllerRef::You),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Ninja".to_string())
                                .controller(ControllerRef::You),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::default()
                                .subtype("Turtle".to_string())
                                .controller(ControllerRef::You),
                        ),
                    ],
                };
                let mut trigger = TriggerDefinition::new(TriggerMode::DamageDoneOnceByController)
                    .execute(AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        Effect::PutCounterAll {
                            counter_type: CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::TrackedSet {
                                id: TrackedSetId(0),
                            },
                        },
                    ));
                trigger.valid_source = Some(valid_source);
                trigger.valid_target = Some(TargetFilter::Player);
                trigger
            });

        let mutant = create_creature(&mut state, PlayerId(0), "Mutant", 2, 2);
        state
            .objects
            .get_mut(&mutant)
            .unwrap()
            .card_types
            .subtypes
            .push("Mutant".to_string());
        let human = create_creature(&mut state, PlayerId(0), "Human", 2, 2);
        state
            .objects
            .get_mut(&human)
            .unwrap()
            .card_types
            .subtypes
            .push("Human".to_string());
        state
            .tracked_object_sets
            .insert(TrackedSetId(99), vec![human]);
        setup_combat(&mut state, vec![mutant, human], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);

        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&mutant]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            1
        );
        assert_eq!(
            state.objects[&human]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            0
        );
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

        // Stock P0's library so the per-step draw trigger never draws from empty.
        for _ in 0..2 {
            let card_id = CardId(state.next_object_id);
            create_object(
                &mut state,
                card_id,
                PlayerId(0),
                "Lib".to_string(),
                Zone::Library,
            );
        }

        let mut events = Vec::new();
        // CR 510.4: First-strike sub-step. The double striker deals its 2 damage,
        // the DamageDone trigger fires (stack len 1), and CR 510.3 grants priority
        // before the regular sub-step — so resolve_combat_damage pauses here.
        let waiting = resolve_combat_damage(&mut state, &mut events);
        assert!(
            matches!(waiting, Some(WaitingFor::Priority { .. })),
            "CR 510.3: priority is granted after the first-strike sub-step's trigger is stacked"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "CR 510.3a: only the first-strike sub-step's trigger is on the stack so far"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            1,
            "CR 510.4: the double striker dealt damage once in the first-strike sub-step"
        );

        // CR 510.3 + CR 510.4: resolve the first-strike trigger, then re-enter the
        // turn-based action for the mandatory regular (second) combat-damage sub-step.
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(state.stack.is_empty(), "first-strike trigger resolved");
        resolve_combat_damage(&mut state, &mut events);

        // CR 510.4: The double striker deals damage AGAIN in the regular sub-step,
        // so the DamageDone trigger fires a second time (now on the stack).
        assert_eq!(state.stack.len(), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CombatDamageDealtToPlayer { .. }))
                .count(),
            2,
            "CR 510.4: the double striker dealt combat damage in both sub-steps"
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
        state.layers_dirty.mark_full();

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
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

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
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

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
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

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
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

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
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

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

    // --- #314: combat-damage prevention batch + Inkshield rider aggregation ---

    /// Resolve an Inkshield-style spell ("prevent all combat damage to `player`
    /// this turn; for each 1 damage prevented this way, create a 2/1 Inkling")
    /// through the real `PreventDamage` effect resolver. The shield lands in
    /// `pending_damage_replacements` with its `runtime_execute` Token rider.
    fn install_inkshield(state: &mut GameState, player: PlayerId) {
        use crate::game::effects::prevent_damage;
        use crate::types::ability::{
            PreventionAmount, PreventionScope, PtValue, ResolvedAbility, TargetFilter,
        };
        use crate::types::mana::ManaColor;

        let shield_source = create_object(
            state,
            CardId(state.next_object_id),
            player,
            "Inkshield".to_string(),
            Zone::Stack,
        );

        let mut token = ResolvedAbility::new(
            Effect::Token {
                name: "Inkling".to_string(),
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Inkling".to_string()],
                colors: vec![ManaColor::White, ManaColor::Black],
                keywords: vec![Keyword::Flying],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            shield_source,
            player,
        );
        token.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        });

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                scope: PreventionScope::CombatDamage,
                damage_source_filter: None,
            },
            vec![],
            shield_source,
            player,
        )
        .sub_ability(token);

        let mut events = Vec::new();
        prevent_damage::resolve(state, &ability, &mut events).unwrap();
    }

    fn count_inklings(state: &GameState) -> usize {
        state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Inkling")
            .count()
    }

    /// Step 1: Two unblocked 3/3 attackers hit an Inkshield controller. The
    /// rider must fire once against the aggregate (6), creating 6 Inklings —
    /// not 3+3 from two separate firings, and not 0 from a fragmented count.
    #[test]
    fn test_inkshield_aggregates_combat_damage_into_tokens() {
        let mut state = setup();
        // PlayerId(1) controls Inkshield; PlayerId(0) attacks them.
        install_inkshield(&mut state, PlayerId(1));

        let a1 = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let a2 = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        setup_combat(&mut state, vec![a1, a2], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 615.6: prevented damage never happens — 0 life lost.
        assert_eq!(state.players[1].life, 20, "all 6 combat damage prevented");
        // CR 615.7 + CR 615.13: one rider firing against the aggregate of 6.
        assert_eq!(
            count_inklings(&state),
            6,
            "rider fires once for the whole batch: 3 + 3 = 6 Inklings"
        );
    }

    /// Step 6: A double-strike attacker vs Inkshield. CR 510.4 — first-strike
    /// and regular damage are separate combat-damage steps, each its own
    /// simultaneous batch → its own rider firing. A 4/4 double-striker
    /// prevents 4 then 4: two firings, 8 Inklings total.
    #[test]
    fn test_inkshield_double_strike_fires_rider_per_combat_step() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let attacker = create_creature(&mut state, PlayerId(0), "Striker", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::DoubleStrike);
        setup_combat(&mut state, vec![attacker], vec![]);

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(state.players[1].life, 20, "both strikes prevented");
        // CR 510.4: two separate combat-damage steps → two rider firings of 4.
        assert_eq!(
            count_inklings(&state),
            8,
            "double strike: 4 (first-strike step) + 4 (regular step) = 8"
        );
        // CR 510.4: two DamagePrevented events, one per combat-damage step.
        let prevented = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamagePrevented { .. }))
            .count();
        assert_eq!(prevented, 2, "one DamagePrevented per combat-damage step");
    }

    /// Step 7: Trample attacker partially blocked, defending player shielded.
    /// The trample-to-player damage batches with the to-creature assignment;
    /// the shield aggregates only the player-targeted portion (CR 615.7).
    #[test]
    fn test_inkshield_trample_aggregates_only_player_portion() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 2);
        setup_combat(&mut state, vec![attacker], vec![(attacker, vec![blocker])]);

        let mut events = Vec::new();
        resolve_combat_with_greedy_assignment(&mut state, &mut events);

        // 2 lethal to the blocker, 3 trample over to the shielded player.
        // The 0/2 Wall takes its lethal portion and dies via SBA (CR 704.5g) —
        // to-creature damage is not prevented by the player-scoped shield.
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker takes its lethal portion (not prevented) and dies"
        );
        assert_eq!(state.players[1].life, 20, "trample-over damage prevented");
        // CR 615.7: only the 3 player-targeted damage is aggregated.
        assert_eq!(count_inklings(&state), 3, "3 trample damage → 3 Inklings");
    }

    /// Step 8: A mixed batch — attacker X hits a creature, attacker Y hits the
    /// shielded player, in one `apply_combat_damage` call. The shield
    /// aggregates only Y's amount (CR 615.7).
    #[test]
    fn test_inkshield_mixed_creature_and_player_batch() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let x = create_creature(&mut state, PlayerId(0), "Ogre X", 3, 3);
        let y = create_creature(&mut state, PlayerId(0), "Ogre Y", 4, 4);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 6);
        setup_combat(
            &mut state,
            vec![x, y],
            vec![(x, vec![blocker]), (y, vec![])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        assert_eq!(
            state.objects[&blocker].damage_marked, 3,
            "X's 3 to-creature damage applies normally"
        );
        assert_eq!(
            state.players[1].life, 20,
            "Y's 4 to-player damage prevented"
        );
        assert_eq!(count_inklings(&state), 4, "only Y's 4 damage → 4 Inklings");
    }

    /// Step 9: A deathtouch attacker in a batch alongside a prevented attacker.
    /// The Phase A/B/C split must not disturb deathtouch marking — the
    /// deathtouch creature still dies to SBA.
    #[test]
    fn test_deathtouch_unaffected_by_combat_damage_batch_split() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        // Deathtouch attacker vs a fat blocker; prevented attacker hits player.
        let dt = create_creature(&mut state, PlayerId(0), "Adder", 1, 1);
        state
            .objects
            .get_mut(&dt)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        let blocker = create_creature(&mut state, PlayerId(1), "Giant", 6, 6);
        let unblocked = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        setup_combat(
            &mut state,
            vec![dt, unblocked],
            vec![(dt, vec![blocker]), (unblocked, vec![])],
        );

        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 702.2c: 1 deathtouch damage is lethal — the 6/6 blocker dies to SBA.
        assert!(
            state
                .objects
                .get(&blocker)
                .is_none_or(|o| o.zone != Zone::Battlefield),
            "deathtouch creature destroys the blocker via SBA"
        );
        // The prevented attacker still made 3 Inklings.
        assert_eq!(state.players[1].life, 20);
        assert_eq!(count_inklings(&state), 3);
    }

    /// Step 10: Lifelink (CR 615.6 / CR 702.15b). A fully prevented lifelink
    /// attacker gains 0 life; a non-prevented lifelink attacker in the same
    /// batch still gains life.
    #[test]
    fn test_lifelink_prevented_gains_no_life_unprevented_still_gains() {
        // Case A: lifelink attacker fully prevented → 0 life gained.
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        let ll = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&ll)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        setup_combat(&mut state, vec![ll], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.players[0].life, 20,
            "CR 615.6: prevented damage never happens — no lifelink"
        );

        // Case B: lifelink attacker NOT prevented, batched alongside a
        // prevented attacker → lifelink still fires.
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));
        let prevented = create_creature(&mut state, PlayerId(0), "Ogre", 3, 3);
        let pw_target = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        let ll = create_creature(&mut state, PlayerId(0), "Vampire", 3, 3);
        state
            .objects
            .get_mut(&ll)
            .unwrap()
            .keywords
            .push(Keyword::Lifelink);
        // lifelink attacker hits a creature (not the shielded player).
        setup_combat(
            &mut state,
            vec![prevented, ll],
            vec![(prevented, vec![]), (ll, vec![pw_target])],
        );
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        assert_eq!(
            state.players[0].life, 23,
            "lifelink attacker's 3 to-creature damage gains 3 life"
        );
        assert_eq!(state.players[1].life, 20, "the other attacker is prevented");
    }

    /// Step 11: Commander damage bookkeeping. A commander attacker prevented +
    /// a commander attacker dealing damage in one batch — `commander_damage`
    /// accrues only the unprevented commander (CR 704.6c).
    #[test]
    fn test_commander_damage_accrues_only_unprevented_commander() {
        let mut state = setup();
        install_inkshield(&mut state, PlayerId(1));

        let prevented_cmdr = create_creature(&mut state, PlayerId(0), "Cmdr A", 4, 4);
        state.objects.get_mut(&prevented_cmdr).unwrap().is_commander = true;
        let dealing_cmdr = create_creature(&mut state, PlayerId(0), "Cmdr B", 5, 5);
        state.objects.get_mut(&dealing_cmdr).unwrap().is_commander = true;

        // Both attack PlayerId(1). The shield prevents ALL combat damage to
        // PlayerId(1), so commander damage must accrue 0 for both.
        setup_combat(&mut state, vec![prevented_cmdr, dealing_cmdr], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);

        // CR 704.6c: prevented commander damage does not accrue.
        let total: u32 = state
            .commander_damage
            .iter()
            .filter(|e| e.player == PlayerId(1))
            .map(|e| e.damage)
            .sum();
        assert_eq!(total, 0, "fully prevented — no commander damage accrues");

        // Now an unblocked commander with the shield gone: damage accrues.
        let mut state = setup();
        let cmdr = create_creature(&mut state, PlayerId(0), "Cmdr", 5, 5);
        state.objects.get_mut(&cmdr).unwrap().is_commander = true;
        setup_combat(&mut state, vec![cmdr], vec![]);
        let mut events = Vec::new();
        resolve_combat_damage(&mut state, &mut events);
        let entry = state
            .commander_damage
            .iter()
            .find(|e| e.player == PlayerId(1) && e.commander == cmdr);
        assert_eq!(
            entry.map(|e| e.damage),
            Some(5),
            "unprevented commander accrues 5 combat damage"
        );
    }

    // === CR 702.19b: single-blocker trample interactive-assignment truth table ===
    //
    // Build a REAL combat state with the blocker present in state.objects so
    // lethal_damage_needed reads its real toughness, then exercise
    // needs_interactive_assignment directly.
    fn needs_interactive(
        power: i32,
        blocker_toughness: i32,
        trample: Option<TrampleKind>,
        deathtouch: bool,
        extra_blocker_toughness: Option<i32>,
    ) -> bool {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Attacker", power, power);
        if let Some(kind) = trample {
            let kw = match kind {
                TrampleKind::Standard => Keyword::Trample,
                TrampleKind::OverPlaneswalkers => Keyword::TrampleOverPlaneswalkers,
            };
            state.objects.get_mut(&attacker).unwrap().keywords.push(kw);
        }
        if deathtouch {
            state
                .objects
                .get_mut(&attacker)
                .unwrap()
                .keywords
                .push(Keyword::Deathtouch);
        }
        let blocker = create_creature(&mut state, PlayerId(1), "Blocker", 1, blocker_toughness);
        let mut blockers = vec![blocker];
        if let Some(t) = extra_blocker_toughness {
            let blocker2 = create_creature(&mut state, PlayerId(1), "Blocker2", 1, t);
            blockers.push(blocker2);
        }
        setup_combat(&mut state, vec![attacker], vec![(attacker, blockers)]);

        let combat = state.combat.as_ref().unwrap().clone();
        let attacker_info = combat
            .attackers
            .iter()
            .find(|a| a.object_id == attacker)
            .unwrap()
            .clone();
        let obj = state.objects.get(&attacker).unwrap().clone();
        needs_interactive_assignment(
            &state,
            &obj,
            &combat,
            &attacker_info,
            power.max(0) as u32,
            deathtouch,
            trample,
        )
    }

    #[test]
    fn single_blocker_trample_with_excess_needs_interactive() {
        // CR 702.19b: 5/5 trample vs 2/2 → 3 excess → controller chooses.
        assert!(needs_interactive(
            5,
            2,
            Some(TrampleKind::Standard),
            false,
            None
        ));
    }

    #[test]
    fn single_blocker_trample_no_excess_auto_assigns() {
        // CR 510.1c: 2/2 trample vs 2/2 → power == lethal → no choice.
        assert!(!needs_interactive(
            2,
            2,
            Some(TrampleKind::Standard),
            false,
            None
        ));
    }

    #[test]
    fn single_blocker_no_trample_auto_assigns() {
        // CR 510.1c: single blocker without trample → all damage to blocker.
        assert!(!needs_interactive(5, 2, None, false, None));
    }

    #[test]
    fn two_blockers_need_interactive() {
        // CR 510.1c: 2+ blockers → controller divides.
        assert!(needs_interactive(5, 2, None, false, Some(2)));
    }

    #[test]
    fn single_blocker_trample_deathtouch_with_excess_needs_interactive() {
        // CR 702.2c + CR 702.19b: deathtouch makes lethal 1; power 5 > 1 → excess.
        assert!(needs_interactive(
            5,
            2,
            Some(TrampleKind::Standard),
            true,
            None
        ));
    }

    #[test]
    fn single_blocker_trample_deathtouch_no_excess_auto_assigns() {
        // CR 702.2c: deathtouch lethal is 1; power 1 == lethal → no choice.
        assert!(!needs_interactive(
            1,
            2,
            Some(TrampleKind::Standard),
            true,
            None
        ));
    }

    // === CR 702.19b: single-blocker trample runtime, driven through the pipeline ===

    #[test]
    fn single_blocker_trample_excess_prompts_assignment() {
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
        let waiting = resolve_combat_damage(&mut state, &mut events);

        // CR 702.19b: single-blocker trample with excess → interactive prompt.
        match waiting {
            Some(WaitingFor::AssignCombatDamage {
                ref blockers,
                trample,
                total_damage,
                ..
            }) => {
                assert_eq!(blockers.len(), 1);
                assert_eq!(blockers[0].lethal_minimum, 2);
                assert_eq!(trample, Some(TrampleKind::Standard));
                assert_eq!(total_damage, 5);
            }
            other => panic!("Expected AssignCombatDamage, got {other:?}"),
        }
    }

    #[test]
    fn single_blocker_trample_outcome_a_trample_through() {
        // Outcome A: keep lethal on blocker, trample excess to player.
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
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess must prompt");
        // A bare return from resolve_combat_damage does not write state.waiting_for;
        // the apply path requires it, so set it from the returned WaitingFor.
        state.waiting_for = waiting;

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 2)],
                trample_damage: 3,
                controller_damage: 0,
            },
        )
        .expect("trample-through assignment is legal");

        // CR 702.19b: 2 lethal to blocker (it dies via SBA → off battlefield, so
        // damage_marked is cleared on the zone change), 3 excess tramples to the
        // defending player. Player life is the discriminator vs Outcome B.
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker took lethal (2) and should have died"
        );
        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn trample_multi_blocker_power_below_combined_lethal_no_excess_is_legal() {
        // CR 702.19b + CR 510.1c: a 4/4 trample attacker blocked by two 3/3s has
        // combined lethal 6 > power 4, so the controller cannot assign lethal to
        // both AND send excess to the player.  The controller MUST be allowed to
        // freely distribute all 4 damage among the two blockers with trample_damage=0.
        // (The frontend #1491 bug: trampleLethalMet unconditionally required lethal
        // to every blocker, deadlocking combat here.)
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Rampager", 4, 4);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .keywords
            .push(Keyword::Trample);
        let blocker_a = create_creature(&mut state, PlayerId(1), "Guard A", 3, 3);
        let blocker_b = create_creature(&mut state, PlayerId(1), "Guard B", 3, 3);
        setup_combat(
            &mut state,
            vec![attacker],
            vec![(attacker, vec![blocker_a, blocker_b])],
        );

        let mut events = Vec::new();
        let waiting =
            resolve_combat_damage(&mut state, &mut events).expect("2 blockers must always prompt");
        state.waiting_for = waiting;

        // Assign 2 to each blocker, 0 trample through; this must succeed even though
        // neither blocker gets lethal (CR 702.19b: lethal gating only applies when
        // excess is being sent to the player/PW).
        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker_a, 2), (blocker_b, 2)],
                trample_damage: 0,
                controller_damage: 0,
            },
        )
        .expect("distributing all damage among blockers with trample_damage=0 is legal");

        // Neither blocker takes lethal; both survive. Defending player untouched.
        assert!(
            state.battlefield.contains(&blocker_a),
            "blocker_a took 2 < lethal (3) and must survive"
        );
        assert!(
            state.battlefield.contains(&blocker_b),
            "blocker_b took 2 < lethal (3) and must survive"
        );
        assert_eq!(state.players[1].life, 20, "no trample damage to player");
    }

    #[test]
    fn single_blocker_trample_outcome_b_keep_on_blocker() {
        // Outcome B (the user's case): controller keeps ALL 5 on the blocker and
        // tramples nothing through. CR 702.19b: assigning no excess to the player
        // is legal (need not assign lethal to blocker before keeping excess there).
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
        let waiting = resolve_combat_damage(&mut state, &mut events)
            .expect("single-blocker trample with excess must prompt");
        state.waiting_for = waiting;

        crate::game::engine::apply_as_current(
            &mut state,
            crate::types::actions::GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(blocker, 5)],
                trample_damage: 0,
                controller_damage: 0,
            },
        )
        .expect("keeping all damage on the blocker is legal (CR 702.19b)");

        // CR 702.19b: all 5 stay on the blocker (it dies via SBA → off battlefield),
        // and NONE tramples through — the defending player is untouched. The
        // unchanged player life (20) is the proof the controller may decline to
        // assign excess to the player (the user's reported case).
        assert!(
            !state.battlefield.contains(&blocker),
            "blocker took 5 (>= lethal) and should have died"
        );
        assert_eq!(state.players[1].life, 20);
    }
}
