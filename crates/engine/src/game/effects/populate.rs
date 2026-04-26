use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;

/// CR 701.36a: Populate — choose a creature token you control, then create a
/// token that's a copy of that creature token.
///
/// CR 701.36b: If you control no creature tokens when instructed to populate,
/// you won't create a token.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Collect creature tokens the controller controls on the battlefield.
    let valid_tokens: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if obj.controller == ability.controller
                && obj.is_token
                && obj.card_types.core_types.contains(&CoreType::Creature)
            {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    match valid_tokens.len() {
        // CR 701.36b: No creature tokens → no-op.
        0 => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Populate,
                source_id: ability.source_id,
            });
        }
        // Exactly one → auto-select, no player choice needed.
        1 => {
            create_token_copy(state, valid_tokens[0], ability, events)?;
        }
        // Multiple → player chooses which token to copy.
        _ => {
            state.waiting_for = WaitingFor::PopulateChoice {
                player: ability.controller,
                source_id: ability.source_id,
                valid_tokens,
            };
        }
    }

    Ok(())
}

/// Create a token copy of the selected creature token by delegating to
/// the existing `token_copy::resolve()` handler with a synthetic ability.
pub fn create_token_copy(
    state: &mut GameState,
    token_to_copy: ObjectId,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.2: Build a synthetic CopyTokenOf ability targeting the selected token.
    let copy_ability = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::Any,
            enters_attacking: false,
            tapped: false,
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        vec![TargetRef::Object(token_to_copy)],
        ability.source_id,
        ability.controller,
    );
    super::token_copy::resolve(state, &copy_ability, events)?;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Populate,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityKind, Effect};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature_token(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_token = true;
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        id
    }

    /// CR 701.36b: If you control no creature tokens when instructed to
    /// populate, you won't create a token — populate is a no-op.
    #[test]
    fn populate_no_creature_tokens_is_noop() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(Effect::Populate, vec![], source_id, PlayerId(0));
        let before = state.battlefield.len();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.battlefield.len(),
            before,
            "no token should be created"
        );
        assert!(state.last_created_token_ids.is_empty());
    }

    /// CR 701.36a: With exactly one creature token, populate auto-selects
    /// and creates a copy — no player choice needed.
    #[test]
    fn populate_single_token_auto_copies() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".into(),
            Zone::Battlefield,
        );
        let saproling = setup_creature_token(&mut state, PlayerId(0), "Saproling");
        let ability = Effect::Populate;
        let resolved = ResolvedAbility::new(ability, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &resolved, &mut events).unwrap();

        // One new token on the battlefield, and last_created_token_ids is set.
        assert_eq!(state.last_created_token_ids.len(), 1);
        let new_id = state.last_created_token_ids[0];
        assert_ne!(new_id, saproling);
        let token = state.objects.get(&new_id).unwrap();
        assert!(token.is_token);
        assert_eq!(token.name, "Saproling");
    }

    /// CR 701.36a + CR 603.7: Determined Iteration's end-to-end sequence.
    /// At combat, populate creates a copy; "The token created this way gains
    /// haste" applies via the GenericEffect + LastCreated filter the parser
    /// post-pass rewrites; "Sacrifice it at the beginning of the next end
    /// step" snapshots the populated token via LastCreated at delayed-
    /// trigger creation.
    #[test]
    fn determined_iteration_populate_chain_snapshots_token_for_delayed_sacrifice() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::TargetFilter;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Determined Iteration".into(),
            Zone::Battlefield,
        );
        let saproling = setup_creature_token(&mut state, PlayerId(0), "Saproling");

        // Parse the full effect chain from Oracle text.
        let chain = parse_effect_chain(
            "populate. the token created this way gains haste. sacrifice it at the beginning of the next end step.",
            AbilityKind::Spell,
        );

        // Sanity-check the chain shape: Populate → GenericEffect(LastCreated) → CreateDelayedTrigger.
        assert!(matches!(&*chain.effect, Effect::Populate));
        let gains = chain.sub_ability.as_ref().expect("gains haste sub");
        assert!(
            matches!(
                &*gains.effect,
                Effect::GenericEffect {
                    target: Some(TargetFilter::LastCreated),
                    ..
                }
            ),
            "anaphor should rewrite to GenericEffect LastCreated, got {:?}",
            &*gains.effect
        );
        let delayed = gains.sub_ability.as_ref().expect("delayed trigger sub");
        let (inner_target,) = match &*delayed.effect {
            Effect::CreateDelayedTrigger { effect, .. } => match &*effect.effect {
                Effect::Sacrifice { target, .. } => (target.clone(),),
                other => panic!("expected Sacrifice inside delayed trigger, got {other:?}"),
            },
            other => panic!("expected CreateDelayedTrigger, got {other:?}"),
        };
        assert_eq!(
            inner_target,
            TargetFilter::LastCreated,
            "Sacrifice target should be rewritten from ParentTarget to LastCreated"
        );

        // Simulate populate resolution (single-token path, auto-copy).
        let populate_ability =
            ResolvedAbility::new(Effect::Populate, vec![], source_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &populate_ability, &mut events).unwrap();
        let populated_id = state.last_created_token_ids[0];
        assert_ne!(populated_id, saproling);

        // Now create the delayed trigger with Sacrifice { target: LastCreated }.
        // delayed_trigger::resolve must snapshot last_created_token_ids into
        // the delayed ability's targets so the trigger fires on THIS populated
        // token even if more tokens are created later.
        let delayed_effect_def = match &*delayed.effect {
            Effect::CreateDelayedTrigger { effect, .. } => (**effect).clone(),
            _ => unreachable!(),
        };
        let create_delayed = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: crate::types::ability::DelayedTriggerCondition::AtNextPhase {
                    phase: crate::types::phase::Phase::End,
                },
                effect: Box::new(delayed_effect_def),
                uses_tracked_set: false,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        super::super::delayed_trigger::resolve(&mut state, &create_delayed, &mut events).unwrap();
        assert_eq!(state.delayed_triggers.len(), 1);
        let snapshot = &state.delayed_triggers[0].ability.targets;
        assert_eq!(
            snapshot,
            &vec![TargetRef::Object(populated_id)],
            "delayed trigger must snapshot the populated token id"
        );

        // Overwrite last_created_token_ids (simulating an unrelated token
        // creation between now and end-step). Snapshot should be untouched.
        state.last_created_token_ids = vec![ObjectId(9999)];
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(populated_id)],
            "later token creations must not retarget the snapshotted delayed trigger"
        );
    }
}
