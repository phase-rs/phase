use crate::game::stack;
use crate::game::targeting::{self, TriggeringSpell};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 107.3e + CR 608.2h: "double the value of X" — rewrite the X of the spell
/// the spell-cast trigger event names, read as it is now, so two triggers
/// compound (Unbound Flourishing ruling: two copies multiply X by four).
/// CR 400.7 + CR 107.3g: only the pinned spell still on the stack is changed;
/// a countered (departed) or gone spell has no X left to change.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ModifyAnnouncedX { modification } = &ability.effect else {
        return Err(EffectError::MissingParam(
            "expected ModifyAnnouncedX effect".into(),
        ));
    };

    // CR 107.3e: "the value of X" is the X of the spell that caused the
    // trigger, looked up at the incarnation pinned when the trigger was put on
    // the stack (CR 400.7) — never the topmost or any other live spell.
    let subject = match targeting::triggering_spell(state) {
        Some(TriggeringSpell::OnStack(entry)) => Some(entry.id),
        Some(TriggeringSpell::Departed(_) | TriggeringSpell::Gone) | None => None,
    };
    if let Some(id) = subject {
        stack::rewrite_stack_object_x(state, id, |x| modification.apply(x));
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ModifyAnnouncedX,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::game_state::StackEntryKind;
    use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;

    const UNBOUND: &str = "Whenever you cast a permanent spell with a mana cost that contains {X}, double the value of X.\nWhenever you cast an instant or sorcery spell or activate an ability, if that spell's mana cost or that ability's activation cost contains {X}, copy that spell or ability. You may choose new targets for the copy.";
    const WALKING_BALLISTA: &str = "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";

    /// PIN-1 (CR 400.7 + CR 107.3e, supplemental resolver-level coverage): the
    /// event's spell id live on the stack at a later incarnation than the
    /// trigger's pin is a different object, so the doubling leaves it alone;
    /// the same trigger pinned at the live incarnation doubles it. Ordinary
    /// play cannot put a recast spell below the old trigger, so the pin is set
    /// by hand on a trigger the real cast pipeline put on the stack.
    #[test]
    fn a_pin_naming_an_earlier_incarnation_is_not_rewritten() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let unbound = scenario
            .add_enchantment_from_oracle(P0, "Unbound Flourishing", UNBOUND)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            })
            .id();
        let ballista = scenario
            .add_creature_to_hand_from_oracle(P0, "Walking Ballista", 0, 0, WALKING_BALLISTA)
            .as_artifact_creature()
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::X],
                generic: 0,
            })
            .id();
        let mut runner = scenario.build();
        for _ in 0..2 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        drop(runner.cast(ballista).x(1).commit());

        let state = runner.state_mut();
        let trigger = state.stack.back().cloned().expect("Unbound's trigger");
        let StackEntryKind::TriggeredAbility {
            source_id,
            trigger_event,
            ..
        } = &trigger.kind
        else {
            panic!("the top of the stack is Unbound's trigger: {trigger:?}");
        };
        assert_eq!(*source_id, unbound);
        let live = state.objects[&ballista].incarnation;
        assert_eq!(
            trigger.ability().unwrap().context.triggering_spell,
            Some(ObjectIncarnationRef::of(ballista, live)),
            "reach: placement pinned the live incarnation"
        );
        let earlier = live
            .checked_sub(1)
            .expect("casting moved Ballista from hand to the stack");
        let event = trigger_event.clone();

        let resolve_pinned_at = |state: &mut GameState, incarnation: u64| {
            let mut entry = trigger.clone();
            entry.ability_mut().unwrap().context.triggering_spell =
                Some(ObjectIncarnationRef::of(ballista, incarnation));
            state.resolving_stack_entry = Some(entry.clone());
            state.current_trigger_event = event.clone();
            let mut events = Vec::new();
            resolve(state, entry.ability().unwrap(), &mut events).unwrap();
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        GameEvent::EffectResolved {
                            kind: EffectKind::ModifyAnnouncedX,
                            ..
                        }
                    )
                })
                .count()
        };
        let x_channels = |state: &GameState| {
            (
                state.objects[&ballista].cost_x_paid,
                state.stack_paid_facts[&ballista].x_value,
            )
        };

        assert_eq!(resolve_pinned_at(state, earlier), 1);
        assert_eq!(
            x_channels(state),
            (Some(1), Some(1)),
            "a pin naming an earlier incarnation does not name the live spell"
        );

        assert_eq!(resolve_pinned_at(state, live), 1);
        assert_eq!(
            x_channels(state),
            (Some(2), Some(2)),
            "the pin at the live incarnation doubles it"
        );
    }
}
