use crate::game::combat::{AttackTarget, DamageAssignment, DamageTarget, TrampleKind};
use crate::types::ability::{CostPaidObjectSnapshot, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CombatDamageAssignmentMode, CombatTaxContext, CombatTaxPending, DamageSlot, GameState,
    WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::{begin_pending_trigger_target_selection, EngineError};
use super::priority;
use super::triggers;
use super::turns;

pub(super) fn handle_declare_attackers(
    state: &mut GameState,
    player: PlayerId,
    attacks: &[(ObjectId, AttackTarget)],
    bands: &[Vec<ObjectId>],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if state.active_player != player {
        return Err(EngineError::WrongPlayer);
    }
    // CR 508.1d + CR 508.1h: Enumerate UnlessPay static abilities (Ghostly Prison,
    // Propaganda, Sphere of Safety, etc.) before tapping attackers. If any apply,
    // pause the declaration so the active player can pay or decline the locked-in
    // aggregate cost. The actual `declare_attackers` call (which taps creatures
    // per CR 508.1f and populates CombatState) is deferred until the payment is
    // accepted or declined.
    if let Some((total_cost, per_creature)) = super::combat::compute_attack_tax(state, attacks) {
        return Ok(WaitingFor::CombatTaxPayment {
            player,
            context: CombatTaxContext::Attacking,
            total_cost,
            per_creature,
            pending: CombatTaxPending::Attack {
                attacks: attacks.to_vec(),
                // CR 702.22c + CR 702.22h: preserve band declarations across the
                // tax-payment pause so the resume path re-runs
                // `declare_attackers_with_bands` and the band is grouped for
                // blocking rather than being silently dropped.
                bands: bands.to_vec(),
            },
        });
    }
    let declaration_start = events.len();
    // CR 702.22c: declare attackers together with any banding declarations so
    // `band_id` is stamped on the attacking-band members before block
    // propagation (CR 702.22h) and damage assignment (CR 702.22j/k).
    super::combat::declare_attackers_with_bands(state, attacks, bands, events)
        .map_err(EngineError::InvalidAction)?;

    // CR 508.1g + CR 701.43d: before attack triggers are put on the stack, the
    // active player pays any optional "exert this creature as it attacks" costs.
    // Offer each eligible attacker one at a time; the post-declaration
    // trigger/priority logic resumes via `finish_declare_attackers` once the
    // exert queue is drained.
    let candidates = exert_candidates(state, attacks);
    if let Some((first, rest)) = candidates.split_first() {
        // CR 508.2: defer the declaration triggers until after the exert
        // sub-step so attack and exert "when you do" triggers are placed on the
        // stack simultaneously.
        state.pending_attack_trigger_events = events[declaration_start..].to_vec();
        return Ok(WaitingFor::ExertChoice {
            player,
            attacker: *first,
            remaining: rest.to_vec(),
        });
    }

    if let Some(waiting_for) = next_enlist_choice(state, player, enlist_candidates(state, attacks))
    {
        // CR 508.2: defer declaration triggers until after Enlist optional
        // attack costs, so attack and linked "when you do" triggers are placed
        // on the stack before priority.
        state.pending_attack_trigger_events = events[declaration_start..].to_vec();
        return Ok(waiting_for);
    }

    finish_declare_attackers(state, events, attacks.is_empty())
}

/// CR 701.43d: Attackers carrying an "exert as it attacks" ability (a
/// `TriggerMode::Exerted` triggered ability) that have not yet been exerted this
/// turn, in declaration order. These attackers are offered the optional exert
/// cost per CR 508.1g.
fn exert_candidates(state: &GameState, attacks: &[(ObjectId, AttackTarget)]) -> Vec<ObjectId> {
    attacks
        .iter()
        .map(|(attacker_id, _)| *attacker_id)
        .filter(|attacker_id| {
            !state.exerted_this_turn.contains(attacker_id)
                && state.objects.get(attacker_id).is_some_and(|obj| {
                    super::functioning_abilities::active_trigger_definitions(state, obj)
                        .any(|(_, def)| def.mode == crate::types::triggers::TriggerMode::Exerted)
                })
        })
        .collect()
}

/// CR 702.154b + CR 702.154d: each Enlist instance represents an independent
/// optional attack cost linked to its own "when you do" trigger. Return one
/// queue entry per active `TriggerMode::Enlisted` definition, preserving attack
/// declaration order and per-instance multiplicity.
fn enlist_candidates(state: &GameState, attacks: &[(ObjectId, AttackTarget)]) -> Vec<ObjectId> {
    attacks
        .iter()
        .flat_map(|(attacker_id, _)| {
            let count = state.objects.get(attacker_id).map_or(0, |obj| {
                super::functioning_abilities::active_trigger_definitions(state, obj)
                    .filter(|(_, def)| def.mode == crate::types::triggers::TriggerMode::Enlisted)
                    .count()
            });
            (0..count).map(|_| *attacker_id)
        })
        .collect()
}

fn current_enlist_candidates(state: &GameState) -> Vec<ObjectId> {
    let Some(combat) = state.combat.as_ref() else {
        return Vec::new();
    };
    let attacks: Vec<_> = combat
        .attackers
        .iter()
        .map(|attacker| (attacker.object_id, attacker.attack_target))
        .collect();
    enlist_candidates(state, &attacks)
}

pub(super) fn next_enlist_choice(
    state: &GameState,
    player: PlayerId,
    candidates: Vec<ObjectId>,
) -> Option<WaitingFor> {
    let mut remaining = candidates;
    while !remaining.is_empty() {
        let attacker = remaining.remove(0);
        let eligible = enlist_eligible_targets(state, attacker);
        if !eligible.is_empty() {
            return Some(WaitingFor::EnlistChoice {
                player,
                attacker,
                eligible,
                remaining,
            });
        }
    }
    None
}

pub(super) fn next_current_enlist_choice(
    state: &GameState,
    player: PlayerId,
) -> Option<WaitingFor> {
    next_enlist_choice(state, player, current_enlist_candidates(state))
}

fn enlist_eligible_targets(state: &GameState, attacker: ObjectId) -> Vec<ObjectId> {
    let Some(attacker_obj) = state.objects.get(&attacker) else {
        return Vec::new();
    };
    super::targeting::find_legal_targets(
        state,
        &crate::database::synthesis::enlist_tap_target_filter(),
        attacker_obj.controller,
        attacker,
    )
    .into_iter()
    .filter_map(|target| match target {
        TargetRef::Object(id) => Some(id),
        TargetRef::Player(_) => None,
    })
    .collect()
}

/// CR 701.43a + CR 701.43c: Pay the optional exert cost for an attacking
/// creature — record it as exerted this turn, add the "doesn't untap during your
/// next untap step" effect (mirroring the `AbilityCost::Exert` cost path), and
/// emit `CreatureExerted` so the linked "when you do" trigger (CR 701.43d)
/// fires. No-op if the creature has left the battlefield since attackers were
/// declared.
pub(super) fn apply_attack_exert(
    state: &mut GameState,
    attacker: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let Some(obj) = state.objects.get(&attacker) else {
        return;
    };
    if obj.zone != Zone::Battlefield {
        return;
    }
    let controller = obj.controller;
    state.exerted_this_turn.insert(attacker);
    state.add_transient_continuous_effect(
        attacker,
        controller,
        crate::types::ability::Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: crate::types::ability::PlayerScope::Controller,
        },
        crate::types::ability::TargetFilter::SpecificObject { id: attacker },
        vec![
            crate::types::ability::ContinuousModification::AddStaticMode {
                mode: crate::types::statics::StaticMode::CantUntap,
            },
        ],
        None,
    );
    let exerted = GameEvent::CreatureExerted {
        object_id: attacker,
    };
    // Buffer for deferred trigger matching (CR 508.2) and surface to the
    // per-action event stream for the frontend.
    state.pending_attack_trigger_events.push(exerted.clone());
    events.push(exerted);
}

/// CR 702.154a-c: Pay one Enlist optional attack cost by tapping an eligible
/// creature. The normal tap event and the linked Enlist event are both buffered
/// for CR 508.2 trigger processing after all attack costs are chosen.
pub(super) fn apply_attack_enlist(
    state: &mut GameState,
    attacker: ObjectId,
    tapped: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if !enlist_eligible_targets(state, attacker).contains(&tapped) {
        return Err(EngineError::InvalidAction(format!(
            "{tapped:?} is not eligible to be enlisted"
        )));
    }

    let Some(obj) = state.objects.get(&tapped) else {
        return Ok(());
    };
    let snapshot = CostPaidObjectSnapshot {
        object_id: tapped,
        lki: obj.snapshot_public_characteristics(),
    };
    let Some(obj) = state.objects.get_mut(&tapped) else {
        return Ok(());
    };
    obj.tapped = true;

    let tap_event = GameEvent::PermanentTapped {
        object_id: tapped,
        caused_by: None,
    };
    let enlisted = GameEvent::CreatureEnlisted {
        attacker,
        tapped,
        tapped_snapshot: Box::new(snapshot),
    };
    state.pending_attack_trigger_events.push(tap_event.clone());
    state.pending_attack_trigger_events.push(enlisted.clone());
    events.push(tap_event);
    events.push(enlisted);
    Ok(())
}

/// Post-declaration tail of `handle_declare_attackers`, shared with the exert
/// prompt resumption: process attack/exert triggers, then route to trigger
/// ordering, pending trigger-target selection, the no-attackers end-of-combat
/// path, or priority.
pub(super) fn finish_declare_attackers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    attacks_empty: bool,
) -> Result<WaitingFor, EngineError> {
    // CR 508.2: process the buffered declaration events together with any
    // `CreatureExerted` events from the exert sub-step. In the common (no-exert)
    // path the buffer is empty and the per-action `events` slice carries the
    // declaration events.
    let deferred = std::mem::take(&mut state.pending_attack_trigger_events);
    if deferred.is_empty() {
        triggers::process_triggers(state, events);
    } else {
        triggers::process_triggers(state, &deferred);
    }
    // CR 603.3b (#531): if process_triggers paused on OrderTriggers (the active
    // player has 2+ simultaneous triggers awaiting their ordering choice),
    // surface that prompt instead of overwriting it with Priority.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(state.waiting_for.clone());
    }
    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        return Ok(waiting_for);
    }

    if attacks_empty {
        state.phase = Phase::EndCombat;
        events.push(GameEvent::PhaseChanged {
            phase: Phase::EndCombat,
        });
        state.combat = None;
        super::layers::prune_end_of_combat_effects(state);
        turns::advance_phase(state, events);
        Ok(turns::auto_advance(state, events))
    } else {
        priority::reset_priority(state);
        Ok(WaitingFor::Priority {
            player: state.active_player,
        })
    }
}

pub(super) fn handle_declare_blockers(
    state: &mut GameState,
    player: PlayerId,
    assignments: &[(ObjectId, ObjectId)],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    for (blocker_id, _) in assignments {
        let blocker = state.objects.get(blocker_id).ok_or_else(|| {
            EngineError::InvalidAction(format!("Blocker {:?} not found", blocker_id))
        })?;
        if blocker.controller != player {
            return Err(EngineError::WrongPlayer);
        }
    }

    // CR 509.1c + CR 509.1d: Enumerate UnlessPay block-tax static abilities before
    // finalizing the blocker declaration. Defending player pays or declines the
    // locked-in total; on decline, taxed blockers are dropped from the assignment
    // list (CR 509.1c: "that player is not required to pay that cost").
    if let Some((total_cost, per_creature)) = super::combat::compute_block_tax(state, assignments) {
        return Ok(WaitingFor::CombatTaxPayment {
            player,
            context: CombatTaxContext::Blocking,
            total_cost,
            per_creature,
            pending: CombatTaxPending::Block {
                assignments: assignments.to_vec(),
            },
        });
    }
    super::combat::declare_blockers_for_player(state, player, assignments, events)
        .map_err(EngineError::InvalidAction)?;

    next_blocker_or_finish_declaration(state, events)
}

/// CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Resume a combat declaration after
/// the combat-tax choice is made.
///
/// - `accept = true`: deduct the locked-in total via the shared mana-payment pipeline,
///   then run the pending declaration with every creature intact (CR 508.1i–k:
///   mana-abilities chance → pay costs → become attacking).
/// - `accept = false`: drop the taxed creatures from the declaration and submit the
///   remaining untaxed subset. If no creatures remain on the attack side, the engine
///   ends combat via `handle_empty_attackers` (CR 508.8); on the block side, submit
///   the filtered assignments.
pub(super) fn handle_pay_combat_tax(
    state: &mut GameState,
    waiting_for: WaitingFor,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::CombatTaxPayment {
        player,
        context,
        total_cost,
        per_creature,
        pending,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for combat tax payment".to_string(),
        ));
    };

    if accept {
        // CR 508.1i–j / CR 509.1e–f: pay the locked-in total through the shared
        // unless-cost mana path. Failures bubble up to the caller.
        super::casting::pay_unless_cost(state, player, &total_cost, events)?;
        events.push(GameEvent::CombatTaxPaid {
            player,
            total_mana_value: total_cost.mana_value(),
        });
        match pending {
            CombatTaxPending::Attack { attacks, bands } => {
                return resume_declare_attackers(state, &attacks, &bands, events);
            }
            CombatTaxPending::Block { assignments } => {
                return resume_declare_blockers(state, player, &assignments, events);
            }
        }
    }

    // Decline — filter the taxed creatures out of the pending declaration.
    let taxed: std::collections::HashSet<ObjectId> =
        per_creature.iter().map(|(id, _)| *id).collect();
    match pending {
        CombatTaxPending::Attack { attacks, bands } => {
            let filtered: Vec<(ObjectId, AttackTarget)> = attacks
                .into_iter()
                .filter(|(id, _)| !taxed.contains(id))
                .collect();
            // CR 702.22f: a creature dropped from the attack (because its tax was
            // declined) is also removed from its band. Filter the taxed members
            // out of every band. CR 702.22c: a band must still contain at least
            // one creature with banding to remain a band — drop any band that no
            // longer does (this also discards bands left empty). Survivors of a
            // dissolved band stay in `filtered` as ungrouped individual attackers.
            let filtered_bands: Vec<Vec<ObjectId>> = bands
                .into_iter()
                .map(|band| {
                    band.into_iter()
                        .filter(|id| !taxed.contains(id))
                        .collect::<Vec<_>>()
                })
                .filter(|band| {
                    band.iter()
                        .any(|&id| crate::game::combat::has_banding(state, id))
                })
                .collect();
            events.push(GameEvent::CombatTaxDeclined {
                player,
                dropped: taxed.iter().copied().collect(),
            });
            resume_declare_attackers(state, &filtered, &filtered_bands, events)
        }
        CombatTaxPending::Block { assignments } => {
            let filtered: Vec<(ObjectId, ObjectId)> = assignments
                .into_iter()
                .filter(|(blocker, _)| !taxed.contains(blocker))
                .collect();
            events.push(GameEvent::CombatTaxDeclined {
                player,
                dropped: taxed.iter().copied().collect(),
            });
            let _ = context; // suppresses unused in this branch; kept for symmetry
            resume_declare_blockers(state, player, &filtered, events)
        }
    }
}

fn resume_declare_attackers(
    state: &mut GameState,
    attacks: &[(ObjectId, AttackTarget)],
    bands: &[Vec<ObjectId>],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if attacks.is_empty() {
        // CR 508.8: No creatures declared as attackers — skip to end of combat.
        return handle_empty_attackers(state, events);
    }
    // CR 702.22c + CR 702.22h: re-run the band-aware declaration so `band_id` is
    // stamped on the attacking-band members and block propagation groups them
    // (this resume path previously dropped bands, leaving members individually
    // blockable behind a combat-tax static like Ghostly Prison).
    super::combat::declare_attackers_with_bands(state, attacks, bands, events)
        .map_err(EngineError::InvalidAction)?;

    triggers::process_triggers(state, events);
    // CR 603.3b (#531): process_triggers may have paused on OrderTriggers
    // for a player with 2+ simultaneous triggers. Propagate that prompt
    // instead of overwriting it with Priority below.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(state.waiting_for.clone());
    }
    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        return Ok(waiting_for);
    }

    priority::reset_priority(state);
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

fn resume_declare_blockers(
    state: &mut GameState,
    player: PlayerId,
    assignments: &[(ObjectId, ObjectId)],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if assignments.is_empty() {
        return handle_empty_blockers(state, player, events);
    }
    super::combat::declare_blockers_for_player(state, player, assignments, events)
        .map_err(EngineError::InvalidAction)?;

    next_blocker_or_finish_declaration(state, events)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_assign_combat_damage(
    state: &mut GameState,
    player: PlayerId,
    attacker_id: ObjectId,
    total_damage: u32,
    blockers: &[DamageSlot],
    assignment_modes: &[CombatDamageAssignmentMode],
    trample: Option<TrampleKind>,
    defending_player: PlayerId,
    attack_target: &AttackTarget,
    pw_loyalty: Option<u32>,
    pw_controller: Option<PlayerId>,
    mode: CombatDamageAssignmentMode,
    assignments: &[(ObjectId, u32)],
    trample_damage: u32,
    controller_damage: u32,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !assignment_modes.contains(&mode) {
        return Err(EngineError::InvalidAction(format!(
            "Combat damage assignment mode {:?} is not allowed for attacker {:?}",
            mode, attacker_id
        )));
    }

    if mode == CombatDamageAssignmentMode::AsThoughUnblocked {
        if !assignments.is_empty() || trample_damage > 0 || controller_damage > 0 {
            return Err(EngineError::InvalidAction(
                "As-though-unblocked assignment does not use blocker or trample splits".to_string(),
            ));
        }
        let attacker_info = state
            .combat
            .as_ref()
            .and_then(|combat| {
                combat
                    .attackers
                    .iter()
                    .find(|info| info.object_id == attacker_id)
                    .cloned()
            })
            .ok_or_else(|| {
                EngineError::InvalidAction(format!(
                    "Attacker {:?} not found in combat state",
                    attacker_id
                ))
            })?;
        let damage_assignments = super::combat_damage::assign_damage_as_though_unblocked(
            state,
            &attacker_info,
            total_damage,
            trample,
        );
        if let Some(combat) = &mut state.combat {
            combat.pending_damage.extend(
                damage_assignments
                    .into_iter()
                    .map(|assignment| (attacker_id, assignment)),
            );
            combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
        }

        if let Some(waiting_for) = super::combat_damage::resolve_combat_damage(state, events) {
            return Ok(waiting_for);
        }

        priority::reset_priority(state);
        return Ok(WaitingFor::Priority { player });
    }

    let assigned_total: u32 = assignments.iter().map(|(_, amount)| *amount).sum::<u32>()
        + trample_damage
        + controller_damage;
    let expected_total = if blockers.is_empty() && trample.is_none() {
        0
    } else {
        total_damage
    };
    if assigned_total != expected_total {
        return Err(EngineError::InvalidAction(format!(
            "Damage assignment total {} != expected {}",
            assigned_total, expected_total
        )));
    }

    let valid_blocker_ids: Vec<ObjectId> = blockers.iter().map(|slot| slot.blocker_id).collect();
    for (blocker_id, _) in assignments {
        if !valid_blocker_ids.contains(blocker_id) {
            return Err(EngineError::InvalidAction(format!(
                "{:?} is not a blocker of attacker {:?}",
                blocker_id, attacker_id
            )));
        }
    }

    if (trample_damage > 0 || controller_damage > 0) && trample.is_none() {
        return Err(EngineError::InvalidAction(
            "Cannot assign trample damage without trample".to_string(),
        ));
    }

    if controller_damage > 0 {
        let is_valid = trample == Some(TrampleKind::OverPlaneswalkers)
            && pw_controller.is_some()
            && matches!(attack_target, AttackTarget::Planeswalker(_));
        if !is_valid {
            return Err(EngineError::InvalidAction(
                "Controller damage only allowed with trample over planeswalkers attacking a planeswalker".to_string(),
            ));
        }

        let loyalty_threshold = pw_loyalty.unwrap_or(0);
        if trample_damage < loyalty_threshold {
            return Err(EngineError::InvalidAction(format!(
                "Trample over planeswalkers: must assign at least {} to PW before {} to controller",
                loyalty_threshold, controller_damage
            )));
        }
    }

    // CR 702.19b: lethal-to-all-blockers is a *precondition for assigning excess
    // to the player/planeswalker/battle* — not an unconditional constraint on a
    // trampling attacker. "The attacking creature's controller need not assign
    // lethal damage to all those blocking creatures but in that case can't assign
    // any damage to the player or planeswalker it's attacking." When no excess is
    // assigned (trample_damage == 0 && controller_damage == 0), the controller may
    // divide damage freely among blockers (CR 510.1c), so the per-blocker lethal
    // minimum does not apply. Gating on actual excess is also what keeps states
    // legal where the attacker's power is less than the summed lethal of all its
    // blockers (e.g. 11 power vs six 2-toughness blockers = 12 lethal): assigning
    // lethal to every blocker is impossible, but assigning all 11 among them with
    // no trample-through is perfectly legal — and without this gate that attacker
    // would have NO legal damage assignment, deadlocking combat.
    if trample.is_some() && (trample_damage > 0 || controller_damage > 0) {
        for slot in blockers {
            let assigned = assignments
                .iter()
                .find(|(id, _)| *id == slot.blocker_id)
                .map(|(_, amount)| *amount)
                .unwrap_or(0);
            if assigned < slot.lethal_minimum {
                return Err(EngineError::InvalidAction(format!(
                    "Trample: blocker {:?} must receive at least {} lethal damage before excess to player",
                    slot.blocker_id, slot.lethal_minimum
                )));
            }
        }
    }

    if let Some(combat) = &mut state.combat {
        for (blocker_id, amount) in assignments {
            if *amount > 0 {
                combat.pending_damage.push((
                    attacker_id,
                    DamageAssignment {
                        target: DamageTarget::Object(*blocker_id),
                        amount: *amount,
                    },
                ));
            }
        }

        if trample_damage > 0 {
            let is_over_pw = trample == Some(TrampleKind::OverPlaneswalkers);
            let excess_target = match attack_target {
                AttackTarget::Player(player_id) => Some(DamageTarget::Player(*player_id)),
                AttackTarget::Planeswalker(pw_id) => match state.objects.get(pw_id) {
                    Some(obj) if obj.zone == Zone::Battlefield => {
                        Some(DamageTarget::Object(*pw_id))
                    }
                    _ if is_over_pw => Some(DamageTarget::Player(defending_player)),
                    _ => None,
                },
                AttackTarget::Battle(battle_id) => match state.objects.get(battle_id) {
                    Some(obj) if obj.zone == Zone::Battlefield => {
                        Some(DamageTarget::Object(*battle_id))
                    }
                    _ => None,
                },
            };
            if let Some(target) = excess_target {
                combat.pending_damage.push((
                    attacker_id,
                    DamageAssignment {
                        target,
                        amount: trample_damage,
                    },
                ));
            }
        }

        if controller_damage > 0 {
            if let Some(controller) = pw_controller {
                combat.pending_damage.push((
                    attacker_id,
                    DamageAssignment {
                        target: DamageTarget::Player(controller),
                        amount: controller_damage,
                    },
                ));
            }
        }

        combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
    }

    if let Some(waiting_for) = super::combat_damage::resolve_combat_damage(state, events) {
        return Ok(waiting_for);
    }

    priority::reset_priority(state);
    Ok(WaitingFor::Priority { player })
}

/// CR 510.1d + CR 702.22k: Record the active player's division of a banded
/// blocker's combat damage among the attackers it's blocking, then re-enter the
/// combat-damage resolver.
///
/// Validation (CR 510.1e — the total assignment is checked, not individual
/// assignments): the submitted amounts must sum to the blocker's combat power,
/// and every target must be an attacker the blocker is actually blocking. There
/// is NO lethal requirement — a blocker divides its damage freely (CR 510.1d).
///
/// The server bypasses its legality-enumeration gate for this state
/// (`accepts_freeform_blocker_damage_assignment`), so this handler is the real
/// validation boundary.
pub(super) fn handle_assign_blocker_damage(
    state: &mut GameState,
    _player: PlayerId,
    blocker_id: ObjectId,
    total_damage: u32,
    attackers: &[ObjectId],
    assignments: &[(ObjectId, u32)],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 510.1e: the total damage assigned must equal the blocker's combat power.
    let assigned_total: u32 = assignments.iter().map(|(_, amount)| *amount).sum();
    if assigned_total != total_damage {
        return Err(EngineError::InvalidAction(format!(
            "Blocker {:?} damage assignment total {} != combat power {}",
            blocker_id, assigned_total, total_damage
        )));
    }

    // CR 510.1d: every target must be an attacker this blocker is blocking.
    for (attacker_id, _) in assignments {
        if !attackers.contains(attacker_id) {
            return Err(EngineError::InvalidAction(format!(
                "{:?} is not an attacker blocked by {:?}",
                attacker_id, blocker_id
            )));
        }
    }

    if let Some(combat) = &mut state.combat {
        // Record into the per-sub-step resume-skip key so the re-entered blocker
        // loop in `collect_damage_assignments` skips this blocker. A non-empty
        // entry is what the skip check keys on; mirror the auto-split bookkeeping.
        let mut recorded: Vec<DamageAssignment> = Vec::new();
        for (attacker_id, amount) in assignments {
            if *amount > 0 {
                let da = DamageAssignment {
                    target: DamageTarget::Object(*attacker_id),
                    amount: *amount,
                };
                combat.pending_damage.push((blocker_id, da.clone()));
                recorded.push(da);
            }
        }
        // Guard against an all-zero division (would otherwise leave the skip key
        // empty and re-prompt forever); total_damage > 0 is guaranteed by the
        // power==0 skip in `collect_damage_assignments`, and the total-equality
        // check above ensures `recorded` is non-empty here.
        combat.damage_assignments.insert(blocker_id, recorded);
    }

    if let Some(waiting_for) = super::combat_damage::resolve_combat_damage(state, events) {
        return Ok(waiting_for);
    }

    priority::reset_priority(state);
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

/// CR 508.8: If no creatures are declared as attackers, skip declare blockers and combat damage steps.
///
/// This helper is intentionally asymmetric with `handle_empty_blockers`:
/// - CR 508.8 *explicitly* skips declare blockers and combat damage when there
///   are no attackers — no priority window is owed during skipped steps.
/// - CR 509.1 (handled by `handle_empty_blockers`) says the declare blockers
///   step still runs even if no blockers are declared, and CR 117.1c requires
///   AP priority during it (required for instants and CR 702.49 Ninjutsu-family
///   activations — notably Sneak, which is restricted to this step).
///
/// Do not "harmonize" the two paths: collapsing them reintroduces the Sneak bug.
pub(super) fn handle_empty_attackers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    super::combat::declare_attackers(state, &[], events).map_err(EngineError::InvalidAction)?;

    triggers::process_triggers(state, events);
    // CR 603.3b (#531): if process_triggers paused on OrderTriggers (the
    // active player has 2+ simultaneous triggers awaiting their ordering
    // choice), surface that prompt instead of overwriting it with Priority.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(state.waiting_for.clone());
    }
    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        return Ok(waiting_for);
    }

    state.phase = Phase::EndCombat;
    events.push(GameEvent::PhaseChanged {
        phase: Phase::EndCombat,
    });
    state.combat = None;
    super::layers::prune_end_of_combat_effects(state);
    turns::advance_phase(state, events);
    Ok(turns::auto_advance(state, events))
}

pub(super) fn handle_empty_blockers(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    super::combat::declare_blockers_for_player(state, player, &[], events)
        .map_err(EngineError::InvalidAction)?;

    next_blocker_or_finish_declaration(state, events)
}

fn next_blocker_or_finish_declaration(
    state: &mut GameState,
    _events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(player) = super::combat::next_defending_player_to_declare_blockers(state) {
        let valid_block_targets = super::combat::get_valid_block_targets_for_player(state, player);
        let valid_blocker_ids: Vec<_> = valid_block_targets.keys().copied().collect();
        let block_requirements = super::combat::block_requirements_for_player(state, player);
        return Ok(WaitingFor::DeclareBlockers {
            player,
            valid_blocker_ids,
            valid_block_targets,
            block_requirements,
        });
    }

    // CR 509.2a + CR 802.4: After each defending player has declared blockers
    // in APNAP order, put blocker-declaration triggers on the stack before the
    // active player receives priority.
    let blocker_events = state
        .combat
        .as_mut()
        .map(|combat| std::mem::take(&mut combat.pending_blocker_declaration_events))
        .unwrap_or_default();
    triggers::process_triggers(state, &blocker_events);
    // CR 603.3b (#531): if process_triggers paused on OrderTriggers (the
    // active player has 2+ simultaneous triggers awaiting their ordering
    // choice), surface that prompt instead of overwriting it with Priority.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        return Ok(state.waiting_for.clone());
    }
    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        return Ok(waiting_for);
    }

    // CR 117.1c: The active player receives priority during the declare blockers step,
    // even when no blockers were declared. Required for instants and Ninjutsu-family
    // activations (CR 702.49) — notably Sneak, which is restricted to this step.
    priority::reset_priority(state);
    Ok(WaitingFor::Priority {
        player: state.active_player,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::game_state::CombatDamageAssignmentMode;
    use crate::types::identifiers::CardId;

    fn setup() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
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
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        id
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
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Planeswalker);
        // CR 306.5b: loyalty field and counter map mirror each other.
        obj.loyalty = Some(loyalty);
        obj.counters
            .insert(crate::types::counter::CounterType::Loyalty, loyalty);
        id
    }

    fn add_enlist_trigger(state: &mut GameState, attacker: ObjectId) {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ObjectScope, PtValue, QuantityExpr, QuantityRef,
            TargetFilter,
        };
        use crate::types::triggers::TriggerMode;

        let pump = AbilityDefinition::new(
            AbilityKind::Spell,
            crate::types::ability::Effect::Pump {
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Anaphoric,
                    },
                }),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::SelfRef,
            },
        );
        let trigger = crate::types::ability::TriggerDefinition::new(TriggerMode::Enlisted)
            .valid_card(TargetFilter::SelfRef)
            .execute(pump);
        state
            .objects
            .get_mut(&attacker)
            .unwrap()
            .trigger_definitions
            .push(trigger);
        crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
    }

    fn declare_single_enlist_attacker(state: &mut GameState, attacker: ObjectId) -> WaitingFor {
        let attacks = vec![(attacker, AttackTarget::Player(PlayerId(1)))];
        let mut events = Vec::new();
        handle_declare_attackers(state, PlayerId(0), &attacks, &[], &mut events)
            .expect("declare attackers")
    }

    #[test]
    fn enlist_prompts_before_priority_and_stack() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enlister", 2, 2);
        let helper = create_creature(&mut state, PlayerId(0), "Helper", 3, 3);
        add_enlist_trigger(&mut state, attacker);

        let waiting = declare_single_enlist_attacker(&mut state, attacker);

        assert!(
            matches!(
                waiting,
                WaitingFor::EnlistChoice {
                    attacker: id,
                    ref eligible,
                    ..
                } if id == attacker && eligible.contains(&helper)
            ),
            "declare attackers must pause for Enlist before priority, got {waiting:?}"
        );
        assert!(
            state.stack.is_empty(),
            "no Enlist trigger is stacked before the cost choice"
        );
        assert!(
            !state.objects[&helper].tapped,
            "the enlisted creature is not tapped until the Enlist choice is paid"
        );
    }

    #[test]
    fn decline_enlist_taps_nothing_and_stacks_no_linked_trigger() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enlister", 2, 2);
        let helper = create_creature(&mut state, PlayerId(0), "Helper", 3, 3);
        add_enlist_trigger(&mut state, attacker);
        state.waiting_for = declare_single_enlist_attacker(&mut state, attacker);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::ChooseEnlist { target: None },
        )
        .expect("decline enlist");

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(!state.objects[&helper].tapped);
        assert!(state.stack.is_empty());
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::CreatureEnlisted { .. })),
            "declining Enlist must not fire the linked trigger"
        );
    }

    #[test]
    fn paid_enlist_seeds_tapped_creature_lki_on_linked_trigger() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enlister", 2, 2);
        let helper = create_creature(&mut state, PlayerId(0), "Helper", 3, 3);
        add_enlist_trigger(&mut state, attacker);
        state.waiting_for = declare_single_enlist_attacker(&mut state, attacker);

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::ChooseEnlist {
                target: Some(helper),
            },
        )
        .expect("pay enlist");

        let ability = match &state.stack.back().expect("Enlist trigger on stack").kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => ability,
            other => panic!("expected triggered ability, got {other:?}"),
        };
        let snapshot = ability
            .effect_context_object
            .as_ref()
            .expect("Enlist trigger must carry tapped creature LKI");
        assert_eq!(snapshot.object_id, helper);
        assert_eq!(snapshot.lki.power, Some(3));
    }

    #[test]
    fn multiple_enlist_instances_offer_independent_choices() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Enlister", 2, 2);
        let first = create_creature(&mut state, PlayerId(0), "First Helper", 3, 3);
        let second = create_creature(&mut state, PlayerId(0), "Second Helper", 4, 4);
        add_enlist_trigger(&mut state, attacker);
        add_enlist_trigger(&mut state, attacker);
        state.waiting_for = declare_single_enlist_attacker(&mut state, attacker);

        let result = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            crate::types::actions::GameAction::ChooseEnlist {
                target: Some(first),
            },
        )
        .expect("pay first enlist");

        assert!(state.objects[&first].tapped);
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::EnlistChoice {
                    attacker: id,
                    ref eligible,
                    ..
                } if id == attacker && eligible.contains(&second) && !eligible.contains(&first)
            ),
            "second Enlist instance must offer a fresh eligible set, got {:?}",
            result.waiting_for
        );
    }

    #[test]
    fn as_though_unblocked_mode_applies_only_when_chosen() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 5, 5);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            blocker_assignments: std::iter::once((attacker, vec![blocker])).collect(),
            blocker_to_attacker: std::iter::once((blocker, vec![attacker])).collect(),
            ..Default::default()
        });
        if let Some(combat) = &mut state.combat {
            combat.attackers[0].blocked = true;
        }

        let mut events = Vec::new();
        let waiting = handle_assign_combat_damage(
            &mut state,
            PlayerId(0),
            attacker,
            5,
            &[DamageSlot {
                blocker_id: blocker,
                lethal_minimum: 4,
            }],
            &[
                CombatDamageAssignmentMode::Normal,
                CombatDamageAssignmentMode::AsThoughUnblocked,
            ],
            None,
            PlayerId(1),
            &AttackTarget::Player(PlayerId(1)),
            None,
            None,
            CombatDamageAssignmentMode::AsThoughUnblocked,
            &[],
            0,
            0,
            &mut events,
        )
        .unwrap();

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        assert_eq!(state.players[1].life, 15);
        assert_eq!(state.objects[&blocker].damage_marked, 0);
    }

    #[test]
    fn as_though_unblocked_mode_can_hit_planeswalker() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Thorn Elemental", 4, 4);
        let blocker = create_creature(&mut state, PlayerId(1), "Wall", 0, 4);
        let pw = create_planeswalker(&mut state, PlayerId(1), "Test Planeswalker", 6);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::new(
                attacker,
                AttackTarget::Planeswalker(pw),
                PlayerId(1),
            )],
            blocker_assignments: std::iter::once((attacker, vec![blocker])).collect(),
            blocker_to_attacker: std::iter::once((blocker, vec![attacker])).collect(),
            ..Default::default()
        });
        if let Some(combat) = &mut state.combat {
            combat.attackers[0].blocked = true;
        }

        let mut events = Vec::new();
        let waiting = handle_assign_combat_damage(
            &mut state,
            PlayerId(0),
            attacker,
            4,
            &[DamageSlot {
                blocker_id: blocker,
                lethal_minimum: 4,
            }],
            &[
                CombatDamageAssignmentMode::Normal,
                CombatDamageAssignmentMode::AsThoughUnblocked,
            ],
            None,
            PlayerId(1),
            &AttackTarget::Planeswalker(pw),
            None,
            None,
            CombatDamageAssignmentMode::AsThoughUnblocked,
            &[],
            0,
            0,
            &mut events,
        )
        .unwrap();

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        assert_eq!(state.objects[&pw].loyalty, Some(2));
        assert_eq!(state.players[1].life, 20);
        assert_eq!(state.objects[&blocker].damage_marked, 0);
    }

    /// CR 702.19b regression: a trample attacker whose power is less than the
    /// summed lethal of all its blockers can legally assign all its damage among
    /// the blockers with zero trample-through. Reproduces the reported deadlock:
    /// an 11-power Lotleth Troll blocked by six 2-toughness Soldiers (12 lethal
    /// total). Pre-fix, the validator demanded lethal to every blocker whenever
    /// the attacker merely *had* trample, rejecting every possible assignment and
    /// leaving the combat-damage step with no legal action.
    #[test]
    fn trample_attacker_below_total_lethal_can_split_among_blockers() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Lotleth Troll", 11, 1);
        let blockers: Vec<ObjectId> = (0..6)
            .map(|_| create_creature(&mut state, PlayerId(1), "Soldier", 1, 2))
            .collect();
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            blocker_assignments: std::iter::once((attacker, blockers.clone())).collect(),
            blocker_to_attacker: blockers.iter().map(|&b| (b, vec![attacker])).collect(),
            ..Default::default()
        });
        if let Some(combat) = &mut state.combat {
            combat.attackers[0].blocked = true;
        }

        let damage_slots: Vec<DamageSlot> = blockers
            .iter()
            .map(|&b| DamageSlot {
                blocker_id: b,
                lethal_minimum: 2,
            })
            .collect();
        // Lethal to five blockers (10), remainder (1) to the sixth, none to player.
        let assignments: Vec<(ObjectId, u32)> = blockers
            .iter()
            .enumerate()
            .map(|(i, &b)| (b, if i < 5 { 2 } else { 1 }))
            .collect();

        let mut events = Vec::new();
        let result = handle_assign_combat_damage(
            &mut state,
            PlayerId(0),
            attacker,
            11,
            &damage_slots,
            &[CombatDamageAssignmentMode::Normal],
            Some(TrampleKind::Standard),
            PlayerId(1),
            &AttackTarget::Player(PlayerId(1)),
            None,
            None,
            CombatDamageAssignmentMode::Normal,
            &assignments,
            0,
            0,
            &mut events,
        );

        assert!(
            result.is_ok(),
            "trample attacker below total blocker lethal must have a legal \
             all-to-blockers assignment (CR 702.19b), got: {:?}",
            result
        );
        // No excess reached the defending player (trample_damage was 0).
        assert_eq!(state.players[1].life, 20);
        // The 1-damage remainder actually resolved onto the sixth blocker —
        // proving the assignment was applied, not silently dropped. It survives
        // (1 < toughness 2) while the five lethally-damaged blockers are removed
        // by SBAs (CR 704.5g), so it's the one observable post-resolution.
        assert_eq!(state.objects[&blockers[5]].damage_marked, 1);
    }

    /// CR 702.19b guard: the lethal-to-all precondition still bites when excess
    /// *is* assigned to the player. Assigning trample-through while a blocker is
    /// under its lethal minimum must be rejected.
    #[test]
    fn trample_through_with_under_lethal_blocker_is_rejected() {
        let mut state = setup();
        let attacker = create_creature(&mut state, PlayerId(0), "Trampler", 5, 5);
        let blocker = create_creature(&mut state, PlayerId(1), "Bear", 2, 2);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, PlayerId(1))],
            blocker_assignments: std::iter::once((attacker, vec![blocker])).collect(),
            blocker_to_attacker: std::iter::once((blocker, vec![attacker])).collect(),
            ..Default::default()
        });
        if let Some(combat) = &mut state.combat {
            combat.attackers[0].blocked = true;
        }

        let mut events = Vec::new();
        let result = handle_assign_combat_damage(
            &mut state,
            PlayerId(0),
            attacker,
            5,
            &[DamageSlot {
                blocker_id: blocker,
                lethal_minimum: 2,
            }],
            &[CombatDamageAssignmentMode::Normal],
            Some(TrampleKind::Standard),
            PlayerId(1),
            &AttackTarget::Player(PlayerId(1)),
            None,
            None,
            CombatDamageAssignmentMode::Normal,
            // Only 1 to the blocker (under lethal 2), 4 tramples to player — illegal.
            &[(blocker, 1)],
            4,
            0,
            &mut events,
        );

        assert!(
            result.is_err(),
            "trample-through while a blocker is under its lethal minimum must be \
             rejected (CR 702.19b)"
        );
    }

    /// Install a Norn's-Annex-style attack-tax static on a fresh battlefield
    /// object controlled by `controller`. Mirrors the constructor used in
    /// `combat::tests::compute_attack_tax_norns_annex_phyrexian_cost` but lives
    /// here so this test module can exercise the full
    /// declare-attackers → CombatTaxPayment → handle_pay_combat_tax cycle.
    fn install_attack_tax_static(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        cost: crate::types::mana::ManaCost,
    ) -> ObjectId {
        use crate::types::ability::{
            ControllerRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
            TypedFilter, UnlessPayScaling,
        };
        use crate::types::card_type::CoreType;
        use crate::types::statics::StaticMode;

        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut def = StaticDefinition::new(StaticMode::CantAttack)
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }))
            .description(name.to_string());
        def.condition = Some(StaticCondition::UnlessPay {
            cost,
            scaling: UnlessPayScaling::PerAffectedCreature,
            // CR 506.3: Defender scope — these test fixtures simulate
            // Ghostly-Prison-class statics that defend the controller. Match
            // the Oracle text "Creatures can't attack you...".
            defended: Some(crate::types::triggers::AttackTargetFilter::Player),
        });
        obj.static_definitions.push(def);
        id
    }

    /// L9-52 regression: with Norn's Annex on the battlefield, declaring
    /// attackers must yield `WaitingFor::CombatTaxPayment` (not deadlock or
    /// panic), and accepting the tax must resolve cleanly to
    /// `WaitingFor::Priority` with the attackers in combat.
    ///
    /// CR 508.1d + CR 508.1h: combat tax pause/resume.
    /// CR 202.3g + CR 107.4f: each `{W/P}` shard auto-resolves to mana when
    /// available, otherwise 2 life.
    #[test]
    fn norns_annex_attack_tax_resolves_without_deadlock_paying_mana() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup();
        // Defender (PlayerId(1)) controls Norn's Annex.
        install_attack_tax_static(
            &mut state,
            PlayerId(1),
            "Norn's Annex",
            ManaCost::Cost {
                shards: vec![ManaCostShard::PhyrexianWhite],
                generic: 0,
            },
        );
        // Attacker has two creatures and one White mana floating.
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        let a2 = create_creature(&mut state, PlayerId(0), "A2", 2, 2);
        state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap()
            .mana_pool
            .add(ManaUnit {
                color: ManaType::White,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        let life_before = state.players[0].life;

        let attacks = vec![
            (a1, AttackTarget::Player(PlayerId(1))),
            (a2, AttackTarget::Player(PlayerId(1))),
        ];

        // Step 1: declare-attackers must pause for the tax (no deadlock).
        let mut events = Vec::new();
        let waiting = handle_declare_attackers(&mut state, PlayerId(0), &attacks, &[], &mut events)
            .expect("declare-attackers must yield WaitingFor::CombatTaxPayment, not error");
        let WaitingFor::CombatTaxPayment {
            player,
            total_cost,
            per_creature,
            ..
        } = waiting.clone()
        else {
            panic!("expected CombatTaxPayment, got {waiting:?}");
        };
        assert_eq!(player, PlayerId(0));
        assert_eq!(total_cost.mana_value(), 2);
        assert_eq!(per_creature.len(), 2);

        // Step 2: accepting the tax must complete the declaration.
        // Auto-decide consumes the one White mana for shard 0 and 2 life for shard 1.
        let mut events = Vec::new();
        let resumed = handle_pay_combat_tax(&mut state, waiting, true, &mut events)
            .expect("pay-combat-tax must resolve, not deadlock or error");
        assert!(
            matches!(resumed, WaitingFor::Priority { .. }),
            "post-payment WaitingFor must be Priority, got {resumed:?}"
        );
        // CR 107.4f: one shard paid with mana, one with 2 life.
        assert_eq!(
            state.players[0].life,
            life_before - 2,
            "second {{W/P}} shard auto-pays 2 life when mana exhausted"
        );
        // CR 508.1f: combat state must contain both attackers post-resume.
        let combat = state.combat.as_ref().expect("combat state present");
        assert_eq!(combat.attackers.len(), 2);
    }

    /// Class regression: same flow with a Ghostly-Prison-style `{2}` cost
    /// must remain unaffected by the Phyrexian-handling change.
    /// CR 508.1d + CR 508.1h: generic-mana attack tax.
    #[test]
    fn ghostly_prison_attack_tax_resolves_without_deadlock() {
        use crate::types::mana::{ManaCost, ManaType, ManaUnit};

        let mut state = setup();
        install_attack_tax_static(
            &mut state,
            PlayerId(1),
            "Ghostly Prison",
            ManaCost::generic(2),
        );
        let a1 = create_creature(&mut state, PlayerId(0), "A1", 2, 2);
        // Give attacker enough generic mana to pay {2}.
        for _ in 0..2 {
            state
                .players
                .iter_mut()
                .find(|p| p.id == PlayerId(0))
                .unwrap()
                .mana_pool
                .add(ManaUnit {
                    color: ManaType::Colorless,
                    source_id: ObjectId(0),
                    pip_id: crate::types::mana::ManaPipId(0),
                    supertype: None,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: Vec::new(),
                    grants: vec![],
                    expiry: None,
                });
        }
        let life_before = state.players[0].life;

        let attacks = vec![(a1, AttackTarget::Player(PlayerId(1)))];
        let mut events = Vec::new();
        let waiting = handle_declare_attackers(&mut state, PlayerId(0), &attacks, &[], &mut events)
            .expect("declare-attackers must yield CombatTaxPayment");
        assert!(matches!(waiting, WaitingFor::CombatTaxPayment { .. }));

        let mut events = Vec::new();
        let resumed = handle_pay_combat_tax(&mut state, waiting, true, &mut events)
            .expect("pay-combat-tax must resolve cleanly for generic-mana tax");
        assert!(matches!(resumed, WaitingFor::Priority { .. }));
        assert_eq!(
            state.players[0].life, life_before,
            "generic-mana tax must not change life total"
        );
    }
}
