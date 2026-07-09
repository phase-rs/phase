//! CR 509.1h: effect-level "becomes blocked" resolver.
//!
//! Backs the ~21-card "target ... becomes blocked" class (e.g. Dazzling Beauty:
//! "Target unblocked attacking creature becomes blocked."). An effect can make an
//! attacking creature blocked even if no creature is declared as a blocker for it,
//! and it remains blocked even if it never had any blockers (CR 509.1h). Per
//! CR 510.1c a blocked creature with no creatures blocking it assigns no combat
//! damage — so the marked attacker deals no combat damage this turn.
//!
//! CR 509.3c: the `AttackerBecameBlockedByEffect` event is emitted only when the
//! attacker was an *unblocked* creature at the instant the effect resolved — the
//! precondition for "whenever ~ becomes blocked" triggers to fire from an effect.
//! CR 509.3d: because this is a distinct event (not `BlockersDeclared`), the
//! "becomes blocked BY A CREATURE" form and blocker-side "whenever ~ blocks"
//! triggers do NOT fire.

use crate::game::combat;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// Resolve the object target(s) of a `BecomeBlocked` effect. Mirrors the
/// `resolve_object_targets` chokepoint in `saddle.rs`:
/// - `SelfRef` → the source (a "~ becomes blocked" self-anaphor)
/// - a `ParentTarget`/context ref with no announced target → the source
///   (via `resolve_event_context_targets`)
/// - otherwise → the announced object targets (Dazzling Beauty's chosen attacker)
fn resolve_object_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let Effect::BecomeBlocked { target } = &ability.effect else {
        return Vec::new();
    };
    // CR 608.2c: the printed-name anaphor always resolves to the source.
    if matches!(target, TargetFilter::SelfRef) {
        return vec![ability.source_id];
    }
    // CR 608.2c: a context ref (`ParentTarget`/`TriggeringSource`) resolves from
    // the trigger event; empty for a plain targeted effect, which falls through
    // to the announced targets below.
    let event_targets =
        crate::game::targeting::resolve_event_context_targets(state, target, ability.source_id);
    if !event_targets.is_empty() {
        return event_targets
            .into_iter()
            .filter_map(|t| match t {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            })
            .collect();
    }
    ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect()
}

/// CR 509.1h: resolver for `Effect::BecomeBlocked` — the target attacking
/// creature(s) become blocked with no blockers assigned. Emits
/// `AttackerBecameBlockedByEffect` for each attacker that was unblocked at the
/// instant of resolution (CR 509.3c), so "whenever ~ becomes blocked" triggers
/// fire. An illegal target (no longer a current attacker) is a no-op: CR 608.2b
/// fizzle is enforced upstream by `target_filter()`; a stale non-attacker id
/// simply fails `mark_attacker_blocked`.
pub fn resolve_become_blocked(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for oid in resolve_object_targets(state, ability) {
        // CR 509.3c: read whether the attacker was unblocked BEFORE mutating —
        // the "becomes blocked" trigger fires from an effect only if the attacker
        // was an unblocked creature at that time.
        let was_unblocked = state
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == oid && !a.blocked));
        if combat::mark_attacker_blocked(state, oid) && was_unblocked {
            events.push(GameEvent::AttackerBecameBlockedByEffect { attacker: oid });
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::BecomeBlocked,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn attacker_on_battlefield(state: &mut GameState, blocked: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Attacker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        let combat = state.combat.get_or_insert_with(CombatState::default);
        let mut info = AttackerInfo::new(id, AttackTarget::Player(PlayerId(1)), PlayerId(1));
        info.blocked = blocked;
        combat.attackers.push(info);
        id
    }

    #[test]
    fn become_blocked_marks_unblocked_attacker_and_emits_event() {
        // CR 509.1h + CR 509.3c: an unblocked attacker becomes blocked and the
        // effect-block event fires (precondition for "becomes blocked" triggers).
        let mut state = GameState::new_two_player(42);
        let attacker = attacker_on_battlefield(&mut state, false);
        let ability = ResolvedAbility::new(
            Effect::BecomeBlocked {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(attacker)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_blocked(&mut state, &ability, &mut events).unwrap();

        assert!(state
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|a| a.object_id == attacker && a.blocked));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::AttackerBecameBlockedByEffect { attacker: a } if *a == attacker)
        ));
    }

    #[test]
    fn become_blocked_already_blocked_does_not_emit_event() {
        // CR 509.3c: an attacker already blocked (e.g. via place_blocking) was not
        // an unblocked creature, so no effect-block event fires — the was_unblocked
        // guard. This assertion fails if the guard is removed.
        let mut state = GameState::new_two_player(42);
        let attacker = attacker_on_battlefield(&mut state, true);
        let ability = ResolvedAbility::new(
            Effect::BecomeBlocked {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(attacker)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_blocked(&mut state, &ability, &mut events).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackerBecameBlockedByEffect { .. })));
        // Reach-guard: the effect still ran (EffectResolved present) so the absence
        // of the block event is not vacuous.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::BecomeBlocked,
                ..
            }
        )));
    }

    #[test]
    fn become_blocked_non_attacker_target_is_noop() {
        // A stale / non-attacking target id fails mark_attacker_blocked and emits
        // no block event, but the effect still resolves.
        let mut state = GameState::new_two_player(42);
        let non_attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bystander".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::BecomeBlocked {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(non_attacker)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_become_blocked(&mut state, &ability, &mut events).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackerBecameBlockedByEffect { .. })));
    }
}
