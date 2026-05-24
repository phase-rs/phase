use crate::game::effects::choose_one_of;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectError, EffectKind, PtValue, QuantityExpr,
    ResolvedAbility, TargetFilter,
};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::mana::ManaColor;

/// CR 701.63a: Endure N.
///
/// The enduring permanent's controller creates an N/N white Spirit creature
/// token unless they put N +1/+1 counters on that permanent. This is a
/// two-branch "choose one" keyword action, so the resolver composes the
/// existing `ChooseOneOf` modal machine rather than reimplementing the
/// branch-choice state machine.
///
/// CR 701.63b: Endure 0 does nothing — no token is created and no counters are
/// put on the permanent.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::Endure { amount } => *amount,
        _ => return Ok(()),
    };

    // CR 701.63b: Endure 0 — nothing happens, no prompt is presented.
    if amount == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Endure,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.63a + CR 111.1/111.4: Branch A creates one N/N white Spirit
    // creature token. The N is the token's power/toughness, not the count —
    // endure always creates a single token.
    let mut token_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Token {
            name: String::new(),
            power: PtValue::Fixed(amount as i32),
            toughness: PtValue::Fixed(amount as i32),
            types: vec!["Creature".to_string(), "Spirit".to_string()],
            colors: vec![ManaColor::White],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
    );
    token_branch.description = Some(format!("Create a {amount}/{amount} white Spirit token."));

    // CR 701.63a + CR 122.1: Branch B puts N +1/+1 counters on the enduring
    // permanent. `SelfRef` resolves to the ability source (the permanent that
    // is enduring).
    let mut counter_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed {
                value: amount as i32,
            },
            target: TargetFilter::SelfRef,
        },
    );
    counter_branch.description = Some(format!("Put {amount} +1/+1 counters on it."));

    // CR 701.63a: "that permanent's controller" makes the choice — a single
    // chooser. Delegate to the modal machine, which sets
    // `WaitingFor::ChooseOneOfBranch` and owns AI/multiplayer/frontend wiring.
    choose_one_of::prompt_next(
        state,
        ability.controller,
        ability.source_id,
        vec![token_branch, counter_branch],
        ability.targets.clone(),
        ability.context.clone(),
        vec![ability.controller],
    );

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Endure,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Put a creature on PlayerId(0)'s battlefield and return a `ResolvedAbility`
    /// whose source is that creature, carrying `Effect::Endure { amount }`. The
    /// state is left in a `Priority` waiting state so `apply_as_current` can
    /// drive the resulting branch choice.
    fn setup(amount: u32) -> (GameState, ObjectId, ResolvedAbility) {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let ability =
            ResolvedAbility::new(Effect::Endure { amount }, vec![], source_id, PlayerId(0));
        (state, source_id, ability)
    }

    fn spirit_tokens(state: &GameState) -> Vec<ObjectId> {
        state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Spirit"))
                    .unwrap_or(false)
            })
            .collect()
    }

    #[test]
    fn endure_counter_branch_puts_counters_on_source() {
        let (mut state, source_id, ability) = setup(2);
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The resolver prompts the controller via the modal machine.
        let counter_index = match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch {
                player,
                source_id: prompt_source,
                branches,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*prompt_source, source_id);
                branches
                    .iter()
                    .position(|b| matches!(*b.effect, Effect::PutCounter { .. }))
                    .expect("counter branch present")
            }
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        };

        apply_as_current(
            &mut state,
            GameAction::ChooseBranch {
                index: counter_index,
            },
        )
        .unwrap();

        // CR 122.1: 2 +1/+1 counters on the enduring permanent.
        assert_eq!(
            state.objects[&source_id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2)
        );
        // No token was created.
        assert!(spirit_tokens(&state).is_empty());
    }

    #[test]
    fn endure_token_branch_creates_spirit_token() {
        let (mut state, source_id, ability) = setup(2);
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let token_index = match &state.waiting_for {
            WaitingFor::ChooseOneOfBranch { branches, .. } => branches
                .iter()
                .position(|b| matches!(*b.effect, Effect::Token { .. }))
                .expect("token branch present"),
            other => panic!("expected ChooseOneOfBranch, got {other:?}"),
        };

        apply_as_current(&mut state, GameAction::ChooseBranch { index: token_index }).unwrap();

        // CR 701.63a: a 2/2 white Spirit creature token controlled by the
        // endure effect's controller.
        let tokens = spirit_tokens(&state);
        assert_eq!(tokens.len(), 1);
        let token = &state.objects[&tokens[0]];
        assert!(token.is_token);
        assert_eq!(token.power, Some(2));
        assert_eq!(token.toughness, Some(2));
        assert_eq!(token.color, vec![ManaColor::White]);
        assert_eq!(token.controller, PlayerId(0));
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(token.card_types.subtypes.iter().any(|s| s == "Spirit"));
        // No counters were put on the enduring permanent.
        assert!(!state.objects[&source_id]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
    }

    #[test]
    fn endure_zero_does_nothing() {
        let (mut state, source_id, ability) = setup(0);
        let mut events = Vec::new();

        // CR 701.63b: endure 0 — no prompt, no token, no counters.
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
            "endure 0 must not present a branch choice"
        );
        assert!(spirit_tokens(&state).is_empty());
        assert!(!state.objects[&source_id]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
    }
}
