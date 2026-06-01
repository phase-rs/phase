//! CR 702.26: Phasing — the status-based "treat as though it does not exist"
//! mechanic. Phased-out permanents remain in `Zone::Battlefield` (CR 702.26d:
//! phasing never causes a zone change); their `GameObject::phase_status`
//! discriminates the two states.
//!
//! Architectural invariants:
//! - Filter exclusion lives in `game/filter.rs::filter_inner` and
//!   `game/targeting.rs::zone_object_ids` (the single choke points). All other
//!   callers get phased-out exclusion transparently.
//! - Zone doesn't change — never emit `ZoneChanged` (CR 702.26d).
//! - Counters, stickers, attachments, is_commander, chosen_attributes all
//!   persist unchanged across a phase-out/phase-in cycle.
//! - Combat involvement clears on phase-out (CR 702.26b + CR 506.4).

use std::collections::HashSet;

use crate::game::effects::remove_from_combat::remove_object_from_combat;
use crate::game::game_object::{AttachTarget, PhaseOutCause, PhaseStatus};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::{PlayerId, PlayerStatus};
use crate::types::zones::Zone;

/// CR 702.26b: Phase out a permanent directly, cascading indirect phase-out
/// through Auras/Equipment/Fortifications attached to it (CR 702.26g).
///
/// Returns the set of object ids that transitioned to phased-out during this
/// call (empty if the target was already phased out or didn't exist). Useful
/// for `Effect::PhaseOut` resolvers that emit individual events and for
/// aggregate callers (Teferi's Protection, Teferi's Realm) that phase many
/// permanents at once.
pub fn phase_out_object(
    state: &mut GameState,
    object_id: ObjectId,
    cause: PhaseOutCause,
    events: &mut Vec<GameEvent>,
) -> Vec<ObjectId> {
    let mut phased: Vec<ObjectId> = Vec::new();
    let mut queue: Vec<(ObjectId, PhaseOutCause)> = vec![(object_id, cause)];
    let mut seen: HashSet<ObjectId> = HashSet::new();

    while let Some((id, this_cause)) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        // CR 702.26h: if an object would phase out directly and indirectly at
        // the same time, it phases out indirectly — never promote indirect to
        // direct when a later pass reaches the same object.
        if obj.is_phased_out() {
            if matches!(this_cause, PhaseOutCause::Indirectly)
                && matches!(
                    obj.phase_status,
                    PhaseStatus::PhasedOut {
                        cause: PhaseOutCause::Directly
                    }
                )
            {
                obj.phase_status = PhaseStatus::PhasedOut {
                    cause: PhaseOutCause::Indirectly,
                };
            }
            continue;
        }

        obj.phase_status = PhaseStatus::PhasedOut { cause: this_cause };
        phased.push(id);

        // CR 702.26g: cascade to attached Auras/Equipment/Fortifications.
        // One level deep only — the spec wording "any Auras, Equipment, or
        // Fortifications attached to that permanent" refers to direct
        // attachments; attachments-of-attachments remain in their attached
        // state (the host they were attached to didn't phase out — we did).
        let attachments = state
            .objects
            .get(&id)
            .map(|o| o.attachments.clone())
            .unwrap_or_default();
        for att_id in attachments {
            if let Some(att) = state.objects.get(&att_id) {
                if is_attachment_cascaded_by_phasing(&att.card_types.core_types) {
                    queue.push((att_id, PhaseOutCause::Indirectly));
                }
            }
        }
    }

    // CR 506.4 + CR 702.26b: Removal from combat happens once all cascades
    // settle, so concurrent attacker/blocker updates apply to the full set.
    for &id in &phased {
        remove_object_from_combat(state, id);
    }

    // Emit events after all mutation, so observers see a consistent state.
    for &id in &phased {
        let indirect = matches!(
            state
                .objects
                .get(&id)
                .map(|o| o.phase_status)
                .unwrap_or_default(),
            PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Indirectly
            }
        );
        events.push(GameEvent::PermanentPhasedOut {
            object_id: id,
            indirect,
        });
    }

    phased
}

/// CR 702.26c / CR 702.26g: Phase in a permanent. Directly-phased-out
/// permanents phase in on their own; indirectly-phased-out ones phase in
/// alongside the host they were attached to (follow the attachment chain).
///
/// Returns the set of object ids that transitioned to phased-in.
pub fn phase_in_object(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Vec<ObjectId> {
    let mut phased: Vec<ObjectId> = Vec::new();
    let mut queue: Vec<ObjectId> = vec![object_id];
    let mut seen: HashSet<ObjectId> = HashSet::new();

    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(obj) = state.objects.get_mut(&id) else {
            continue;
        };
        if obj.is_phased_in() {
            continue;
        }
        obj.phase_status = PhaseStatus::PhasedIn;
        phased.push(id);

        // CR 702.26g: phase in any permanents that phased out indirectly
        // because this one phased out (they were attached to it). They ride
        // along with the host.
        let attachments = state
            .objects
            .get(&id)
            .map(|o| o.attachments.clone())
            .unwrap_or_default();
        for att_id in attachments {
            if let Some(att) = state.objects.get(&att_id) {
                if matches!(
                    att.phase_status,
                    PhaseStatus::PhasedOut {
                        cause: PhaseOutCause::Indirectly
                    }
                ) {
                    queue.push(att_id);
                }
            }
        }
    }

    for &id in &phased {
        // CR 702.103g: A bestow Aura that phases in unattached (its host left
        // the battlefield while it was phased out) ceases to be bestowed and
        // becomes a creature. The attached_to pointer persists across phase-out
        // (CR 702.26d: no zone change), so we validate whether the attachment
        // target is still on the battlefield rather than checking is_none().
        let is_bestow_unattached = state.objects.get(&id).is_some_and(|o| {
            o.bestow_form.is_some()
                && match o.attached_to {
                    Some(AttachTarget::Object(t)) => !state
                        .objects
                        .get(&t)
                        .is_some_and(|h| h.zone == Zone::Battlefield),
                    Some(AttachTarget::Player(pid)) => !state
                        .players
                        .get(pid.0 as usize)
                        .is_some_and(|p| !p.is_eliminated),
                    None => true,
                }
        });
        if is_bestow_unattached {
            super::casting::revert_bestow_form(state, id);
        }

        events.push(GameEvent::PermanentPhasedIn { object_id: id });
    }

    phased
}

/// CR 702.26g: Only Auras, Equipment, and Fortifications cascade when the
/// host phases out.
fn is_attachment_cascaded_by_phasing(core_types: &[CoreType]) -> bool {
    core_types.iter().any(|t| {
        matches!(
            t,
            CoreType::Enchantment | CoreType::Artifact // Auras are Enchantments; Equipment and Fortifications are Artifacts.
        )
    })
}

/// CR 502.1 + CR 702.26a: Perform the untap-step phasing turn-based action
/// for the active player. All phased-in permanents the player controls that
/// have phasing phase out; simultaneously all phased-out permanents that had
/// phased out under that player's control phase in.
///
/// CR 702.26m: If the untap step itself is skipped, phasing is also skipped.
/// This TBA must be called only when the untap step is actually happening.
pub fn execute_untap_step_phasing(state: &mut GameState, events: &mut Vec<GameEvent>) {
    use crate::types::keywords::Keyword;
    let active = state.active_player;

    // Collect BEFORE mutating: snapshot all target ids so simultaneous
    // phase-in + phase-out semantics hold (CR 702.26a "simultaneously").
    //
    // CR 702.26a: "all phased-in permanents with phasing that player controls
    // phase out" — look at the controller's on-battlefield phased-in objects
    // with the Phasing keyword.
    //
    // CR 702.26a: "all phased-out permanents that had phased out under that
    // player's control phase in". We identify these by scanning all objects
    // whose last-known controller is `active` and whose phase_status is
    // `PhasedOut` directly (indirect ones ride along with their host).
    let phase_out_ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_phased_in() && obj.controller == active && obj.has_keyword(&Keyword::Phasing)
            })
        })
        .collect();

    let phase_in_ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                matches!(
                    obj.phase_status,
                    PhaseStatus::PhasedOut {
                        cause: PhaseOutCause::Directly
                    }
                ) && obj.controller == active
            })
        })
        .collect();

    // CR 702.26a: simultaneity. We perform both in one pass so that e.g. a
    // creature with phasing doesn't immediately phase back in, and a
    // phased-out permanent doesn't phase out again this same step.
    for id in phase_in_ids {
        phase_in_object(state, id, events);
    }
    for id in phase_out_ids {
        phase_out_object(state, id, PhaseOutCause::Directly, events);
    }
}

/// Phase a player out. Player phasing is not formally governed by CR 702.26
/// (which is permanent-only); semantics derive from the small set of card
/// Oracle text that says "you phase out". The status field on `Player`
/// is the sole encoding — the player remains in `state.players`.
///
/// Returns the player ids that transitioned (empty if already phased out or
/// the player is not in the game).
///
/// Per the player-phasing invariant list on `PlayerStatus`, callers do NOT
/// need to scatter exclusion checks: the four filter choke points
/// (`add_players` for targeting, `get_valid_attack_targets` for combat,
/// `apply_damage_after_replacement` for damage, and `check_player_life` for
/// SBA) handle every downstream consequence transparently.
pub fn phase_out_player(
    state: &mut GameState,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Vec<PlayerId> {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return Vec::new();
    };
    if player.is_phased_out() {
        return Vec::new();
    }
    player.status = PlayerStatus::PhasedOut;
    events.push(GameEvent::PlayerPhasedOut { player_id });
    vec![player_id]
}

/// Phase a player back in. Idempotent for already-phased-in players.
///
/// Returns the player ids that transitioned (empty if already phased in or
/// the player is not in the game).
pub fn phase_in_player(
    state: &mut GameState,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Vec<PlayerId> {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return Vec::new();
    };
    if player.is_phased_in() {
        return Vec::new();
    }
    player.status = PlayerStatus::Active;
    events.push(GameEvent::PlayerPhasedIn { player_id });
    vec![player_id]
}

/// Phase any phased-out players back in at the start of their next turn.
///
/// Player-phasing semantics: a player phased out by an `UntilYourNextTurn`
/// effect phases back in at the active player's untap step, simultaneously
/// with their controlled permanents (which are handled by
/// `execute_untap_step_phasing`). Unlike permanent phasing (CR 702.26a),
/// player phasing has no formal CR rule — this mirrors the permanent
/// behaviour so the duration semantics stay consistent.
///
/// Called from the untap step before any other turn-based actions, so that
/// downstream priority/draw/SBA logic sees the player as phased in.
pub fn execute_untap_step_player_phase_in(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let active = state.active_player;
    if state
        .players
        .iter()
        .find(|p| p.id == active)
        .is_some_and(|p| p.is_phased_out())
    {
        phase_in_player(state, active, events);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature(state: &mut GameState, name: &str, controller: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        id
    }

    fn setup_aura(
        state: &mut GameState,
        name: &str,
        controller: PlayerId,
        attached_to: ObjectId,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(2),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.subtypes = vec!["Aura".to_string()];
            obj.attached_to = Some(attached_to.into());
        }
        if let Some(host) = state.objects.get_mut(&attached_to) {
            host.attachments.push(id);
        }
        id
    }

    #[test]
    fn phase_out_transitions_status_and_emits_event() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state, "Breezekeeper", PlayerId(0));
        let mut events = Vec::new();

        let phased = phase_out_object(&mut state, id, PhaseOutCause::Directly, &mut events);

        assert_eq!(phased, vec![id]);
        assert!(state.objects[&id].is_phased_out());
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentPhasedOut {
                object_id,
                indirect: false,
            } if *object_id == id
        )));
    }

    #[test]
    fn phase_out_cascades_to_attached_aura() {
        let mut state = GameState::new_two_player(42);
        let creature = setup_creature(&mut state, "Bear", PlayerId(0));
        let aura = setup_aura(&mut state, "Boon", PlayerId(0), creature);
        let mut events = Vec::new();

        phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);

        assert!(state.objects[&creature].is_phased_out());
        assert!(state.objects[&aura].is_phased_out());
        assert!(matches!(
            state.objects[&aura].phase_status,
            PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Indirectly
            }
        ));
    }

    #[test]
    fn phase_in_cascades_to_indirectly_phased_attachments() {
        let mut state = GameState::new_two_player(42);
        let creature = setup_creature(&mut state, "Bear", PlayerId(0));
        let aura = setup_aura(&mut state, "Boon", PlayerId(0), creature);
        let mut events = Vec::new();

        phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);
        events.clear();

        phase_in_object(&mut state, creature, &mut events);

        assert!(state.objects[&creature].is_phased_in());
        assert!(state.objects[&aura].is_phased_in());
    }

    #[test]
    fn phased_out_permanent_is_excluded_from_trigger_candidates() {
        // CR 702.26b: a phased-out permanent is treated as though it doesn't
        // exist and must not be offered as a trigger candidate. The index does
        // not track phase status, so `candidates_for_event` filters it; this
        // test would fail before that filter was added.
        use crate::game::trigger_index::candidates_for_event;

        let mut state = GameState::new_two_player(42);
        let phased = setup_creature(&mut state, "Breezekeeper", PlayerId(0));
        let live = setup_creature(&mut state, "Bear", PlayerId(0));
        // Register both as catch-all trigger sources (consulted on every event).
        state.trigger_index.unclassified.push(phased);
        state.trigger_index.unclassified.push(live);

        let event = GameEvent::PermanentPhasedOut {
            object_id: live,
            indirect: false,
        };

        // Positive control: both are candidates while phased in.
        let before = candidates_for_event(&state, &event);
        assert!(before.contains(&phased));
        assert!(before.contains(&live));

        let mut events = Vec::new();
        phase_out_object(&mut state, phased, PhaseOutCause::Directly, &mut events);

        let after = candidates_for_event(&state, &event);
        assert!(
            !after.contains(&phased),
            "phased-out permanent must not be a trigger candidate (CR 702.26b)"
        );
        assert!(
            after.contains(&live),
            "phased-in permanent must remain a trigger candidate"
        );
    }

    #[test]
    fn untap_step_phasing_toggles_phasing_keyword_permanents() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let id = setup_creature(&mut state, "Breezekeeper", PlayerId(0));
        state.objects.get_mut(&id).unwrap().keywords = vec![Keyword::Phasing];
        state.objects.get_mut(&id).unwrap().base_keywords = vec![Keyword::Phasing];
        let mut events = Vec::new();

        // Turn 1: phase out on the active player's untap step.
        execute_untap_step_phasing(&mut state, &mut events);
        assert!(state.objects[&id].is_phased_out());

        events.clear();

        // Turn 2: phase in on the same player's next untap step.
        execute_untap_step_phasing(&mut state, &mut events);
        assert!(state.objects[&id].is_phased_in());
    }

    /// CR 702.26b: A phased-out creature can't be targeted — the filter
    /// choke point in `filter_inner` excludes phased-out objects.
    #[test]
    fn phased_out_creature_is_not_targetable() {
        use crate::game::targeting::find_legal_targets;
        use crate::types::ability::{TargetFilter, TargetRef, TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state, "Bear", PlayerId(0));
        let source = setup_creature(&mut state, "Source", PlayerId(1));
        let mut events = Vec::new();

        // Before phase-out: targetable.
        let filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            ..Default::default()
        });
        let legal_before = find_legal_targets(&state, &filter, PlayerId(1), source);
        assert!(legal_before.contains(&TargetRef::Object(id)));

        phase_out_object(&mut state, id, PhaseOutCause::Directly, &mut events);

        // After phase-out: not targetable.
        let legal_after = find_legal_targets(&state, &filter, PlayerId(1), source);
        assert!(!legal_after.contains(&TargetRef::Object(id)));
    }

    /// CR 702.26b + CR 506.4: A phased-out creature is removed from combat.
    #[test]
    fn phase_out_removes_from_combat() {
        use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state, "Attacker", PlayerId(0));
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
            }],
            ..Default::default()
        });
        let mut events = Vec::new();

        phase_out_object(&mut state, id, PhaseOutCause::Directly, &mut events);

        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.is_empty(),
            "Phased-out creature must leave combat (CR 506.4 + CR 702.26b)"
        );
    }

    /// CR 702.26d: Counters persist across a phase-out/phase-in cycle.
    #[test]
    fn counters_persist_through_phasing() {
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state, "Bear", PlayerId(0));
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);
        let mut events = Vec::new();

        phase_out_object(&mut state, id, PhaseOutCause::Directly, &mut events);
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(3),
            "Counters must not be removed by phase-out (CR 702.26d)"
        );

        phase_in_object(&mut state, id, &mut events);
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(3),
            "Counters must persist across the phase-in (CR 702.26d)"
        );
    }

    /// CR 702.26d: "Tokens continue to exist on the battlefield while phased
    /// out" — a phased-out token stays on the battlefield.
    #[test]
    fn phased_out_token_persists() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state, "Spirit Token", PlayerId(0));
        state.objects.get_mut(&id).unwrap().is_token = true;
        let mut events = Vec::new();

        phase_out_object(&mut state, id, PhaseOutCause::Directly, &mut events);
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert!(state.objects[&id].is_phased_out());
        assert!(state.objects[&id].is_token);
    }

    /// CR 702.26e: Continuous effects from a phased-out source don't apply.
    /// Exercised via `collect_shared_active_continuous_effects` which skips
    /// phased-out battlefield objects.
    #[test]
    fn continuous_effects_skip_phased_out_source() {
        use crate::game::layers::collect_shared_active_continuous_effects;
        use crate::types::ability::{ContinuousModification, StaticDefinition, TargetFilter};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let anthem_id = setup_creature(&mut state, "Glorious Anthem", PlayerId(0));
        // Attach an "other creatures get +1/+1" continuous static.
        let anthem_static = StaticDefinition::new(StaticMode::Continuous)
            .modifications(vec![ContinuousModification::AddPower { value: 1 }])
            .affected(TargetFilter::Any);
        state
            .objects
            .get_mut(&anthem_id)
            .unwrap()
            .static_definitions = vec![anthem_static.clone()].into();
        state
            .objects
            .get_mut(&anthem_id)
            .unwrap()
            .base_static_definitions = Arc::new(vec![anthem_static]);

        let before = collect_shared_active_continuous_effects(&state);
        assert!(
            !before.is_empty(),
            "Phased-in anthem should contribute effects"
        );

        let mut events = Vec::new();
        phase_out_object(&mut state, anthem_id, PhaseOutCause::Directly, &mut events);

        let after = collect_shared_active_continuous_effects(&state);
        assert!(
            after.is_empty(),
            "Phased-out anthem must contribute no continuous effects (CR 702.26e)"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Player-phasing tests. Player phasing is not formally governed by
    // CR 702.26 (which is permanent-only); these exercise the four filter
    // choke points (targeting / attacking / damage / SBA-life-loss) plus
    // the untap-step phase-in semantics that mirror the permanent path.
    // ─────────────────────────────────────────────────────────────────────

    /// Phasing a player out flips the typed `PlayerStatus` and emits the
    /// `PlayerPhasedOut` event; phasing in flips back and emits
    /// `PlayerPhasedIn`. The player remains in `state.players` throughout —
    /// the status is the sole encoding (mirrors the permanent invariant).
    #[test]
    fn player_phase_out_and_in_round_trip() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let phased = phase_out_player(&mut state, PlayerId(0), &mut events);
        assert_eq!(phased, vec![PlayerId(0)]);
        assert!(state.players[0].is_phased_out());
        assert_eq!(
            state.players.len(),
            2,
            "Player must remain in state.players"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPhasedOut { player_id } if *player_id == PlayerId(0)
        )));

        events.clear();
        let phased_in = phase_in_player(&mut state, PlayerId(0), &mut events);
        assert_eq!(phased_in, vec![PlayerId(0)]);
        assert!(state.players[0].is_phased_in());
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPhasedIn { player_id } if *player_id == PlayerId(0)
        )));
    }

    /// Phasing an already-phased-out player out is a no-op (no event, empty
    /// return). Same for phasing in an already-phased-in player.
    #[test]
    fn player_phase_out_idempotent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        phase_out_player(&mut state, PlayerId(0), &mut events);
        events.clear();

        let phased = phase_out_player(&mut state, PlayerId(0), &mut events);
        assert!(phased.is_empty());
        assert!(events.is_empty());

        phase_in_player(&mut state, PlayerId(0), &mut events);
        events.clear();

        let phased_in = phase_in_player(&mut state, PlayerId(0), &mut events);
        assert!(phased_in.is_empty());
        assert!(events.is_empty());
    }

    /// Targeting choke point: a phased-out player is excluded from the legal
    /// target set for `TargetFilter::Player` and `TargetFilter::Any`.
    #[test]
    fn phased_out_player_is_not_targetable() {
        use crate::game::targeting::find_legal_targets;
        use crate::types::ability::{TargetFilter, TargetRef};

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        // Before phase-out: both players are valid targets.
        let before = find_legal_targets(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert!(before.contains(&TargetRef::Player(PlayerId(0))));
        assert!(before.contains(&TargetRef::Player(PlayerId(1))));

        phase_out_player(&mut state, PlayerId(1), &mut events);

        // After phase-out: phased-out player is excluded.
        let after = find_legal_targets(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert!(after.contains(&TargetRef::Player(PlayerId(0))));
        assert!(
            !after.contains(&TargetRef::Player(PlayerId(1))),
            "Phased-out player must be excluded from legal targets"
        );

        // Same exclusion applies via the `Any` filter (which dispatches through
        // `add_players`).
        let any_after = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), ObjectId(99));
        assert!(!any_after.contains(&TargetRef::Player(PlayerId(1))));
    }

    /// Attacking choke point: a phased-out player can't be attacked, and
    /// neither can their planeswalkers nor any battles they protect.
    #[test]
    fn phased_out_player_is_not_attackable() {
        use crate::game::combat::{get_valid_attack_targets, AttackTarget};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        // Set up an opposing planeswalker controlled by player 1.
        let pw = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Opposing PW".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&pw).unwrap().card_types.core_types = vec![CoreType::Planeswalker];

        let before = get_valid_attack_targets(&state);
        assert!(before.contains(&AttackTarget::Player(PlayerId(1))));
        assert!(before.contains(&AttackTarget::Planeswalker(pw)));

        let mut events = Vec::new();
        phase_out_player(&mut state, PlayerId(1), &mut events);

        let after = get_valid_attack_targets(&state);
        assert!(
            !after.contains(&AttackTarget::Player(PlayerId(1))),
            "Phased-out player must be excluded from attack targets"
        );
        assert!(
            !after.contains(&AttackTarget::Planeswalker(pw)),
            "Planeswalkers controlled by phased-out player must be excluded too"
        );
    }

    /// Damage routing: damage routed to a phased-out player is a no-op —
    /// no life loss, no DamageDealt event for that target.
    #[test]
    fn phased_out_player_takes_no_damage() {
        use crate::game::effects::deal_damage::{apply_damage_to_target, DamageContext};
        use crate::types::ability::TargetRef;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bolt Source".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        phase_out_player(&mut state, PlayerId(1), &mut events);
        events.clear();

        let initial_life = state.players[1].life;
        let ctx = DamageContext::from_source(&state, source_id).unwrap();
        let _ = apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(1)),
            3,
            false,
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.players[1].life, initial_life,
            "Phased-out player must not take damage"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::DamageDealt {
                    target: TargetRef::Player(pid),
                    ..
                } if *pid == PlayerId(1)
            )),
            "No DamageDealt event should be emitted for a phased-out player"
        );
    }

    /// SBA: a phased-out player at 0-or-less life does NOT lose the game.
    /// The check_player_life SBA filters them out.
    #[test]
    fn phased_out_player_does_not_lose_at_zero_life() {
        use crate::game::sba::check_state_based_actions;

        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        phase_out_player(&mut state, PlayerId(1), &mut events);
        state.players[1].life = -5;
        events.clear();

        check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.players[1].is_eliminated,
            "Phased-out player must not be eliminated by 0-or-less life SBA"
        );
        assert!(
            !events.iter().any(
                |e| matches!(e, GameEvent::PlayerLost { player_id } if *player_id == PlayerId(1))
            ),
            "No PlayerLost event for phased-out player"
        );
    }

    /// Phase-in timing: at the start of the phased-out player's next turn
    /// (their untap step), they phase back in. The execute_untap pipeline
    /// invokes `execute_untap_step_player_phase_in` ahead of permanent
    /// phasing, so by the time SBAs run the player is back in the game.
    #[test]
    fn player_phases_in_at_start_of_their_next_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let mut events = Vec::new();

        phase_out_player(&mut state, PlayerId(0), &mut events);
        assert!(state.players[0].is_phased_out());
        events.clear();

        // Active player's untap step.
        execute_untap_step_player_phase_in(&mut state, &mut events);

        assert!(state.players[0].is_phased_in());
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPhasedIn { player_id } if *player_id == PlayerId(0)
        )));
    }

    /// `Effect::PhaseOut` with `TargetFilter::Controller` (the parser's
    /// lowering of "you phase out") phases out the ability's controller via
    /// the resolver's player branch.
    #[test]
    fn effect_phase_out_with_controller_target_phases_player() {
        use crate::game::effects::phase_out::resolve;
        use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Synthetic Source".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::PhaseOut {
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].is_phased_out());
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPhasedOut { player_id } if *player_id == PlayerId(0)
        )));
    }

    /// Composite Teferi's-Protection-style scenario: the same `UntilYourNextTurn`
    /// turn boundary that prunes transient continuous effects also drives the
    /// player phase-in, and the active player's untap step phases their
    /// controlled permanents back in alongside (CR 702.26a). Exercising both
    /// halves end-to-end demonstrates the mechanism the user asked for, even
    /// though no current corpus card prints "you phase out".
    #[test]
    fn teferis_protection_synthetic_composite() {
        use crate::game::game_object::PhaseOutCause;

        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let creature = setup_creature(&mut state, "Bear", PlayerId(0));
        let mut events = Vec::new();

        // Synthetically phase out the controller player AND a permanent they
        // control — this is the engine-side composite that "Until your next
        // turn, you and permanents you control phase out" would resolve to
        // (Effect::PhaseOut for a Controller player target chained with
        // Effect::PhaseOut for a `Typed { Permanent, You }` mass filter).
        phase_out_player(&mut state, PlayerId(0), &mut events);
        phase_out_object(&mut state, creature, PhaseOutCause::Directly, &mut events);

        assert!(state.players[0].is_phased_out());
        assert!(state.objects[&creature].is_phased_out());

        // At the active player's untap step, both phase back in.
        events.clear();
        execute_untap_step_player_phase_in(&mut state, &mut events);
        execute_untap_step_phasing(&mut state, &mut events);

        assert!(
            state.players[0].is_phased_in(),
            "Player must phase in at start of their next turn"
        );
        assert!(
            state.objects[&creature].is_phased_in(),
            "Permanent must phase in at start of controller's next untap step (CR 702.26a)"
        );
    }

    /// CR 702.26g: When the active player's untap step arrives, an aura
    /// that phased out indirectly phases back in along with its host, not
    /// on its own. Test both phase-out *and* the host-driven phase-in.
    #[test]
    fn indirect_aura_phases_back_with_host_on_untap() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        let creature = setup_creature(&mut state, "Bear", PlayerId(0));
        state.objects.get_mut(&creature).unwrap().keywords = vec![Keyword::Phasing];
        state.objects.get_mut(&creature).unwrap().base_keywords = vec![Keyword::Phasing];
        let aura = setup_aura(&mut state, "Boon", PlayerId(0), creature);
        let mut events = Vec::new();

        // First untap step: creature phases out, aura cascades indirectly.
        execute_untap_step_phasing(&mut state, &mut events);
        assert!(state.objects[&creature].is_phased_out());
        assert!(matches!(
            state.objects[&aura].phase_status,
            PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Indirectly
            }
        ));

        events.clear();

        // Second untap step: creature phases back in directly; aura rides
        // along (doesn't phase in on its own — it's indirect).
        execute_untap_step_phasing(&mut state, &mut events);
        assert!(state.objects[&creature].is_phased_in());
        assert!(
            state.objects[&aura].is_phased_in(),
            "Aura must phase in with its host (CR 702.26g)"
        );
    }

    /// CR 702.103g: A bestow Aura that phases in but whose host has left
    /// the battlefield while it was phased out reverts to creature form.
    #[test]
    fn bestow_aura_phases_in_unattached_reverts_to_creature() {
        use crate::game::game_object::BestowFormState;

        let mut state = GameState::new_two_player(42);
        let host = setup_creature(&mut state, "Host Bear", PlayerId(0));
        let bestow_id = setup_aura(&mut state, "Hopeful Eidolon", PlayerId(0), host);

        // Mark the Aura as bestowed (it has both Enchantment and Creature types).
        if let Some(obj) = state.objects.get_mut(&bestow_id) {
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.bestow_form = Some(BestowFormState);
        }

        // Phase out the host (and the bestow Aura cascades indirectly).
        let mut events = Vec::new();
        phase_out_object(&mut state, host, PhaseOutCause::Directly, &mut events);
        assert!(state.objects[&bestow_id].is_phased_out());

        // The attached_to pointer persists across phase-out (CR 702.26d).
        assert!(
            state.objects[&bestow_id].attached_to.is_some(),
            "attached_to must NOT be cleared by phasing — it persists (CR 702.26d)"
        );

        // Kill the host while the Aura is phased out.
        state.objects.remove(&host);

        // Phase in: the bestow Aura's host is gone → CR 702.103g revert.
        events.clear();
        phase_in_object(&mut state, bestow_id, &mut events);

        let obj = &state.objects[&bestow_id];
        assert!(obj.is_phased_in());
        assert!(
            obj.bestow_form.is_none(),
            "CR 702.103g: bestow form must revert when phasing in unattached"
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "CR 702.103g: reverted object is a creature again"
        );
        assert!(
            !obj.card_types.subtypes.iter().any(|s| s == "Aura"),
            "CR 702.103g: Aura subtype removed on revert"
        );
    }
}
