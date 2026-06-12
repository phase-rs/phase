use crate::types::ability::{
    Effect, EffectError, EffectKind, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 701.19a: "Regenerate [permanent]" creates a one-shot replacement shield.
/// The next time that permanent would be destroyed this turn, instead remove
/// all damage marked on it, tap it, and remove it from combat.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // If no explicit targets, regenerate the source itself.
    // "Regenerate this creature" parses to Any with no targeting keyword,
    // so when targets are empty we default to self.
    let use_self = match &ability.effect {
        Effect::Regenerate { target } => {
            matches!(target, TargetFilter::None | TargetFilter::SelfRef)
                || (matches!(target, TargetFilter::Any) && ability.targets.is_empty())
        }
        _ => false,
    };

    let targets: Vec<_> = if use_self {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    };

    // CR 614.8: Regeneration is a destruction-replacement effect. The word "instead"
    // is implicit: "The next time [permanent] would be destroyed this turn, instead
    // remove all damage, tap it, and remove from combat."
    for obj_id in targets {
        let on_battlefield = state
            .objects
            .get(&obj_id)
            .is_some_and(|o| o.zone == Zone::Battlefield);

        if !on_battlefield {
            continue;
        }

        // CR 701.19: "Can't regenerate" suppresses regeneration-shield creation.
        // The effect itself still resolves (EffectResolved fires below) so any
        // costs paid remain paid, but no shield is installed for this target.
        if crate::game::static_abilities::object_has_static_other(state, obj_id, "CantRegenerate") {
            continue;
        }

        // CR 701.19a: Create a regeneration shield as a replacement definition.
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();

        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.replacement_definitions.push(shield);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn regenerate_creates_shield_on_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(obj.replacement_definitions[0].shield_kind.is_shield());
        assert!(!obj.replacement_definitions[0].is_consumed);
        assert_eq!(
            obj.replacement_definitions[0].event,
            ReplacementEvent::Destroy
        );
    }

    #[test]
    fn regenerate_creates_shield_on_target() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(obj.replacement_definitions[0].shield_kind.is_shield());
    }

    #[test]
    fn regenerate_skips_off_battlefield() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.replacement_definitions.is_empty());
    }

    #[test]
    fn regenerate_self_with_any_and_empty_targets() {
        // "Regenerate this creature" parses to Any with no targeting keyword
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Skeleton".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![], // no explicit targets
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(
            obj.replacement_definitions.len(),
            1,
            "Should create shield on source when targets empty"
        );
    }

    #[test]
    fn cant_regenerate_suppresses_shield_creation() {
        // CR 701.19: "Can't regenerate" suppresses the regeneration-shield
        // replacement. The effect itself still resolves (EffectResolved fires).
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Skeleton".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantRegenerate".to_string()))
                    .affected(TargetFilter::SelfRef),
            );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            obj.replacement_definitions.is_empty(),
            "no regeneration shield should be installed"
        );
        // EffectResolved still fires so any cost paid remains paid.
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Regenerate,
                ..
            }
        )));
    }

    #[test]
    fn regenerate_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Regenerate,
                ..
            }
        )));
    }

    // -------------------------------------------------------------------
    // GH #325 — Blight Mamba regenerate-from-lethal-combat-damage tests.
    // CR 701.19a + CR 614.6 + CR 704.5g: Activated regenerate ability must
    // install a destroy-replacement shield that intercepts the SBA-driven
    // destroy event triggered by lethal combat damage.
    // -------------------------------------------------------------------

    use crate::game::sba::check_state_based_actions;
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::ability::QuantityExpr;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaType, ManaUnit};
    use crate::types::phase::Phase;

    /// Pay {1}{G} to a player's pool — enough to activate Blight Mamba's
    /// regenerate cost without going through the land-tap pipeline.
    fn add_one_g(state: &mut GameState, player: PlayerId) {
        let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for color in [ManaType::Colorless, ManaType::Green] {
            p.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    /// Pass priority until the stack is empty, exhausting attempts on stall.
    fn resolve_stack_to_empty(runner: &mut crate::game::scenario::GameRunner) {
        for _ in 0..40 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
    }

    /// CR 701.19a + CR 614.6 + CR 704.5g: Blight Mamba activates
    /// `{1}{G}: Regenerate this creature.`, then takes 3 damage marked from
    /// a non-infect attacker (lethal vs toughness 1). The SBA destroy event
    /// must be replaced by the regen shield: damage cleared, creature tapped,
    /// shield consumed, creature still on the battlefield.
    #[test]
    fn blight_mamba_regenerate_survives_lethal_combat_damage() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mamba_id = scenario
            .add_creature_from_oracle(
                P0,
                "Blight Mamba",
                1,
                1,
                "Infect\n{1}{G}: Regenerate this creature.",
            )
            .id();

        let mut runner = scenario.build();
        add_one_g(runner.state_mut(), P0);

        // Activate the regenerate ability (index 1; index 0 is the Infect keyword line).
        let ability_index = runner.state().objects[&mamba_id]
            .abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::Regenerate { .. }))
            .expect("Blight Mamba must parse a Regenerate ability from Oracle text");

        runner
            .act(GameAction::ActivateAbility {
                source_id: mamba_id,
                ability_index,
            })
            .expect("activation must succeed");
        resolve_stack_to_empty(&mut runner);

        // Shield must be installed and not yet consumed.
        let mamba = runner.state().objects.get(&mamba_id).unwrap();
        let shield = mamba
            .replacement_definitions
            .iter_all()
            .find(|r| r.shield_kind.is_shield())
            .expect("regen shield must be installed after activation");
        assert!(!shield.is_consumed);
        assert_eq!(shield.event, ReplacementEvent::Destroy);

        // Simulate 3 lethal combat damage marked on the 1/1.
        runner
            .state_mut()
            .objects
            .get_mut(&mamba_id)
            .unwrap()
            .damage_marked = 3;
        let mut events = Vec::new();
        check_state_based_actions(runner.state_mut(), &mut events);

        // CR 704.5g + CR 701.19a: regen shield replaces destruction.
        let mamba = runner.state().objects.get(&mamba_id).unwrap();
        assert!(
            runner.state().battlefield.contains(&mamba_id),
            "Blight Mamba must survive lethal combat damage when regenerated (got zone {:?})",
            mamba.zone,
        );
        assert_eq!(
            mamba.damage_marked, 0,
            "regenerate must clear marked damage"
        );
        assert!(mamba.tapped, "regenerate must tap the permanent");
        assert!(
            mamba
                .replacement_definitions
                .iter_all()
                .any(|r| r.shield_kind.is_shield() && r.is_consumed),
            "regen shield must be marked consumed after firing"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::Regenerated { object_id } if *object_id == mamba_id)
            ),
            "Regenerated event must fire"
        );
    }

    /// CR 701.19a: Only the *next* destroy is replaced. A second lethal-damage
    /// event in the same turn passes through and destroys the permanent.
    #[test]
    fn regenerate_shield_only_consumes_once_per_turn() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();

        let mut runner = scenario.build();

        // Install a regen shield directly (bypass activation — the SBA path
        // is what we're stressing, not the cost flow).
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // First lethal — shield absorbs.
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .damage_marked = 4;
        let mut events = Vec::new();
        check_state_based_actions(runner.state_mut(), &mut events);
        assert!(
            runner.state().battlefield.contains(&bear_id),
            "first lethal damage replaced by shield"
        );

        // Second lethal — shield consumed, no longer replaces.
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .damage_marked = 4;
        let mut events2 = Vec::new();
        check_state_based_actions(runner.state_mut(), &mut events2);
        assert!(
            !runner.state().battlefield.contains(&bear_id),
            "second lethal damage must destroy — shield is one-shot"
        );
    }

    /// CR 701.19 + CR 701.16: Sacrifice is not destruction. A regenerated
    /// creature that is sacrificed still goes to the graveyard — the shield
    /// must NOT intercept the sacrifice event.
    #[test]
    fn regenerate_does_not_save_from_sacrifice() {
        use crate::game::effects::sacrifice;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();

        let mut runner = scenario.build();
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let sac_ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![crate::types::ability::TargetRef::Object(bear_id)],
            bear_id,
            P0,
        );
        let mut events = Vec::new();
        sacrifice::resolve(runner.state_mut(), &sac_ability, &mut events).unwrap();

        assert!(
            !runner.state().battlefield.contains(&bear_id),
            "sacrifice (CR 701.16) bypasses regeneration (CR 701.19) — creature must die"
        );
        assert!(
            runner.state().players[0].graveyard.contains(&bear_id),
            "sacrificed creature must reach owner's graveyard"
        );
    }

    /// CR 701.19a: Regenerate parsed from real Oracle text resolves into a
    /// shield with `valid_card: SelfRef`. Building-block test guarding that
    /// the parser → resolver lowering does not drift.
    #[test]
    fn regenerate_parsed_from_oracle_emits_self_ref_shield() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let card_id = scenario
            .add_creature_from_oracle(
                P0,
                "Drudge Skeletons",
                1,
                1,
                "{B}: Regenerate this creature.",
            )
            .id();

        let mut runner = scenario.build();

        // Add the right mana to pay {B}.
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool
            .add(ManaUnit {
                color: ManaType::Black,
                source_id: ObjectId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });

        let regen_idx = runner.state().objects[&card_id]
            .abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::Regenerate { .. }))
            .expect("Regenerate must parse from Oracle text");

        runner
            .act(GameAction::ActivateAbility {
                source_id: card_id,
                ability_index: regen_idx,
            })
            .expect("activation must succeed");
        resolve_stack_to_empty(&mut runner);

        let obj = runner.state().objects.get(&card_id).unwrap();
        let shield = obj
            .replacement_definitions
            .iter_all()
            .find(|r| r.shield_kind.is_shield())
            .expect("Regenerate must install a shield");
        assert_eq!(
            shield.valid_card,
            Some(TargetFilter::SelfRef),
            "shield must be scoped to the source via SelfRef so SBA destroy on the source matches"
        );
        assert_eq!(shield.event, ReplacementEvent::Destroy);
    }

    /// CR 701.19b: Regen shield expires at cleanup step (end of turn).
    /// A consumed shield AND an unconsumed shield from the same turn both
    /// disappear during cleanup so they can't carry over.
    #[test]
    fn regenerate_shield_expires_at_cleanup() {
        use crate::game::turns::execute_cleanup;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear_id = scenario.add_creature(P0, "Bear", 2, 2).id();

        let mut runner = scenario.build();
        let unused = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        let mut consumed = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        consumed.is_consumed = true;
        {
            let defs = &mut runner
                .state_mut()
                .objects
                .get_mut(&bear_id)
                .unwrap()
                .replacement_definitions;
            defs.push(unused);
            defs.push(consumed);
        }

        let mut events = Vec::new();
        execute_cleanup(runner.state_mut(), &mut events);

        let obj = runner.state().objects.get(&bear_id).unwrap();
        assert!(
            obj.replacement_definitions
                .iter_all()
                .all(|r| !r.shield_kind.is_shield()),
            "all shields (consumed or fresh) must be pruned at cleanup"
        );
    }

    /// Building-block guard: the resolver MUST install a `Destroy`-event
    /// shield with `valid_card = SelfRef` regardless of whether the typed
    /// Effect target is `SelfRef` or `Any` with empty target list.
    #[test]
    fn regenerate_self_branches_emit_identical_shield_shape() {
        // Branch 1: target = SelfRef, no targets list.
        let mut state_a = GameState::new_two_player(42);
        let id_a = create_object(
            &mut state_a,
            CardId(1),
            P0,
            "A".to_string(),
            Zone::Battlefield,
        );
        state_a
            .objects
            .get_mut(&id_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        resolve(
            &mut state_a,
            &ResolvedAbility::new(
                Effect::Regenerate {
                    target: TargetFilter::SelfRef,
                },
                vec![],
                id_a,
                P0,
            ),
            &mut Vec::new(),
        )
        .unwrap();

        // Branch 2: target = Any with empty targets — the parser's "this
        // creature" pathway falls into this shape.
        let mut state_b = GameState::new_two_player(42);
        let id_b = create_object(
            &mut state_b,
            CardId(1),
            P0,
            "B".to_string(),
            Zone::Battlefield,
        );
        state_b
            .objects
            .get_mut(&id_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        resolve(
            &mut state_b,
            &ResolvedAbility::new(
                Effect::Regenerate {
                    target: TargetFilter::Any,
                },
                vec![],
                id_b,
                P0,
            ),
            &mut Vec::new(),
        )
        .unwrap();

        let shield_a = state_a.objects[&id_a]
            .replacement_definitions
            .iter_all()
            .find(|r| r.shield_kind.is_shield())
            .expect("branch A shield");
        let shield_b = state_b.objects[&id_b]
            .replacement_definitions
            .iter_all()
            .find(|r| r.shield_kind.is_shield())
            .expect("branch B shield");

        // The two parser-input branches must produce identical replacement
        // shapes — anything else means the building block has drifted.
        assert_eq!(shield_a.event, shield_b.event);
        assert_eq!(shield_a.valid_card, shield_b.valid_card);
        assert_eq!(shield_a.shield_kind, shield_b.shield_kind);
    }

    /// Keyword::Indestructible interacts with regen: the shield is
    /// applicable but never *needed*. Ensure both shields and Indestructible
    /// can co-exist without panicking and without spurious shield consumption.
    #[test]
    fn indestructible_creature_with_regen_shield_does_not_consume_shield() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear_id = scenario.add_creature(P0, "Avacyn", 8, 8).id();

        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        runner
            .state_mut()
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .damage_marked = 99;
        let mut events = Vec::new();
        check_state_based_actions(runner.state_mut(), &mut events);

        let obj = runner.state().objects.get(&bear_id).unwrap();
        assert!(
            runner.state().battlefield.contains(&bear_id),
            "indestructible creature survives lethal damage without consuming shield"
        );
        assert!(
            !obj.replacement_definitions
                .iter_all()
                .any(|r| r.is_consumed),
            "shield must NOT be consumed when Indestructible already prevents the destruction"
        );
    }
}
