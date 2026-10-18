use crate::game::combat::CombatParticipation;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::resolved_commands::{
    ResolvedCombatMembershipCommand, ResolvedCombatMembershipEdit,
};

/// CR 506.4: Remove a creature from combat — it stops being an attacking,
/// blocking, blocked, and/or unblocked creature.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let targets: Vec<_> = match &ability.effect {
        Effect::RemoveFromCombat {
            target: TargetFilter::SelfRef,
        } => ability.self_ref_binding(state).into_iter().collect(),
        // CR 400.7 + CR 603.7c: a delayed combat-removal whose pinned referent
        // became a new object removes nothing. This read is RAW — the file
        // makes no `resolved_targets` call, so the targeting chokepoint never
        // sees this pin.
        //
        // Slot carve-out applies: the list is passed straight into
        // `effect_object_targets`, which indexes `ParentTargetSlot`
        // positionally. Population is 0 today (`melee`'s filter is a bare
        // `ParentTarget`), but this is the standing 22-call-site constraint,
        // not a card-specific judgement.
        Effect::RemoveFromCombat { target } => {
            let live_targets = ability.live_object_targets(state);
            let pool: &[TargetRef] = if matches!(target, TargetFilter::ParentTargetSlot { .. }) {
                &ability.targets
            } else {
                &live_targets
            };
            super::effect_object_targets(target, pool)
        }
        _ => return Ok(()),
    };

    // CR 400.7 + CR 603.7c + CR 603.7b: the trigger fired and resolved; it
    // affected nothing. PLACEMENT IS LOAD-BEARING — this MUST sit ABOVE the
    // source rebind below. Letting the substitution empty the list instead
    // falls into `vec![ability.source_id]`, which re-binds the effect to the
    // ability's OWN source instead of doing nothing.
    //
    // NOTE this file has no existing pushing early return to mirror — its only
    // other early return (`_ => return Ok(())` above) deliberately pushes
    // nothing. The shape mirrored here is `change_zone.rs` / `sacrifice.rs`.
    // `EffectKind::RemoveFromCombat` (not `EffectKind::from(&ability.effect)`)
    // matches this file's own convention at the unconditional push below.
    //
    // SCOPED TO THE NON-`SelfRef` ARM. A `SelfRef` removal's subject is the
    // source itself, never the snapshot referent, so a stale pin on some other
    // object in `ability.targets` must not cancel it. Without this guard the
    // predicate and the subject it suppresses are decoupled — the same
    // collapse of "no target declared" into "declared referent went stale"
    // that `flip_permanent.rs` and `transform_effect.rs` preserve their raw
    // `as_slice()` match to avoid. Unreachable today (the in-class population
    // is `melee`, whose filter is a bare `ParentTarget`, so no `SelfRef` node
    // co-occurs with a pin), but the coupling is what makes it correct rather
    // than the population.
    let subject_is_self_ref = matches!(
        &ability.effect,
        Effect::RemoveFromCombat {
            target: TargetFilter::SelfRef
        }
    );
    if !subject_is_self_ref && ability.pinned_object_targets_all_stale(state) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::RemoveFromCombat,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // If no explicit targets, apply to source (e.g., "remove it from combat"
    // where "it" refers to the ability source).
    let targets = if targets.is_empty() {
        ability.self_ref_binding(state).into_iter().collect()
    } else {
        targets
    };

    for oid in targets {
        remove_object_from_combat(state, oid);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RemoveFromCombat,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 506.4: Remove a single object from all combat data structures.
/// Reusable building block for any code that needs to remove a permanent from combat
/// (regeneration, effect resolution, phasing out, leaving the battlefield, and
/// "if its controller ... changes" via the Layer 2 settlement in
/// `layers::finish_layer_evaluation`).
///
/// Returns whether combat membership changed, i.e. whether any combat edge
/// naming `oid` was pruned (its attacker entry, its blocking-creature entry,
/// an assignment naming it, or its pending damage). Every such change marks
/// layers dirty. An object holding no combat role prunes nothing, records
/// nothing, and returns `false`.
pub fn remove_object_from_combat(
    state: &mut GameState,
    oid: crate::types::identifiers::ObjectId,
) -> bool {
    // CR 733: read the exact roles being pruned BEFORE the prune, so the journal
    // records what this removal actually did. An object holding no combat role
    // prunes nothing and is not recorded. A blocking creature none of whose
    // attackers remain still holds a role (CR 509.1g), so it is recorded too.
    let participation = CombatParticipation::capture(state, oid);
    if participation.is_empty() {
        return false;
    }
    let reference = state
        .objects
        .get(&oid)
        .map(ObjectIncarnationRef::from_object);

    let changed = crate::game::combat::prune_object_from_combat(state, oid);

    // CR 506.4 + CR 613.1f: a creature removed from combat stops being an
    // attacking, blocking, blocked and/or unblocked creature, so a Layer 6
    // grant keyed on `FilterProp::Attacking` / `Blocking` / `BlockingSource` /
    // `Blocked`, or a `SourceIsAttacking` / `SourceIsBlocking` gate, must be
    // re-derived immediately — whichever role was removed.
    if changed {
        state.layers_dirty.mark_full();
    }

    if let Some(reference) = reference {
        record_combat_membership_removal(state, reference, participation);
    }
    changed
}

/// CR 733: Journals one settled CR 506.4 removal through its owning family.
fn record_combat_membership_removal(
    state: &mut GameState,
    object: ObjectIncarnationRef,
    expected_participation: CombatParticipation,
) {
    let cause = state.current_or_begin_rules_execution_node();
    state
        .resolved_rules_journal
        .record_combat_membership(ResolvedCombatMembershipCommand {
            object,
            edit: ResolvedCombatMembershipEdit::Remove {
                expected_participation,
            },
            cause,
        })
        .expect("resolved combat removal must have a live journal cause");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::{TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn remove_attacker_from_combat() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let blocker_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Blocker".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: obj_id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: true,
                band_id: None,
            }],
            ..Default::default()
        };
        combat.blocker_assignments.insert(obj_id, vec![blocker_id]);
        combat.blocker_to_attacker.insert(blocker_id, vec![obj_id]);
        state.combat = Some(combat);

        let ability = ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert!(combat.attackers.is_empty(), "Attacker should be removed");
        assert!(
            !combat.blocker_assignments.contains_key(&obj_id),
            "Attacker-keyed block assignment must be removed"
        );
        assert!(
            combat
                .blocker_to_attacker
                .get(&blocker_id)
                .is_none_or(|attackers| !attackers.contains(&obj_id)),
            "Departing attacker must be pruned from every blocker's reverse lookup"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::RemoveFromCombat,
                ..
            }
        )));
    }

    #[test]
    fn remove_blocker_from_combat() {
        let mut state = GameState::new_two_player(42);
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let blocker_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blocker".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(0),
                attack_target: AttackTarget::Player(PlayerId(0)),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        };
        combat
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);
        combat
            .blocker_to_attacker
            .insert(blocker_id, vec![attacker_id]);
        state.combat = Some(combat);

        let ability = ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(blocker_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert_eq!(combat.attackers.len(), 1, "Attacker should remain");
        assert!(
            combat
                .blocker_assignments
                .get(&attacker_id)
                .unwrap()
                .is_empty(),
            "Blocker should be removed from assignments"
        );
        assert!(
            !combat.blocker_to_attacker.contains_key(&blocker_id),
            "Blocker should be removed from reverse lookup"
        );
    }

    /// CR 506.4 + CR 613.1f: removing an attacker stops it being attacking, so a
    /// granted "while attacking" keyword must be revoked — layers must re-evaluate.
    /// Fails on revert of the `attacker_removed` mark.
    #[test]
    fn remove_attacker_marks_layers_dirty() {
        let mut state = GameState::new_two_player(42);
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        });
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        remove_object_from_combat(&mut state, attacker_id);

        assert!(
            state.combat.as_ref().unwrap().attackers.is_empty(),
            "attacker should be removed from combat"
        );
        assert!(
            state.layers_dirty.is_dirty(),
            "removing an attacker must mark layers dirty to revoke FilterProp::Attacking {{ defender: None }} grants"
        );
    }

    /// CR 506.4 + CR 613.1f: removing a pure blocker stops it being a blocking
    /// creature, which a Layer 6 `FilterProp::Blocking` grant or a
    /// `SourceIsBlocking` gate reads, so layers must re-evaluate. Paired
    /// negative: removing an object with no combat role leaves layers clean.
    /// Fails on revert of the any-edge `changed` gate (the old attacker-only
    /// gate left layers clean here).
    #[test]
    fn remove_blocker_marks_layers_dirty() {
        let mut state = GameState::new_two_player(42);
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let blocker_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blocker".to_string(),
            Zone::Battlefield,
        );
        let bystander_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(0),
                attack_target: AttackTarget::Player(PlayerId(0)),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        };
        combat
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);
        combat
            .blocker_to_attacker
            .insert(blocker_id, vec![attacker_id]);
        state.combat = Some(combat);
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        // Paired negative: an object holding no combat role prunes nothing.
        assert!(!remove_object_from_combat(&mut state, bystander_id));
        assert!(
            !state.layers_dirty.is_dirty(),
            "removing an object with no combat role must not dirty layers"
        );

        // Remove the blocker — it is not in combat.attackers.
        assert!(remove_object_from_combat(&mut state, blocker_id));

        assert_eq!(
            state.combat.as_ref().unwrap().attackers.len(),
            1,
            "attacker should remain"
        );
        assert!(
            !state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .contains_key(&blocker_id),
            "reach guard: the blocker really was removed"
        );
        assert!(
            state.layers_dirty.is_dirty(),
            "removing a blocker must dirty layers - FilterProp::Blocking / SourceIsBlocking change"
        );
    }

    /// CR 506.4: the return value reports whether combat membership changed —
    /// `true` for an attacker and for a pure blocker, `false` for an object
    /// holding no combat role. The Layer 2 controller-change settlement keys
    /// its re-derivation on this value.
    #[test]
    fn remove_object_from_combat_reports_membership_change() {
        let mut state = GameState::new_two_player(42);
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let blocker_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blocker".to_string(),
            Zone::Battlefield,
        );
        let bystander_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );

        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(0),
                attack_target: AttackTarget::Player(PlayerId(0)),
                blocked: true,
                band_id: None,
            }],
            ..Default::default()
        };
        combat
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);
        combat
            .blocker_to_attacker
            .insert(blocker_id, vec![attacker_id]);
        state.combat = Some(combat);

        assert!(
            !remove_object_from_combat(&mut state, bystander_id),
            "an object with no combat role removes nothing"
        );
        assert!(
            remove_object_from_combat(&mut state, blocker_id),
            "removing a blocking creature changes combat membership"
        );
        assert!(
            !state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .contains_key(&blocker_id),
            "reach guard: the blocker really was removed"
        );
        assert!(
            remove_object_from_combat(&mut state, attacker_id),
            "removing an attacking creature reports true"
        );
        assert!(state.combat.as_ref().unwrap().attackers.is_empty());
    }

    /// A fresh two-player state where P0's `attacker` attacks P1 and is blocked
    /// by P1's `blocker` alone. Returns `(state, attacker, blocker)`.
    fn blocked_pair() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let attacker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        let blocker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Blocker".to_string(),
            Zone::Battlefield,
        );
        let mut combat = CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: true,
                band_id: None,
            }],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        (state, attacker, blocker)
    }

    fn combat_removals(
        state: &GameState,
        start: usize,
    ) -> Vec<crate::types::resolved_commands::ResolvedCombatMembershipCommand> {
        state
            .resolved_rules_journal
            .entries()
            .iter()
            .skip(start)
            .filter_map(|entry| match &entry.command {
                Some(crate::types::resolved_commands::ResolvedRulesCommand::CombatMembership(
                    command,
                )) => Some(command.clone()),
                _ => None,
            })
            .collect()
    }

    /// CR 509.1g + CR 506.4: an attacker leaving combat by ANY route leaves the
    /// creature that was blocking it a blocking creature (with no attacker
    /// assigned, CR 510.1d). The class test covers every shared caller of the
    /// prune: direct removal, `Effect::RemoveFromCombat`, leaving the
    /// battlefield and phasing out. Fails on revert of the kept-empty key in
    /// `prune_object_from_combat` (the old prune dropped the blocker's key).
    #[test]
    fn attacker_removal_routes_keep_blocker_blocking() {
        type Route = fn(&mut GameState, ObjectId);
        let routes: [(&str, Route); 4] = [
            ("remove_object_from_combat", |state, attacker| {
                remove_object_from_combat(state, attacker);
            }),
            ("Effect::RemoveFromCombat", |state, attacker| {
                let ability = ResolvedAbility::new(
                    Effect::RemoveFromCombat {
                        target: TargetFilter::Any,
                    },
                    vec![TargetRef::Object(attacker)],
                    ObjectId(100),
                    PlayerId(1),
                );
                resolve(state, &ability, &mut Vec::new()).unwrap();
            }),
            ("leaves the battlefield", |state, attacker| {
                crate::game::zones::move_to_zone(state, attacker, Zone::Graveyard, &mut Vec::new());
            }),
            ("phases out", |state, attacker| {
                crate::game::phasing::phase_out_object(
                    state,
                    attacker,
                    crate::game::game_object::PhaseOutCause::Directly,
                    &mut Vec::new(),
                );
            }),
        ];

        for (name, route) in routes {
            let (mut state, attacker, blocker) = blocked_pair();
            assert_eq!(
                state.combat.as_ref().unwrap().blocker_to_attacker[&blocker],
                vec![attacker],
                "{name}: reach guard: the blocker blocks the attacker"
            );

            route(&mut state, attacker);

            let combat = state.combat.as_ref().unwrap();
            assert!(
                combat.attackers.iter().all(|a| a.object_id != attacker),
                "{name}: reach guard: the attacker left combat"
            );
            assert_eq!(
                combat.blocker_to_attacker.get(&blocker),
                Some(&vec![]),
                "{name}: CR 509.1g: the blocker remains a blocking creature with no attacker assigned"
            );
            assert!(
                crate::game::zones::capture_combat_status(&state, blocker).blocking,
                "{name}: the blocking-role reader still sees a blocking creature"
            );
        }
    }

    /// CR 506.4 + CR 400.7: a blocking creature with no attacker assigned that
    /// leaves the battlefield is removed from combat like any blocker; when the
    /// same `ObjectId` re-enters it is a new object and is not blocking. The
    /// look-back snapshot taken at exit still sees it blocking (CR 603.10a).
    /// Fails if the receipt treats the orphan as holding no role (the early
    /// return would leave its key behind for the re-entered object).
    #[test]
    fn orphan_exit_prunes_key_and_reentry_is_not_blocking() {
        let (mut state, attacker, orphan) = blocked_pair();
        remove_object_from_combat(&mut state, attacker);
        assert_eq!(
            state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .get(&orphan),
            Some(&vec![]),
            "reach guard: the blocker is an orphaned blocking creature"
        );
        assert!(
            crate::game::zones::capture_combat_status(&state, orphan).blocking,
            "CR 603.10a: the look-back snapshot sees the orphan blocking"
        );

        crate::game::zones::move_to_zone(&mut state, orphan, Zone::Graveyard, &mut Vec::new());
        assert!(
            !state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .contains_key(&orphan),
            "CR 506.4: leaving the battlefield ends the orphan's blocking membership"
        );

        crate::game::zones::move_to_zone(&mut state, orphan, Zone::Battlefield, &mut Vec::new());
        assert_eq!(state.objects[&orphan].zone, Zone::Battlefield);
        assert!(
            !state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .contains_key(&orphan),
            "CR 400.7: the re-entered object does not inherit the blocking role"
        );
        assert!(!crate::game::zones::capture_combat_status(&state, orphan).blocking);
    }

    /// CR 733 + CR 509.1g: removing an orphaned blocking creature journals
    /// exactly one removal whose receipt says "blocking, no attacker assigned"
    /// (`Some([])`), and replaying it onto the predecessor reproduces the prune.
    #[test]
    fn orphan_removal_journals_once_with_empty_assignment_receipt() {
        let (mut state, attacker, orphan) = blocked_pair();
        remove_object_from_combat(&mut state, attacker);
        assert_eq!(
            state
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .get(&orphan),
            Some(&vec![]),
            "reach guard: the blocker is an orphaned blocking creature"
        );
        let before = state.clone();
        let start = state.resolved_rules_journal.entries().len();
        state.layers_dirty = crate::types::game_state::LayersDirty::Clean;

        assert!(
            remove_object_from_combat(&mut state, orphan),
            "removing the orphan changes combat membership"
        );
        assert!(state.layers_dirty.is_dirty());
        assert!(!state
            .combat
            .as_ref()
            .unwrap()
            .blocker_to_attacker
            .contains_key(&orphan));

        let removals = combat_removals(&state, start);
        assert_eq!(removals.len(), 1, "exactly one journaled removal");
        assert_eq!(removals[0].object.object_id, orphan);
        let ResolvedCombatMembershipEdit::Remove {
            expected_participation,
        } = &removals[0].edit
        else {
            panic!("the orphan's removal is a Remove edit");
        };
        assert_eq!(expected_participation.blocking, Some(vec![]));
        assert!(expected_participation.attacking.is_none());

        let mut replay = before;
        crate::game::combat::apply_resolved_combat_membership(&mut replay, &removals[0])
            .expect("the orphan's removal replays against its predecessor");
        assert_eq!(replay.combat, state.combat, "replay reproduces the prune");
    }

    #[test]
    fn remove_from_combat_self_ref() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Runner".to_string(),
            Zone::Battlefield,
        );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: obj_id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        });

        // No explicit targets — should fall back to source
        let ability = ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.is_empty(),
            "Self-ref should remove source from combat"
        );
    }

    #[test]
    fn remove_from_combat_self_ref_ignores_inherited_parent_target() {
        let mut state = GameState::new_two_player(42);
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Runner".to_string(),
            Zone::Battlefield,
        );
        let inherited_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Revealed Card".to_string(),
            Zone::Library,
        );

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker_id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
                band_id: None,
            }],
            ..Default::default()
        });

        let ability = ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(inherited_id)],
            attacker_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.combat.as_ref().unwrap().attackers.is_empty(),
            "SelfRef must remove the source, not the inherited revealed-card target"
        );
    }
}
