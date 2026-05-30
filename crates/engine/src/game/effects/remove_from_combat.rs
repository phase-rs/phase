use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 506.4: Remove a creature from combat — it stops being an attacking,
/// blocking, blocked, and/or unblocked creature.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let targets: Vec<_> = match &ability.effect {
        Effect::RemoveFromCombat { .. } => ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
        _ => return Ok(()),
    };

    // If no explicit targets, apply to source (e.g., "remove it from combat"
    // where "it" refers to the ability source).
    let targets = if targets.is_empty() {
        vec![ability.source_id]
    } else {
        targets
    };

    for oid in targets {
        remove_object_from_combat(state, oid);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RemoveFromCombat,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 506.4: Remove a single object from all combat data structures.
/// Reusable building block for any code that needs to remove a permanent from combat
/// (regeneration, effect resolution, controller change, etc.).
pub fn remove_object_from_combat(state: &mut GameState, oid: crate::types::identifiers::ObjectId) {
    let mut attacker_removed = false;
    if let Some(ref mut combat) = state.combat {
        // Remove as attacker
        let attackers_before = combat.attackers.len();
        combat.attackers.retain(|a| a.object_id != oid);
        attacker_removed = combat.attackers.len() != attackers_before;
        // Remove as blocker from all attacker assignments
        for blockers in combat.blocker_assignments.values_mut() {
            blockers.retain(|b| *b != oid);
        }
        // Remove reverse blocker lookup
        combat.blocker_to_attacker.remove(&oid);
        // Remove any pending damage assignments for this object
        combat.damage_assignments.remove(&oid);
    }
    // CR 506.4 + CR 613.1f: a creature removed from combat stops being attacking,
    // so a granted "while attacking" keyword (deathtouch/lifelink via
    // FilterProp::Attacking, Layer 6) must be revoked immediately. Mark dirty only
    // when an attacker was actually removed — removing a pure blocker doesn't
    // affect FilterProp::Attacking statics.
    if attacker_removed {
        state.layers_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
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

        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: obj_id,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
            }],
            ..Default::default()
        });

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
            }],
            ..Default::default()
        });
        state.layers_dirty = false;

        remove_object_from_combat(&mut state, attacker_id);

        assert!(
            state.combat.as_ref().unwrap().attackers.is_empty(),
            "attacker should be removed from combat"
        );
        assert!(
            state.layers_dirty,
            "removing an attacker must mark layers dirty to revoke FilterProp::Attacking grants"
        );
    }

    /// CR 506.4: removing a creature that is NOT an attacker (e.g. a pure blocker)
    /// does not change which creatures are attacking, so FilterProp::Attacking
    /// statics are unaffected and layers must NOT be spuriously dirtied. Locks the
    /// `attacker_removed` gate.
    #[test]
    fn remove_blocker_does_not_mark_layers_dirty() {
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
        state.layers_dirty = false;

        // Remove the blocker — it is not in combat.attackers.
        remove_object_from_combat(&mut state, blocker_id);

        assert_eq!(
            state.combat.as_ref().unwrap().attackers.len(),
            1,
            "attacker should remain"
        );
        assert!(
            !state.layers_dirty,
            "removing a pure blocker must not dirty layers — no FilterProp::Attacking change"
        );
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
}
