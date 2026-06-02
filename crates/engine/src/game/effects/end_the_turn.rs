use crate::game::effects::change_zone::{self, ZoneMoveResult};
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::zones::Zone;

/// CR 724.1: End the turn. Time Stop, Sundial of the Infinite, Obeka, Glorious
/// End, Discontinuity, Day's Undoing.
///
/// The steps differ from normal spell/ability resolution:
/// - CR 724.1a: triggered abilities that fired before this process but are not
///   yet on the stack cease to exist.
/// - CR 724.1b: exile every object on the stack.
/// - CR 724.1c: check state-based actions (no priority, no new triggers stacked).
/// - CR 724.1d: remove everything from combat and skip straight to the cleanup
///   step.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 724.1a: Triggered abilities that triggered before this process began
    // but haven't been put on the stack yet cease to exist. Abilities that
    // trigger DURING this process (CR 724.1f) are created after this point and
    // ride to the stack through the normal cleanup-step flow.
    state.pending_trigger = None;
    state.pending_trigger_entry = None;
    state.pending_trigger_order = None;
    state.pending_trigger_event_batch.clear();
    state.deferred_triggers.clear();

    // CR 724.1b: Exile every object on the stack. `resolve_top` already popped
    // the end-the-turn source's own entry before invoking this resolver; its
    // post-resolution routing also uses CR 724.1b and sends that resolving
    // object to exile. Here we exile every OTHER object still on the stack.
    // Spell entries are card-backed and move to exile through the shared
    // zone-change pipeline; ability entries (activated / triggered / keyword)
    // aren't represented by cards, so dropping the stack entry is sufficient
    // (they cease to exist at the next state-based-action check, CR 724.1b).
    while let Some(entry) = state.stack.pop_back() {
        state.stack_paid_facts.remove(&entry.id);
        if matches!(entry.kind, StackEntryKind::Spell { .. }) {
            match change_zone::execute_zone_move(
                state,
                entry.id,
                Zone::Stack,
                Zone::Exile,
                ability.source_id,
                None,
                false,
                false,
                None,
                &[],
                false,
                events,
            ) {
                ZoneMoveResult::Done => {}
                ZoneMoveResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
                ZoneMoveResult::NeedsAuraAttachmentChoice => return Ok(()),
            }
        }
    }

    // CR 724.1c: Check state-based actions. No player gets priority and no
    // triggered abilities are put on the stack as part of this step.
    crate::game::sba::check_state_based_actions(state, events);

    // CR 724.1d: Remove all creatures and planeswalkers from combat. Clearing
    // the combat state is the engine's idiom for ending combat (see the
    // end-of-combat handling in `turns.rs`); attacking/blocking status is
    // derived from `state.combat`.
    state.combat = None;

    // CR 724.1d: Skip straight to the cleanup step (skipping any intervening
    // phases/steps, including the end step — CR 724.1e).
    crate::game::turns::end_turn_to_cleanup(state, events);

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::EndTheTurn,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::ability::Effect;
    use crate::types::game_state::{CastingVariant, StackEntry};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    #[test]
    fn end_the_turn_exiles_stack_clears_combat_and_enters_cleanup() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::DeclareAttackers;

        // CR 724.1b: a non-source spell on the stack must be exiled.
        let other_spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bolt".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: other_spell,
            source_id: other_spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // CR 724.1d: an attacking creature must be removed from combat.
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: PlayerId(1),
                attack_target: AttackTarget::Player(PlayerId(1)),
                blocked: false,
            }],
            ..Default::default()
        });

        // The end-the-turn source (e.g. Time Stop). resolve_top pops its own
        // stack entry before invoking the resolver, so it is not on the stack
        // here — only the other spell is.
        let ability = ResolvedAbility::new(Effect::EndTheTurn, vec![], ObjectId(999), PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 724.1b: stack exiled/emptied.
        assert!(state.stack.is_empty(), "stack should be emptied");
        assert_eq!(
            state.objects.get(&other_spell).map(|o| o.zone),
            Some(Zone::Exile),
            "the other spell should be exiled"
        );
        // CR 724.1d: combat removed and we skipped straight to the cleanup step.
        assert!(state.combat.is_none(), "combat should be cleared");
        assert_eq!(
            state.phase,
            Phase::Cleanup,
            "should skip to the cleanup step"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::EndTheTurn,
                    ..
                }
            )),
            "should emit EffectResolved(EndTheTurn)"
        );
    }
}
