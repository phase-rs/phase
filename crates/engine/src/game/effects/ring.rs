use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// CR 701.52: The Ring tempts you.
///
/// CR 701.52a: When the Ring tempts you, if you don't control a Ring-bearer,
/// choose a creature you control. That creature becomes your Ring-bearer.
///
/// CR 701.52b: Each time the Ring tempts you, your ring level increases by one
/// (to a maximum of four levels, 0-indexed as 0–3).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let controller = ability.controller;

    // CR 701.52b: Increment ring level, capping at 4 (the ring has 4 tiers).
    // Level 0 = never tempted (no abilities). Levels 1–4 unlock progressive tiers.
    let level = state.ring_level.entry(controller).or_insert(0);
    if *level < 4 {
        *level += 1;
    }

    // Emit the event so triggers can fire.
    events.push(GameEvent::RingTemptsYou {
        player_id: controller,
    });

    // CR 701.52a: Collect candidate creatures controlled by this player.
    let candidates: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller == controller
                && obj.zone == Zone::Battlefield
                && obj.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(oid)
            } else {
                None
            }
        })
        .collect();

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RingTemptsYou,
        source_id: ability.source_id,
    });

    if candidates.is_empty() {
        // No creatures — ring tempts but no ring-bearer selection.
        return Ok(());
    }

    if candidates.len() == 1 {
        // Only one creature — auto-select as ring-bearer.
        state.ring_bearer.insert(controller, Some(candidates[0]));
        return Ok(());
    }

    // Multiple candidates — ask the player to choose.
    state.waiting_for = WaitingFor::ChooseRingBearer {
        player: controller,
        candidates,
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_creature(state: &mut GameState, card_id: u64, controller: PlayerId) -> ObjectId {
        let oid = create_object(
            state,
            CardId(card_id),
            controller,
            format!("Creature {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        oid
    }

    #[test]
    fn ring_tempts_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::RingTemptsYou,
                ..
            }
        )));
    }

    #[test]
    fn ring_level_caps_at_four() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));

        // Tempt 5 times — level should cap at 4
        for _ in 0..5 {
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
        }

        assert_eq!(state.ring_level[&PlayerId(0)], 4);
    }

    #[test]
    fn ring_tempts_auto_selects_single_creature() {
        let mut state = GameState::new_two_player(42);
        let creature_id = make_creature(&mut state, 1, PlayerId(0));
        let ability =
            ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.ring_bearer.get(&PlayerId(0)),
            Some(&Some(creature_id))
        );
    }

    #[test]
    fn ring_tempts_prompts_choice_for_multiple_creatures() {
        let mut state = GameState::new_two_player(42);
        make_creature(&mut state, 1, PlayerId(0));
        make_creature(&mut state, 2, PlayerId(0));
        let ability =
            ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseRingBearer { .. }
        ));
    }

    #[test]
    fn ring_tempts_no_creatures_still_increments_level() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::RingTemptsYou, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.ring_level[&PlayerId(0)], 1);
        assert_eq!(state.ring_bearer.get(&PlayerId(0)), None);
    }
}
