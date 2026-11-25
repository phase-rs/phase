//! CR 508.7: reselect which player, planeswalker, or battle an attacking
//! creature is attacking (Misleading Signpost, Portal Mage).
//!
//! The resolver only selects the attacking creature and the chooser; the
//! legal destinations, the in-place rewrite and its journal entry all belong
//! to `combat::choose_reselected_attack_target_or_apply` and
//! `combat::reselect_attack_target`.

use crate::game::combat;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 508.7 + CR 603.5: resolve `Effect::ReselectAttackTarget`. The "you" of
/// "you may reselect" is the ability's controller, who chooses among the
/// attacking creature's legal destinations (CR 508.7c). A single legal
/// destination is applied directly; several park
/// `WaitingFor::EntryAttackTargetChoice { purpose: Reselect }`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ReselectAttackTarget { target } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "ReselectAttackTarget target".to_string(),
        ));
    };
    let ids = super::resolved_effect_object_ids(state, ability, target);
    match ids.as_slice() {
        // CR 608.2b: an illegal target leaves nothing to reselect.
        [] => {}
        [attacker] => {
            // CR 506.4: only a listed attacker that is still an attacking
            // creature has an attacked relation to reselect.
            let is_listed = state
                .combat
                .as_ref()
                .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == *attacker));
            if is_listed && combat::is_attacker_in_play(state, *attacker) {
                combat::choose_reselected_attack_target_or_apply(
                    state,
                    *attacker,
                    ability.controller,
                );
            }
        }
        _ => {
            return Err(EffectError::InvalidParam(
                "reselection names one attacking creature".to_string(),
            ));
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ReselectAttackTarget,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}
