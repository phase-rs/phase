use crate::game::targeting::find_legal_targets;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::keywords::Keyword;
use crate::types::ObjectId;

/// CR 115.7: Change the target(s) of a spell or ability on the stack.
///
/// Resolves in two modes:
/// - `forced_to` is `Some`: directly update the stack entry's targets to the resolved target.
/// - `forced_to` is `None`: set `WaitingFor::RetargetChoice` so the player selects the new target.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ChangeTargets {
        scope, forced_to, ..
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "ChangeTargets effect missing".to_string(),
        ));
    };

    // ability.targets[0] is the TargetRef::Object(id) of the stack entry being retargeted.
    let stack_entry_id = match ability.targets.first() {
        Some(TargetRef::Object(id)) => *id,
        _ => {
            return Err(EffectError::MissingParam(
                "ChangeTargets requires a stack entry target".to_string(),
            ))
        }
    };

    // CR 115.7: Find the stack entry by its object ID.
    let stack_entry_index = state
        .stack
        .iter()
        .position(|e| e.id == stack_entry_id)
        .ok_or_else(|| {
            EffectError::MissingParam("ChangeTargets: targeted entry not on stack".to_string())
        })?;

    if let Some(filter) = forced_to {
        // CR 115.7: Forced retarget — resolve the new target from the filter,
        // but only apply it if the targeted stack entry could legally target it.
        let legal_new_targets = legal_new_targets_for_stack_entry(state, stack_entry_index);
        let new_targets = find_legal_targets(state, filter, ability.controller, ability.source_id);
        if let Some(new_target) = new_targets
            .into_iter()
            .find(|target| legal_new_targets.contains(target))
        {
            if let Some(stack_ability) = state.stack[stack_entry_index].ability_mut() {
                stack_ability.targets = vec![new_target];
            }
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Interactive retarget: present choices to the player.
    // CR 115.7a: The current targets of the targeted spell/ability become the starting point.
    let Some(stack_ability) = state.stack[stack_entry_index].ability().cloned() else {
        // Permanent spell with no ability — nothing to retarget.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };
    let current_targets = stack_ability.targets.clone();

    // CR 115.7: Enumerate legal new targets by re-evaluating the stack entry's
    // own targeting restriction against the current game state.
    //
    // CR 303.4a: An Aura spell's target is defined by its enchant *ability*, not
    // by its effect's target field — the synthesized spell ability carries a
    // placeholder effect with no targetable filter (`target_filter()` is `None`).
    // Enumerate Aura hosts from the source's `Keyword::Enchant(filter)` instead,
    // mirroring the Aura branch of `casting::spell_has_legal_targets`. Non-Aura
    // spells/abilities fall back to the effect's declared target filter.
    let legal_new_targets = legal_new_targets_for_stack_ability(state, &stack_ability);

    state.waiting_for = WaitingFor::RetargetChoice {
        player: ability.controller,
        stack_entry_index,
        scope: scope.clone(),
        current_targets,
        legal_new_targets,
    };
    // EffectResolved is emitted by the engine handler after RetargetSpell action is submitted.
    Ok(())
}

/// Extract the target filter from an effect variant, if it has a standard `target` field.
/// Used to compute legal alternative targets for retargeting (CR 115.7).
fn extract_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    effect.target_filter()
}

/// CR 115.7: Enumerate the legal replacement targets for a spell or ability on
/// the stack by re-evaluating that stack entry's own target restriction against
/// current game state. Shared by interactive retargets, forced retargets, and AI
/// policy scoring so they cannot disagree about what can be changed to what.
pub fn legal_new_targets_for_stack_entry(
    state: &GameState,
    stack_entry_index: usize,
) -> Vec<TargetRef> {
    state
        .stack
        .get(stack_entry_index)
        .and_then(|entry| entry.ability())
        .map(|ability| legal_new_targets_for_stack_ability(state, ability))
        .unwrap_or_default()
}

fn legal_new_targets_for_stack_ability(
    state: &GameState,
    stack_ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    // CR 303.4a: An Aura spell's target is defined by its enchant ability, not
    // by the placeholder effect synthesized for the spell on the stack.
    let retarget_filter = aura_enchant_filter(state, stack_ability.source_id)
        .or_else(|| extract_target_filter(&stack_ability.effect).cloned());
    retarget_filter
        .map(|filter| {
            find_legal_targets(
                state,
                &filter,
                stack_ability.controller,
                stack_ability.source_id,
            )
        })
        .unwrap_or_else(|| stack_ability.targets.clone())
}

/// CR 303.4a: An Aura spell's legal targets are defined by its enchant ability —
/// modeled here as the source object's `Keyword::Enchant(filter)` — not by its
/// (placeholder) spell effect. Returns that filter when `source_id` is an Aura,
/// so retargeting an Aura spell (CR 115.7) enumerates the permanents it could
/// legally enchant. Mirrors the Aura branch of `casting::spell_has_legal_targets`.
pub(crate) fn aura_enchant_filter(state: &GameState, source_id: ObjectId) -> Option<TargetFilter> {
    let obj = state.objects.get(&source_id)?;
    if !obj.card_types.subtypes.iter().any(|s| s == "Aura") {
        return None;
    }
    obj.keywords.iter().find_map(|k| match k {
        Keyword::Enchant(filter) => Some(filter.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TypeFilter, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{CastingVariant, RetargetScope, StackEntry, StackEntryKind};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// CR 303.4a + CR 115.7: Retargeting an Aura spell must enumerate every
    /// permanent the Aura could legally enchant (via its `Keyword::Enchant`
    /// filter), not just keep the original host. Regression test for Bolt Bend
    /// vs. an Aura on the stack: before the fix, the Aura's placeholder spell
    /// effect (`Unimplemented`, whose `target_filter()` is `None`) collapsed the
    /// legal set to the current target, so the player could never pick a new host.
    #[test]
    fn retarget_aura_spell_enumerates_other_enchantable_hosts() {
        let mut state = GameState::new_two_player(42);

        // Two enchantable creatures on the battlefield: the current host and an
        // alternative the player should be able to redirect the Aura onto.
        let host_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear A".into(),
            Zone::Battlefield,
        );
        let host_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear B".into(),
            Zone::Battlefield,
        );
        for id in [host_a, host_b] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        // An Aura spell on the stack, currently targeting host_a, carrying the
        // placeholder spell ability the cast path synthesizes for Auras (its
        // effect has no target filter; targeting is via the Enchant keyword).
        let aura_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Test Aura".into(),
            Zone::Stack,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types = vec![CoreType::Enchantment];
            aura.card_types.subtypes = vec!["Aura".to_string()];
            aura.keywords = vec![Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            ))];
        }
        let aura_spell_ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(host_a)],
            aura_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(aura_spell_ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Bolt Bend: ChangeTargets targeting the Aura spell, no forced target.
        let bolt_bend = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::Single,
                forced_to: None,
            },
            vec![TargetRef::Object(aura_id)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &bolt_bend, &mut events).unwrap();

        let WaitingFor::RetargetChoice {
            current_targets,
            legal_new_targets,
            ..
        } = &state.waiting_for
        else {
            panic!("expected RetargetChoice, got {:?}", state.waiting_for);
        };
        assert_eq!(current_targets, &vec![TargetRef::Object(host_a)]);
        // Discriminating assertion: the alternative host is offered, so the
        // player can actually redirect the Aura. Pre-fix this list was [host_a].
        assert!(
            legal_new_targets.contains(&TargetRef::Object(host_b)),
            "expected alternative enchantable host in legal targets, got {legal_new_targets:?}"
        );
        assert!(legal_new_targets.contains(&TargetRef::Object(host_a)));
    }

    /// CR 608.2b + CR 115.7 + CR 303.4a: End-to-end regression for Bolt Bend
    /// retargeting an Aura spell, driving the real `stack::resolve_top` pipeline.
    /// Guards two stacked bugs:
    ///   1. `Effect::ChangeTargets` was absent from `Effect::target_filter()`, so
    ///      resolution-time re-validation (CR 608.2b) fell to the battlefield-only
    ///      default and dropped the stack-spell target → Bolt Bend always fizzled
    ///      before its effect ran (no `RetargetChoice`).
    ///   2. Once it stopped fizzling, the Aura's hosts had to be enumerated via
    ///      its `Keyword::Enchant` filter (CR 303.4a), not its placeholder effect.
    ///
    /// Pre-fix, `resolve_top` left `waiting_for == Priority` with Bolt Bend in the
    /// graveyard and the Aura untouched. Post-fix it pauses on `RetargetChoice`
    /// offering every other enchantable creature.
    #[test]
    fn bolt_bend_retargets_aura_spell_via_resolve_top() {
        use crate::types::ability::{FilterProp, TypedFilter};

        let mut state = GameState::new_two_player(42);

        // Current host + an alternative host on the battlefield.
        let host_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear A".into(),
            Zone::Battlefield,
        );
        let host_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear B".into(),
            Zone::Battlefield,
        );
        for id in [host_a, host_b] {
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        }

        // Aura spell on the stack, targeting host_a, with the placeholder spell
        // ability the cast path synthesizes for Auras.
        let aura_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Test Aura".into(),
            Zone::Stack,
        );
        {
            let aura = state.objects.get_mut(&aura_id).unwrap();
            aura.card_types.core_types = vec![CoreType::Enchantment];
            aura.card_types.subtypes = vec!["Aura".to_string()];
            aura.keywords = vec![Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            ))];
        }
        let aura_spell_ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            vec![TargetRef::Object(host_a)],
            aura_id,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(aura_spell_ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Bolt Bend on top of the stack, targeting the Aura spell, with its real
        // filter: (StackSpell & HasSingleTarget) | (StackAbility & HasSingleTarget).
        let single = TargetFilter::Typed(TypedFilter {
            type_filters: vec![],
            controller: None,
            properties: vec![FilterProp::HasSingleTarget],
        });
        let bb_filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::And {
                    filters: vec![TargetFilter::StackSpell, single.clone()],
                },
                TargetFilter::And {
                    filters: vec![
                        TargetFilter::StackAbility {
                            controller: None,
                            tag: None,
                        },
                        single,
                    ],
                },
            ],
        };
        let bolt_bend = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Bolt Bend".into(),
            Zone::Stack,
        );
        let bb_ability = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: bb_filter,
                scope: RetargetScope::Single,
                forced_to: None,
            },
            vec![TargetRef::Object(aura_id)],
            bolt_bend,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: bolt_bend,
            source_id: bolt_bend,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(4),
                ability: Some(bb_ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // Bolt Bend must NOT fizzle: it pauses on RetargetChoice (the aura spell
        // stays on the stack awaiting the new host), rather than going to the
        // graveyard with waiting_for == Priority.
        let WaitingFor::RetargetChoice {
            current_targets,
            legal_new_targets,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "expected RetargetChoice (Bolt Bend fizzled instead), got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(current_targets, &vec![TargetRef::Object(host_a)]);
        assert!(
            legal_new_targets.contains(&TargetRef::Object(host_b)),
            "alternative enchantable host must be offered, got {legal_new_targets:?}"
        );
        assert!(state.stack.iter().any(|e| e.id == aura_id));

        // CR 115.7: A single-target retarget resolves through the universal
        // `ChooseTarget` board-click action — the player picks the new host
        // directly on the battlefield rather than through the dialog. The Aura
        // spell's target must update to the chosen host and priority resumes.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(host_b)),
            },
        )
        .expect("board-click retarget should succeed");

        let aura_targets = state
            .stack
            .iter()
            .find(|e| e.id == aura_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .expect("aura spell still on stack with targets");
        assert_eq!(
            aura_targets,
            vec![TargetRef::Object(host_b)],
            "ChooseTarget board-click must retarget the Aura to the chosen host"
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 115.7b: "Change a target ... to this permanent" still has to obey
    /// the targeted spell's own target restriction. Spellskite cannot become
    /// the target of "destroy target nonartifact creature" because it is an
    /// artifact creature.
    #[test]
    fn forced_retarget_ignores_illegal_self_target() {
        let mut state = GameState::new_two_player(42);

        let bear = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".into(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear).unwrap().card_types.core_types = vec![CoreType::Creature];

        let spellskite = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spellskite".into(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&spellskite)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact, CoreType::Creature];

        let spell_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Test Doom Blade".into(),
            Zone::Stack,
        );
        let nonartifact_creature = TargetFilter::Typed(
            TypedFilter::creature().with_type(TypeFilter::Non(Box::new(TypeFilter::Artifact))),
        );
        let destroy_ability = ResolvedAbility::new(
            Effect::Destroy {
                target: nonartifact_creature,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear)],
            spell_id,
            PlayerId(1),
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(3),
                ability: Some(destroy_ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let spellskite_ability = ResolvedAbility::new(
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: RetargetScope::Single,
                forced_to: Some(TargetFilter::SelfRef),
            },
            vec![TargetRef::Object(spell_id)],
            spellskite,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &spellskite_ability, &mut events).unwrap();

        let targets = state
            .stack
            .front()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone())
            .expect("targeted spell remains on stack");
        assert_eq!(targets, vec![TargetRef::Object(bear)]);
    }
}
