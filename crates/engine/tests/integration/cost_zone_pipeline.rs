use engine::database::synthesis::synthesize_plot;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::AttachTarget;
use engine::game::mana_abilities::activate_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, CastingPermission, ChoiceType,
    DiscardSelfScope, Effect, ManaContribution, ManaProduction, ModalChoice, QuantityExpr,
    ReplacementDefinition, ReplacementMode, ResolvedAbility, SacrificeCost, SpellCastingOption,
    TargetFilter, TargetRef, TargetSelectionMode, TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaAbilityCostParentLifecycle, ManaAbilityCostResolutionMode,
    ManaAbilityResume, PayCostKind, PendingCast, PendingCostMoveResume, PendingReplacement,
    StackEntryKind, WaitingFor,
};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::proposed_event::{ProposedEvent, ReplacementId};
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use std::sync::Arc;

fn redirect_moved_to(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: redirected_to,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

fn prompt_after_moved_to_exile() -> ReplacementDefinition {
    redirect_moved_to_with_post_effect(Zone::Exile, Zone::Exile)
}

fn scry_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    replacement
}

fn proliferate_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Proliferate,
    )));
    replacement
}

fn optional_gain_life_after_moved_to_exile() -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(Zone::Exile, Zone::Exile);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )
        .optional(),
    ));
    replacement
}

fn redirect_moved_to_with_post_effect(
    destination: Zone,
    redirected_to: Zone,
) -> ReplacementDefinition {
    let mut replacement = redirect_moved_to(destination, redirected_to);
    replacement
        .execute
        .as_mut()
        .expect("redirect helper always provides its replacement effect")
        .sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::Labeled {
                options: vec!["first".to_string(), "second".to_string()],
            },
            persist: false,
            selection: TargetSelectionMode::Chosen,
        },
    )));
    replacement
}

fn mana_self_exile_cost_redirect_witness() -> (GameScenario, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana Self-Exile Redirect Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Mana Self-Exile Redirect",
        "Second Mana Self-Exile Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    (scenario, source)
}

/// Drives the real replacement-choice dispatcher through its `Prevented` arm
/// while retaining the paused mana-cost root created by the normal cost-move
/// pipeline. Zone-change prevention is not yet an engine replacement outcome,
/// so this uses the existing one-shot prevention producer to exercise the
/// shared dispatcher seam.
fn stage_prevented_mana_cost_move(
    state: &mut GameState,
    source: engine::types::identifiers::ObjectId,
) {
    state
        .objects
        .get_mut(&source)
        .expect("mana source exists while its cost move is paused")
        .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
        .regeneration_shield()
        .description("Prevent the staged mana cost move".to_string())]
    .into();
    state.pending_replacement = Some(PendingReplacement {
        proposed: ProposedEvent::Destroy {
            object_id: source,
            source: None,
            cant_regenerate: false,
            applied: Default::default(),
        },
        sacrifice_provenance: None,
        candidates: vec![ReplacementId { source, index: 0 }],
        search_found_candidates: Vec::new(),
        depth: 0,
        is_optional: false,
        library_placement: None,
        excess_recipient: None,
        lifelink_bonus: 0,
        may_cost_paid: false,
        may_cost_remaining: None,
    });
    state.waiting_for = WaitingFor::ReplacementChoice {
        player: P0,
        candidate_count: 1,
        candidates: vec![],
    };
}

#[test]
fn foretell_cost_honors_moved_redirect_and_completes_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Cost Redirect Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    for name in ["First Foretell Redirect", "Second Foretell Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let result = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("foretell special action should pay its cost and consult Moved replacements");

    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the foretell cost move must consult competing Moved redirects"
    );

    let turn_foretold = runner.state().turn_number;
    let json = serde_json::to_string(runner.state()).expect("paused foretell serializes");
    let restored: GameState = serde_json::from_str(&json).expect("paused foretell deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume.as_ref(),
        Some(&PendingCostMoveResume::Foretell {
            player,
            object_id,
            ref cost,
            turn_foretold: stamped_turn,
        }) if player == P0 && object_id == foretold && cost == &foretell_cost && stamped_turn == turn_foretold
    ));
    let mut runner = GameRunner::from_state(restored);

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the foretell exile");
    let obj = &runner.state().objects[&foretold];
    assert_eq!(obj.zone, Zone::Graveyard);
    assert!(!obj.foretold, "only a card delivered to exile was foretold");
    assert!(
        !obj.face_down,
        "a redirected card must not gain foretell concealment"
    );
    assert!(obj.casting_permissions.is_empty());
    assert!(
        !result.events.iter().any(
            |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
        ),
        "a redirected card must not emit Foretold"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the selected redirect must apply exactly once"
    );
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn foretell_delivery_finalizes_before_a_post_replacement_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Post-Effect Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    scenario
        .add_creature(P0, "Foretell Post-Effect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(prompt_after_moved_to_exile());
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let turn_foretold = runner.state().turn_number;
    let result = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("foretell should deliver before the replacement post-effect prompts");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::NamedChoice { ref options, .. }
                if options == &vec!["first".to_string(), "second".to_string()]
        ),
        "the delivered Foretell move must preserve the replacement prompt"
    );
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert!(matches!(
        object.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold: stamped_turn }]
            if cost == &foretell_cost && *stamped_turn == turn_foretold
    ));
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold))
            .count(),
        1,
        "delivery must emit exactly one Foretold event before the prompt pauses"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the identity redirect must apply before its post-effect prompts"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let paused_waiting_for = runner.state().waiting_for.clone();
    let json =
        serde_json::to_string(runner.state()).expect("post-delivery foretell pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("post-delivery foretell pause deserializes");
    assert_eq!(restored.waiting_for, paused_waiting_for);
    assert!(restored.pending_cost_move_resume.is_none());
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("post-replacement choice should remain actionable after serialization");
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert_eq!(object.casting_permissions.len(), 1);
    assert!(
        !resumed.events.iter().any(
            |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
        ),
        "resolving the post-effect must not re-finalize Foretell"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn foretell_replacement_pause_then_post_effect_prompt_finalizes_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foretell_cost = ManaCost::generic(5);
    let foretold = scenario
        .add_spell_to_hand(P0, "Foretell Replacement Resume Witness", false)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Foretell(foretell_cost.clone()))
        .id();
    let exile_to_graveyard = scenario
        .add_creature(P0, "Foretell Exile to Graveyard", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard))
        .id();
    scenario
        .add_creature(P0, "Foretell Exile to Exile", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Exile));
    let graveyard_to_exile = scenario
        .add_creature(P0, "Foretell Graveyard to Exile", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to_with_post_effect(
            Zone::Graveyard,
            Zone::Exile,
        ))
        .id();
    scenario
        .add_creature(P0, "Foretell Graveyard to Hand", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Graveyard, Zone::Hand));
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&foretold].card_id;
    let turn_foretold = runner.state().turn_number;
    let initial = runner
        .act(GameAction::Foretell {
            object_id: foretold,
            card_id,
        })
        .expect("competing Moved replacements should pause the Foretell cost move");
    assert!(matches!(
        initial.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json =
        serde_json::to_string(runner.state()).expect("pre-delivery foretell pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("pre-delivery foretell pause deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume.as_ref(),
        Some(&PendingCostMoveResume::Foretell {
            player,
            object_id,
            ref cost,
            turn_foretold: stamped_turn,
        }) if player == P0 && object_id == foretold && cost == &foretell_cost && stamped_turn == turn_foretold
    ));
    let mut runner = GameRunner::from_state(restored);

    let mut replacement_prompts = 0;
    let mut delivered = None;
    while let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    {
        let expected_source = match replacement_prompts {
            0 => exile_to_graveyard,
            1 => graveyard_to_exile,
            _ => panic!("unexpected additional Foretell replacement prompt"),
        };
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == expected_source)
            .expect("the chosen redirect must appear in its CR 616.1 ordering prompt");
        delivered = Some(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("apply the selected Foretell redirect"),
        );
        replacement_prompts += 1;
    }
    assert_eq!(
        replacement_prompts, 2,
        "both material Moved replacement collisions must be ordered before delivery"
    );
    let delivered = delivered.expect("the selected graveyard-to-exile redirect must deliver");
    assert!(matches!(
        delivered.waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    let object = &runner.state().objects[&foretold];
    assert_eq!(object.zone, Zone::Exile);
    assert!(object.foretold);
    assert!(object.face_down);
    assert!(matches!(
        object.casting_permissions.as_slice(),
        [CastingPermission::Foretold { cost, turn_foretold: stamped_turn }]
            if cost == &foretell_cost && *stamped_turn == turn_foretold
    ));
    assert_eq!(
        delivered
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold))
            .count(),
        1
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("the post-effect prompt remains actionable after Foretell completes");
    assert!(!resumed.events.iter().any(
        |event| matches!(event, GameEvent::Foretold { object_id, .. } if *object_id == foretold)
    ));
    assert_eq!(
        runner.state().objects[&foretold].casting_permissions.len(),
        1
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn pitch_exile_cost_honors_moved_redirect_and_completes_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let shoal = scenario
        .add_creature_to_hand(P0, "Nourishing Shoal", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .with_ability(Effect::GainLife {
            amount: engine::types::ability::QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            player: TargetFilter::Controller,
        })
        .id();
    let pitched = scenario.add_creature_to_hand(P0, "Green Filler", 2, 2).id();
    scenario
        .add_creature(P0, "Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let shoal_obj = state.objects.get_mut(&shoal).expect("shoal exists");
        shoal_obj
            .casting_options
            .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
                "exile a green card with mana value X from your hand",
            )));
        shoal_obj.color.push(ManaColor::Green);

        let pitched_obj = state
            .objects
            .get_mut(&pitched)
            .expect("pitched card exists");
        pitched_obj.card_types.core_types.push(CoreType::Creature);
        pitched_obj.color.push(ManaColor::Green);
        pitched_obj.mana_cost = ManaCost::generic(3);
    }
    let card_id = runner.state().objects[&shoal].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: shoal,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Nourishing Shoal");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept pitch cost");

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![pitched],
        })
        .expect("pay pitch exile cost");

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Hand),
                to: Zone::Graveyard,
                ..
            } if *object_id == pitched
        )),
        "the redirect must modify the pitch cost's exile event"
    );
    assert_eq!(runner.state().objects[&pitched].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after the redirected pitch cost"
    );
}

#[test]
fn multi_card_exile_cost_resumes_after_each_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_creature_to_hand(P0, "Two-card Pitch Witness", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "First Green Filler", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second Green Filler", 2, 2)
        .id();
    scenario
        .add_creature(P0, "First Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    scenario
        .add_creature(P0, "Second Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));

    let mut runner = scenario.build();
    {
        let spell_obj = runner
            .state_mut()
            .objects
            .get_mut(&spell)
            .expect("spell exists");
        spell_obj
            .casting_options
            .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
                "exile two green cards from your hand",
            )));
        for object_id in [first, second] {
            let filler = runner
                .state_mut()
                .objects
                .get_mut(&object_id)
                .expect("green filler exists");
            filler.card_types.core_types.push(CoreType::Creature);
            filler.color.push(ManaColor::Green);
        }
    }
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast two-card pitch witness");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept two-card pitch cost");
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .expect("select both green cards");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let mut prompts_answered = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the cost-move replacement choice");
        prompts_answered += 1;
        assert!(prompts_answered <= 2, "each selected card pauses once");
    }

    assert_eq!(
        prompts_answered, 2,
        "resume must continue with the next card"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after both replacement choices"
    );
}

#[test]
fn return_to_hand_cost_honors_moved_redirect_and_completes_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_creature_to_hand(P0, "Daze Cost Witness", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let returned_land = scenario.add_basic_land(P0, ManaColor::Blue);
    scenario
        .add_creature(P0, "Hand Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Exile));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell exists")
        .casting_options
        .push(SpellCastingOption::alternative_cost(parse_oracle_cost(
            "Return a land you control to its owner's hand",
        )));
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Daze cost witness");
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept return-to-hand cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![returned_land],
        })
        .expect("pay return-to-hand cost");

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if *object_id == returned_land
        )),
        "the redirect must modify the return-to-hand cost event"
    );
    assert_eq!(runner.state().objects[&returned_land].zone, Zone::Exile);
    assert!(
        !runner.state().stack.is_empty(),
        "the cast must complete after the redirected return-to-hand cost"
    );
}

#[test]
fn self_exile_activation_cost_pauses_for_moved_redirect_without_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Self-Exile Cost Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            }),
        )
        .id();
    for name in ["First Self-Exile Redirect", "Second Self-Exile Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce self-exile activation");

    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().pending_cast.is_none(),
        "a self-exile activation cost must not use PendingCast to resume"
    );

    let json = serde_json::to_string(runner.state()).expect("paused cost move serializes");
    assert!(
        json.contains("pending_cost_move_resume"),
        "a replacement choice must retain its cost-move continuation on the wire"
    );
    let restored: GameState = serde_json::from_str(&json).expect("paused cost move deserializes");
    assert!(matches!(
        restored.pending_cost_move_resume,
        Some(PendingCostMoveResume::Cast {
            pending: Some(_),
            ..
        })
    ));
    let mut runner = GameRunner::from_state(restored);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply self-exile redirect");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the activation must finish after the redirected self-exile cost"
    );
}

#[test]
fn mimeoplasm_forced_exile_cost_resumes_after_redirects_and_tracks_delivered_exiles_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let first = scenario
        .add_creature_to_graveyard(P0, "First Mimeoplasm Witness", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Second Mimeoplasm Witness", 3, 3)
        .id();
    let mimeoplasm = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Mimeoplasm Forced-Cost Witness",
            5,
            5,
            "As ~ enters, you may exile two creature cards from graveyards. If you do, ~ enters as a copy of one of them, except it has +1/+1 counters equal to the other's power.",
        )
        .id();
    for name in ["First Mimeoplasm Redirect", "Second Mimeoplasm Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    assert!(runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&first));
    assert!(runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&second));
    let mut forced_cost_only =
        runner.state().objects[&mimeoplasm].replacement_definitions[0].clone();
    assert!(matches!(
        &forced_cost_only.mode,
        ReplacementMode::MayCost {
            cost: AbilityCost::Exile { count: 2, .. },
            ..
        }
    ));
    // The printed Oracle parse is the coverage pin. Strip only its independent
    // copy/counter branch so this witness isolates the exact typed two-card MayCost.
    forced_cost_only.execute = None;
    runner
        .state_mut()
        .objects
        .get_mut(&mimeoplasm)
        .expect("Mimeoplasm witness exists")
        .replacement_definitions = vec![forced_cost_only].into();
    runner.cast(mimeoplasm).resolve();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Mimeoplasm's replacement cost");

    let state = runner.state();
    let Some(PendingCostMoveResume::ReplacementMayCost { remaining, .. }) =
        state.pending_cost_move_resume.as_ref()
    else {
        panic!("the first Mimeoplasm exile must retain its one-card cost tail");
    };
    assert_eq!(remaining.len(), 1);
    let pending = state
        .pending_replacement
        .as_ref()
        .expect("the first inner exile must own its replacement prompt");
    assert_eq!(pending.candidates.len(), 2);
    assert!(matches!(
        &pending.proposed,
        ProposedEvent::ZoneChange {
            from: Zone::Graveyard,
            to: Zone::Exile,
            ..
        }
    ));
    assert!(
        state
            .pending_spell_resolution
            .as_ref()
            .is_some_and(|ctx| ctx.object_id == mimeoplasm),
        "the outer permanent-spell resolution must survive the inner cost prompt"
    );

    for prompt in 0..2 {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "expected replacement choice for inner cost move {prompt}, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("apply the forced Mimeoplasm cost redirect");
        if prompt == 0 {
            assert!(
                runner.state().pending_cost_move_resume.is_some(),
                "the first redirected exile must retain the second inner cost move"
            );
            assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&mimeoplasm].zone, Zone::Stack);
            assert!(
                runner
                    .state()
                    .pending_spell_resolution
                    .as_ref()
                    .is_some_and(|ctx| ctx.object_id == mimeoplasm),
                "an inner cost redirect must not consume the outer spell-resolution context"
            );
        } else {
            assert!(
                runner.state().pending_cost_move_resume.is_none(),
                "both forced cost moves must finish before the outer replacement re-enters"
            );
        }
    }

    let state = runner.state();
    assert_eq!(state.objects[&first].zone, Zone::Hand);
    assert_eq!(state.objects[&second].zone, Zone::Hand);
    assert!(
        state
            .cards_exiled_with_source_this_turn
            .get(&mimeoplasm)
            .is_none_or(Vec::is_empty),
        "only cards delivered to exile may be indexed as exiled with Mimeoplasm"
    );
    assert!(
        state
            .exile_links
            .iter()
            .all(|link| link.source_id != mimeoplasm),
        "Mimeoplasm's cost must not create a persistent ExileLink"
    );
    assert_eq!(state.objects[&mimeoplasm].zone, Zone::Battlefield);
    assert!(
        state.pending_spell_resolution.is_none(),
        "the outer context is consumed only when Mimeoplasm's own entry completes"
    );
}

#[test]
fn self_return_activation_cost_pauses_for_moved_redirect_without_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Self-Return Cost Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::SelfRef),
                from_zone: None,
            }),
        )
        .id();
    for name in ["First Self-Return Redirect", "Second Self-Return Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Hand, Zone::Exile));
    }

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce self-return activation");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                ..
            }
        ),
        "self-return activation should select its return cost before moving: {:?}",
        result.waiting_for
    );
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
        .expect("select the self-return cost");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        runner.state().pending_cast.is_none(),
        "a self-return activation cost must not use PendingCast to resume"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply self-return redirect");

    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(
        !runner.state().stack.is_empty(),
        "the redirected return-to-hand cost must finish the activation"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
}

#[test]
fn composite_return_cost_resurfaces_each_return_leg() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Two Returns Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                ],
            }),
        )
        .id();
    let first = scenario.add_basic_land(P0, ManaColor::Blue);
    let second = scenario
        .add_creature(P0, "Second Return Witness", 1, 1)
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate two-return witness");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("pay first return leg");
    assert_eq!(runner.state().objects[&first].zone, Zone::Hand);
    assert!(
        runner.state().objects[&source].tapped,
        "automatic tap leg is paid once"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("pay second return leg");
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert!(
        !runner.state().stack.is_empty(),
        "both return legs must complete before the activation reaches the stack"
    );
}

#[test]
fn return_cost_keeps_selected_move_while_residual_self_move_pauses() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Residual Self-Move Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: None,
                        from_zone: None,
                    },
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            }),
        )
        .id();
    let returned = scenario.add_basic_land(P0, ManaColor::Blue);
    for name in ["First Residual Redirect", "Second Residual Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate residual self-move witness");
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![returned],
        })
        .expect("select return before residual self-exile");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::Cast { .. })
    ));
    assert_eq!(runner.state().objects[&returned].zone, Zone::Battlefield);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect residual self-exile");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 2,
        "the automatic PayLife suffix must resume exactly once before the selected return"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "the selected return must finish after the paused automatic self-move"
    );
}

#[test]
fn modal_activation_self_exile_cost_resumes_after_moved_redirect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Modal Self-Exile Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            })
            .with_modal(
                ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 1,
                    mode_descriptions: vec!["Gain life".to_string()],
                    ..ModalChoice::default()
                },
                vec![AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )],
            ),
        )
        .id();
    for name in ["First Modal Redirect", "Second Modal Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce modal activation");
    let result = runner
        .act(GameAction::SelectModes { indices: vec![0] })
        .expect("select the only mode");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect modal activation self-exile cost");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(
        !runner.state().stack.is_empty(),
        "the modal activation must reach the stack after its redirected cost completes"
    );
}

#[test]
fn synthesized_plot_redirect_resumes_as_special_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plotted = scenario
        .add_creature_to_hand(P0, "Synthesized Plot Redirect Witness", 1, 1)
        .id();
    for name in ["First Plot Redirect", "Second Plot Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    let mut face = CardFace::default();
    face.keywords.push(Keyword::Plot(ManaCost::generic(0)));
    synthesize_plot(&mut face);
    let object = runner
        .state_mut()
        .objects
        .get_mut(&plotted)
        .expect("plot witness exists");
    object.keywords = face.keywords.clone();
    object.base_keywords = face.keywords.clone();
    *Arc::make_mut(&mut object.abilities) = face.abilities.clone();
    *Arc::make_mut(&mut object.base_abilities) = face.abilities;

    let first = runner
        .act(GameAction::ActivateAbility {
            source_id: plotted,
            ability_index: 0,
        })
        .expect("start synthesized plot special action");
    assert!(matches!(
        first.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let second = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect plotted self-exile");

    assert_eq!(runner.state().objects[&plotted].zone, Zone::Graveyard);
    assert!(runner.state().objects[&plotted]
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::Plotted { .. })));
    assert!(
        runner.state().stack.is_empty(),
        "plot must never use the stack"
    );
    assert!(
        first
            .events
            .iter()
            .chain(second.events.iter())
            .all(|event| !matches!(event, GameEvent::AbilityActivated { .. })),
        "plot is a special action and must not emit AbilityActivated"
    );
}

#[test]
fn mana_self_exile_cost_redirect_serializes_and_resumes_mana_payment_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&source].card_id;
    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        source,
        card_id,
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Mana Payment Witness".to_string(),
                description: None,
            },
            vec![],
            source,
            P0,
        ),
        ManaCost::generic(1),
    )));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    let initial = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mana ability activation should reach its self-exile cost");

    assert!(
        matches!(initial, WaitingFor::ReplacementChoice { .. }),
        "a mana self-exile cost must consult competing Moved redirects"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment {
            pending,
            cursor,
        }) if matches!(&pending.resume, ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        })
            && cursor.remaining.is_empty()
    ));
    let json = serde_json::to_string(runner.state())
        .expect("a paused mana self-exile replacement choice serializes");
    assert!(
        json.contains("ReplacementChoice"),
        "the replacement choice must remain serialized while mana payment is paused"
    );
    let restored: GameState = serde_json::from_str(&json)
        .expect("a paused mana self-exile replacement choice deserializes");
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the mana self-exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed activation must produce its mana exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the cost move must not repay the earlier tap component"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(
                |event| matches!(event, GameEvent::ManaAdded { player_id, .. } if *player_id == P0)
            )
            .count(),
        1,
        "the resumed activation must not produce mana twice"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player,
            convoke_mode: None,
        } if player == P0
    ));
}

#[test]
fn mana_self_exile_cost_redirect_serializes_and_resumes_unless_payment_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let unless_cost = AbilityCost::Mana {
        cost: ManaCost::generic(1),
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    let resume = ManaAbilityResume::UnlessPayment {
        outer_player: Some(P0),
        cost: Box::new(unless_cost.clone()),
        pending_effect: Box::new(pending_effect.clone()),
        trigger_event: None,
        effect_description: Some("unless payment witness".to_string()),
        remaining: vec![P1],
    };
    let mut initial_events = Vec::new();
    let initial = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        resume,
        None,
    )
    .expect("the mana ability activation should reach its self-exile cost");

    assert!(matches!(initial, WaitingFor::ReplacementChoice { .. }));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment {
            pending,
            cursor,
        }) if matches!(
            &pending.resume,
            ManaAbilityResume::UnlessPayment {
                outer_player: Some(P0),
                cost,
                pending_effect: paused_effect,
                trigger_event: None,
                effect_description: Some(description),
                remaining,
            } if cost.as_ref() == &unless_cost
                && paused_effect.as_ref() == &pending_effect
                && description == "unless payment witness"
                && remaining == &vec![P1]
        ) && cursor.remaining.is_empty()
    ));

    let json = serde_json::to_string(runner.state())
        .expect("a paused unless-payment mana activation serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("a paused unless-payment mana activation deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the mana self-exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed activation must produce its mana exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the cost move must not repay the earlier tap component"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
    match &resumed.waiting_for {
        WaitingFor::UnlessPayment {
            player,
            cost,
            pending_effect: resumed_effect,
            trigger_event,
            effect_description,
            remaining,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(cost, &unless_cost);
            assert_eq!(resumed_effect.as_ref(), &pending_effect);
            assert!(trigger_event.is_none());
            assert_eq!(
                effect_description.as_deref(),
                Some("unless payment witness")
            );
            assert_eq!(remaining, &vec![P1]);
        }
        other => panic!("expected exact UnlessPayment resume, got {other:?}"),
    }
}

#[test]
fn auto_tap_cost_move_redirect_preserves_outer_mana_payment() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Auto-Tap Cost-Move Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    let announced = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("manual spell payment should reach its mana window");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    let paused = runner
        .act(GameAction::PassPriority)
        .expect("auto-tap must surface a mana source's replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor })
            if matches!(pending.resume, ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            })
                && cursor.remaining.is_empty()
    ));
    assert!(runner.state().pending_cast.is_some());
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the spell's mana cost must not be spent before the source move settles"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the auto-payment replacement pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the auto-payment replacement pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the auto-tapped mana source's exile cost");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player,
            convoke_mode: None,
        } if player == P0
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the source resumes once and leaves its mana available to the outer payment"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the outer spell payment resumes after the source move");
    assert!(matches!(completed.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "resuming the outer payment must not repay the mana source's tap prefix"
    );
}

#[test]
fn auto_tap_cost_move_redirect_preserves_outer_unless_payment() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let unless_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            },
            AbilityCost::Mana {
                cost: ManaCost::generic(1),
            },
        ],
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Auto-Tap Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    runner.state_mut().waiting_for = WaitingFor::UnlessPayment {
        player: P0,
        cost: unless_cost.clone(),
        pending_effect: Box::new(pending_effect.clone()),
        trigger_event: None,
        effect_description: Some("auto-tap unless payment witness".to_string()),
        remaining: vec![P1],
    };

    let paused = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("auto-tap must preserve an unless payment while the source move pauses");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) if matches!(
            &pending.resume,
            ManaAbilityResume::UnlessPayment {
                outer_player: Some(P0),
                cost,
                pending_effect: paused_effect,
                trigger_event: None,
                effect_description: Some(description),
                remaining,
            } if cost.as_ref() == &unless_cost
                && paused_effect.as_ref() == &pending_effect
                && description == "auto-tap unless payment witness"
                && remaining == &vec![P1]
        ) && cursor.remaining.is_empty()
            && cursor.resolution_mode == ManaAbilityCostResolutionMode::AutoResolved
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the colored prefix may be produced, but the unsettled generic source must prevent spending the unless cost"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the auto unless-payment replacement pause serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the auto unless-payment replacement pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the auto-tapped source's exile cost");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::UnlessPayment {
            player,
            ref cost,
            ref remaining,
            ..
        } if player == P0 && cost == &unless_cost && remaining == &vec![P1]
    ));

    let paid = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the restored unless payment should spend the resumed mana");
    assert!(matches!(paid.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

#[test]
fn effect_pay_cost_auto_tap_redirect_serializes_exact_cost_and_trailing_effect_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::Mana { cost: cost.clone() },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let starting_life = runner.state().players[P0.0 as usize].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("effect payment should pause only for the replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost {
                payer: P0,
                ability: paused_ability,
                cost: paused_cost,
                ..
            } if paused_ability.as_ref() == &ability
                && paused_cost.as_ref() == &AbilityCost::Mana { cost: cost.clone() }
        )
    ));

    let json =
        serde_json::to_string(runner.state()).expect("paused effect-cost mana payment serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("paused effect-cost mana payment deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected source cost resumes the exact outer effect cost");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        starting_life + 1,
        "the trailing effect must resume exactly once after the outer cost is paid"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn effect_pay_cost_rider_waits_for_scry_post_effect_before_typed_root_settles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Effect PayCost Scry Ordering Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let scry_card = scenario.add_card_to_library_top(P0, "Effect PayCost Scry Ordering Card");
    scenario
        .add_creature(P0, "Effect PayCost Scry Ordering Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(scry_after_moved_to_exile());

    let mut runner = scenario.build();
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));
    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("effect PayCost reaches its source-cost replacement post-effect");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ScryChoice { player: P0, ref cards } if cards == &vec![scry_card]
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(pending.resume, ManaAbilityResume::EffectPayCost { .. })
    ));
    assert!(
        runner.state().pending_continuation.is_none(),
        "only replacement post-effect work may drain before the typed Effect::PayCost root"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before);

    let json = serde_json::to_string(runner.state())
        .expect("the interactive post-effect and typed EffectPayCost root serialize together");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the interactive post-effect and typed EffectPayCost root deserialize together");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectCards {
            cards: vec![scry_card],
        })
        .expect("settling Scry completes the typed root before releasing the PayCost rider");

    let mana_added = resumed
        .events
        .iter()
        .position(
            |event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source),
        )
        .expect("the source produces its mana while the typed root settles");
    let rider_life = resumed
        .events
        .iter()
        .position(|event| matches!(event, GameEvent::LifeChanged { player_id, amount } if *player_id == P0 && *amount == 1))
        .expect("the trailing PayCost rider resolves once");
    assert!(
        mana_added < rider_life,
        "the trailing PayCost rider must remain parked until the Scry post-effect and typed mana root complete"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
    assert!(runner.state().pending_cost_move_resume.is_none());
}

fn assert_repeated_interactive_activation_cost(
    mut runner: GameRunner,
    source: engine::types::identifiers::ObjectId,
    chosen: [engine::types::identifiers::ObjectId; 2],
    one_of: bool,
    expected_kind: impl Fn(&PayCostKind) -> bool,
) {
    let activated = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the activation starts its interactive cost payment");
    if one_of {
        assert!(matches!(
            activated.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ));
        runner
            .act(GameAction::ChooseActivationCostBranch { index: 0 })
            .expect("the only disjunctive cost branch is payable");
    } else {
        assert!(matches!(
            activated.waiting_for,
            WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
        ));
    }

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen[0]],
        })
        .expect("the first repeated interactive cost leg is paid");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { ref kind, .. } if expected_kind(kind)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen[1]],
        })
        .expect("the second repeated interactive cost leg is paid");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the activation reaches the stack only after both selected cost legs"
    );
}

fn repeated_discard_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let discard = AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    };
    let cost = AbilityCost::Composite {
        costs: vec![discard.clone(), discard],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Discard Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario.add_card_to_hand(P0, "First Repeated Discard Witness");
    let second = scenario.add_card_to_hand(P0, "Second Repeated Discard Witness");
    (scenario.build(), source, [first, second])
}

fn repeated_sacrifice_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let sacrifice = AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        1,
    ));
    let cost = AbilityCost::Composite {
        costs: vec![sacrifice.clone(), sacrifice],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Sacrifice Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Repeated Sacrifice Witness", 1, 1)
        .as_artifact()
        .id();
    let second = scenario
        .add_creature(P0, "Second Repeated Sacrifice Witness", 1, 1)
        .as_artifact()
        .id();
    (scenario.build(), source, [first, second])
}

fn repeated_exile_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let exile = AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Hand),
        filter: None,
    };
    let cost = AbilityCost::Composite {
        costs: vec![exile.clone(), exile],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Exile Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario.add_card_to_hand(P0, "First Repeated Exile Witness");
    let second = scenario.add_card_to_hand(P0, "Second Repeated Exile Witness");
    (scenario.build(), source, [first, second])
}

fn repeated_unattach_activation_witness(
    one_of: bool,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let unattach = AbilityCost::UnattachFrom {
        filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        count: 1,
    };
    let cost = AbilityCost::Composite {
        costs: vec![unattach.clone(), unattach],
    };
    let cost = if one_of {
        AbilityCost::OneOf { costs: vec![cost] }
    } else {
        cost
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Unattach Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost),
        )
        .id();
    let first = scenario
        .add_creature(P0, "First Repeated Unattach Witness", 0, 1)
        .as_artifact()
        .id();
    let second = scenario
        .add_creature(P0, "Second Repeated Unattach Witness", 0, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    for attachment in [first, second] {
        runner
            .state_mut()
            .objects
            .get_mut(&attachment)
            .unwrap()
            .attached_to = Some(AttachTarget::Object(source));
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .attachments
            .push(attachment);
    }
    (runner, source, [first, second])
}

#[test]
fn repeated_and_one_of_interactive_activation_costs_surface_each_unpaid_leg() {
    for one_of in [false, true] {
        let (runner, source, chosen) = repeated_discard_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::Discard)
        });

        let (runner, source, chosen) = repeated_sacrifice_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::Sacrifice)
        });

        let (runner, source, chosen) = repeated_exile_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::ExileFromZone { .. })
        });

        let (runner, source, chosen) = repeated_unattach_activation_witness(one_of);
        assert_repeated_interactive_activation_cost(runner, source, chosen, one_of, |kind| {
            matches!(kind, PayCostKind::UnattachFrom { .. })
        });
    }
}

#[test]
fn mana_selected_exile_cost_redirect_resumes_after_the_paid_prefix_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Mana Selected-Exile Redirect Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Battlefield),
                        filter: None,
                    },
                ],
            }),
        )
        .id();
    let selected = scenario
        .add_creature(P0, "Selected Exile Payment", 1, 1)
        .id();
    let second_selected = scenario
        .add_creature(P0, "Second Selected Exile Payment", 1, 1)
        .id();
    for name in [
        "First Mana Selected-Exile Redirect",
        "Second Mana Selected-Exile Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("start the selected-exile mana ability");
    let initial = runner
        .act(GameAction::SelectCards {
            cards: vec![selected, second_selected],
        })
        .expect("select the creature for the mana ability exile cost");

    assert!(
        matches!(initial.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "a selected mana-exile cost must consult competing Moved redirects"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. })
            if cursor.remaining.len() == 1
                && cursor.selected_exile_remaining.as_deref() == Some(&[second_selected])
    ));
    let json = serde_json::to_string(runner.state())
        .expect("a paused selected mana-exile replacement choice serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("a paused selected mana-exile replacement choice deserializes");
    let mut runner = GameRunner::from_state(restored);

    let after_first_redirect = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the first selected mana-exile cost");

    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&second_selected].zone,
        Zone::Battlefield
    );
    assert!(matches!(
        after_first_redirect.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect the second selected mana-exile cost");

    assert_eq!(
        runner.state().objects[&second_selected].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the selected-exile activation must produce exactly one mana after resuming"
    );
    assert_eq!(
        initial
            .events
            .iter()
            .chain(after_first_redirect.events.iter())
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the selected-exile resume must not replay the paid tap prefix"
    );
    assert_eq!(
        initial
            .events
            .iter()
            .chain(after_first_redirect.events.iter())
            .chain(resumed.events.iter())
            .filter(
                |event| matches!(event, GameEvent::ManaAdded { player_id, .. } if *player_id == P0)
            )
            .count(),
        1,
        "the selected-exile resume must not produce mana twice"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
}

#[test]
fn effect_pay_cost_composite_mana_life_suffix_serializes_and_rides_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana.clone() },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: cost.clone(),
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the composite effect cost reaches the mana source replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "neither the later life cost nor the rider may run before the typed mana root settles"
    );
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost { cost: paused_cost, .. }
                if paused_cost.as_ref() == &cost
        )
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the complete composite effect-cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the complete composite effect-cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected mana-source cost resumes the full Composite suffix");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "the exact order is mana, PayLife once, then the +1-life rider once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the source's paid tap prefix is never replayed"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn effect_pay_cost_composite_mana_life_prevention_serializes_and_rides_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost,
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the composite effect cost reaches a mana-source cost move pause");

    // No ZoneChange replacement currently yields `Prevented`, so stage the
    // canonical one-shot Prevented producer while retaining the real paused
    // ManaAbilityPayment root. This drives the actual replacement-choice
    // dispatcher branch that a future cost-move prevention will use.
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("mana source exists")
        .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Destroy)
        .regeneration_shield()
        .description("Prevent the staged cost move".to_string())]
    .into();
    runner.state_mut().pending_replacement = Some(PendingReplacement {
        proposed: ProposedEvent::Destroy {
            object_id: source,
            source: None,
            cant_regenerate: false,
            applied: Default::default(),
        },
        sacrifice_provenance: None,
        candidates: vec![ReplacementId { source, index: 0 }],
        search_found_candidates: Vec::new(),
        depth: 0,
        is_optional: false,
        library_placement: None,
        excess_recipient: None,
        lifelink_bonus: 0,
        may_cost_paid: false,
        may_cost_remaining: None,
    });
    runner.state_mut().waiting_for = WaitingFor::ReplacementChoice {
        player: P0,
        candidate_count: 1,
        candidates: vec![],
    };

    let json = serde_json::to_string(runner.state())
        .expect("the prevented composite effect-cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the prevented composite effect-cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the Prevented dispatcher resumes the complete typed cost root");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "prevention still settles mana then PayLife once before the rider"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the prevented cost move cannot replay the source's paid tap prefix"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn paused_mana_cost_events_create_observer_triggers_once_and_preserve_order_resume() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let observer_trigger = |amount| {
        TriggerDefinition::new(TriggerMode::Taps)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::Any)
            .trigger_zones(vec![Zone::Battlefield])
    };
    for (name, amount) in [
        ("First Cost Event Observer", 1),
        ("Second Cost Event Observer", 2),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_trigger_definition(observer_trigger(amount));
    }
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&source].card_id;
    runner.state_mut().pending_cast = Some(Box::new(PendingCast::new(
        source,
        card_id,
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Cost event observer outer payment".to_string(),
                description: None,
            },
            vec![],
            source,
            P0,
        ),
        ManaCost::generic(1),
    )));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the source pauses after its tap cost");
    assert!(runner.state().stack.is_empty());

    let json = serde_json::to_string(runner.state()).expect("paused cost event batch serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("paused cost event batch deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("resume the typed cost event settlement");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::OrderTriggers { ref triggers, .. } if triggers.len() == 2
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    let ordered = runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("both observer triggers remain orderable after the cost settles");
    assert!(matches!(
        ordered.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().stack.len(),
        2,
        "each actual observer trigger is collected exactly once, not once per pause and resume"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the resumed mana production itself is emitted once"
    );
}

#[test]
fn nested_costed_mana_source_serializes_parent_cursor_and_finishes_outer_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let outer = scenario
        .add_creature(P0, "Outer Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                ],
            }),
        )
        .id();
    let inner = scenario
        .add_creature(P0, "Inner Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Nested Source Redirect",
        "Second Nested Source Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Nested Mana Payment Target", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the nested witness must announce a real pending cast");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let outer_ability = runner.state().objects[&outer].abilities[0].clone();
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        outer,
        P0,
        0,
        &outer_ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the inner source pauses on its self-exile cost");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor })
            if pending.source_id == inner
                && cursor.parent.as_ref().is_some_and(|parent| {
                    parent.lifecycle == ManaAbilityCostParentLifecycle::Suspended
                        && parent.pending.source_id == outer
                        && matches!(parent.cursor.remaining.as_slice(), [AbilityCost::Mana { .. }])
                })
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the suspended parent mana cursor serializes");
    assert!(
        json.contains("Suspended"),
        "the serialized parent frame must retain its typed re-entry ownership"
    );
    let restored: GameState =
        serde_json::from_str(&json).expect("the suspended parent mana cursor deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirect inner self-exile and resume the exact parent cursor");

    assert_eq!(runner.state().objects[&inner].zone, Zone::Graveyard);
    assert!(runner.state().objects[&outer].tapped);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == outer))
            .count(),
        1,
        "the outer tap prefix is retained by the parent cursor rather than replayed"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == inner))
            .count(),
        1,
        "the inner source's tap cost is paid once across the replacement pause"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == inner
            ))
            .count(),
        1,
        "the redirected inner self-exile cost is delivered once"
    );
    for source_id in [inner, outer] {
        assert_eq!(
            initial_events
                .iter()
                .chain(resumed.events.iter())
                .filter(|event| matches!(event, GameEvent::ManaAdded { source_id: id, .. } if *id == source_id))
                .count(),
            1,
            "each nested mana ability produces exactly once"
        );
    }

    runner
        .act(GameAction::PassPriority)
        .expect("the outer spell payment consumes the outer mana once");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

fn two_source_assist_replacement_witness(
    mana_cost: ManaCost,
) -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    [engine::types::identifiers::ObjectId; 2],
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_sources = [
        scenario
            .add_creature(P1, "First Two-Source Assist Mana Witness", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Exile {
                            count: 1,
                            zone: None,
                            filter: Some(TargetFilter::SelfRef),
                        },
                    ],
                }),
            )
            .id(),
        scenario
            .add_creature(P1, "Second Two-Source Assist Mana Witness", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            )
            .id(),
    ];
    for name in [
        "First Two-Source Assist Redirect",
        "Second Two-Source Assist Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Two-Source Assist Payment Witness", true)
        .with_mana_cost(mana_cost)
        .with_keyword(Keyword::Assist)
        .id();
    (scenario.build(), spell, helper_sources)
}

#[test]
fn committed_assist_retries_remaining_sources_after_serialized_ordinary_pause() {
    let (mut runner, spell, helper_sources) =
        two_source_assist_replacement_witness(ManaCost::generic(2));
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 2 })
        .expect("commit the two generic helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the first helper source pauses on its replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the ordinary two-source Assist pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the ordinary two-source Assist pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first helper source completes without losing the remaining plan");
    assert_eq!(
        runner.state().objects[&helper_sources[0]].zone,
        Zone::Graveyard
    );
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("PaymentStarted must tap the remaining helper source before spending");
    assert_eq!(
        runner.state().objects[&helper_sources[1]].zone,
        Zone::Battlefield
    );
    assert!(runner.state().objects[&helper_sources[1]].tapped);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn committed_assist_retries_remaining_sources_after_serialized_phyrexian_pause() {
    let (mut runner, spell, helper_sources) =
        two_source_assist_replacement_witness(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 2,
        });
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist Phyrexian spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 2 })
        .expect("commit the two generic helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the caster chooses the Phyrexian shard before helper payment");
    runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the first helper source pauses after the submitted Phyrexian choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the Phyrexian two-source Assist pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the Phyrexian two-source Assist pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the exact Phyrexian root retries the remaining helper source plan");
    assert_eq!(
        runner.state().objects[&helper_sources[0]].zone,
        Zone::Graveyard
    );
    assert_eq!(
        runner.state().objects[&helper_sources[1]].zone,
        Zone::Battlefield
    );
    assert!(runner.state().objects[&helper_sources[1]].tapped);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn committed_assist_phyrexian_choice_serializes_helper_cost_pause_and_charges_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Helper Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Assist Helper Redirect",
        "Second Assist Helper Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Phyrexian Costed Source Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let assist = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Assist spell reaches the helper choice");
    assert!(matches!(
        assist.waiting_for,
        WaitingFor::AssistChoosePlayer { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the helper");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit one generic from the helper");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let phyrexian = runner
        .act(GameAction::PassPriority)
        .expect("the caster must choose the Phyrexian payment");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::Committed {
            helper: P1,
            generic: 1
        })
    ));

    let paused = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("submitted Phyrexian choice starts the committed helper payment");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::PhyrexianCastPayment {
                caster: P0,
                ref choices,
            } if choices == &vec![engine::types::game_state::ShardChoice::PayLife]
        )
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1
        })
    ));
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        0,
        "a helper pause cannot spend mana before its source resolves"
    );

    let json = serde_json::to_string(runner.state())
        .expect("the committed Assist + submitted Phyrexian root serializes");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the committed Assist + submitted Phyrexian root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("helper source replacement retries the exact submitted choices");

    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        18,
        "the submitted PayLife choice is retained and paid exactly once"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == helper_source))
            .count(),
        1,
        "the helper source's paid tap prefix is never replayed"
    );
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == helper_source))
            .count(),
        1,
        "the helper produces and spends its committed mana once"
    );
}

#[test]
fn caster_phyrexian_finalization_serializes_costed_source_pause_and_retries_choices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Caster Phyrexian Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Caster Phyrexian Redirect",
        "Second Caster Phyrexian Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Caster Phyrexian Costed Source Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the caster reaches the mana-payment window");
    assert!(matches!(
        cast.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    // The regular announcement above creates the real stack entry and
    // `PendingCast`. Enter the exact LifeOnly-shard finalization prompt here
    // so the witness exercises submitted-choice retry without relying on a
    // source-selection heuristic to reach the prompt first.
    runner.state_mut().waiting_for = WaitingFor::PhyrexianPayment {
        player: P0,
        spell_object: spell,
        shards: vec![engine::types::game_state::PhyrexianShard {
            shard_index: 0,
            color: ManaColor::Green,
            options: engine::types::game_state::ShardOptions::LifeOnly,
        }],
    };

    let paused = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("caster's generic source pauses after the submitted Phyrexian choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::PhyrexianCastPayment {
                caster: P0,
                ref choices,
            } if choices == &vec![engine::types::game_state::ShardChoice::PayLife]
        )
    ));
    assert!(runner.state().pending_cast.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("caster Phyrexian costed-source pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("caster Phyrexian costed-source pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the typed caster Phyrexian root retries its submitted choice");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the caster source's cost is paid once across the replacement pause"
    );
}

#[test]
fn committed_assist_mana_payment_serializes_helper_redirect_and_charges_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Ordinary Helper Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Ordinary Assist Helper Redirect",
        "Second Ordinary Assist Helper Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Ordinary Costed Source Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let offered = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    assert!(matches!(
        offered.waiting_for,
        WaitingFor::AssistChoosePlayer { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the assisting player");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit the helper's generic contribution");

    let paused = runner
        .act(GameAction::PassPriority)
        .expect("the helper's cost move pauses on its Moved replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        )
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1,
        })
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the ordinary Assist helper pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the ordinary Assist helper pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the helper's typed outer payment root resumes after redirect");

    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|pending| pending.assist_state),
        Some(engine::types::game_state::AssistState::PaymentStarted {
            helper: P1,
            generic: 1,
        })
    ));
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        1,
        "the resumed helper produces its committed mana before the outer spend"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original Assist payment finishes the cast");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 0);
    assert!(!matches!(
        completed.waiting_for,
        WaitingFor::AssistChoosePlayer { .. } | WaitingFor::AssistPayment { .. }
    ));

    let events = paused
        .events
        .iter()
        .chain(resumed.events.iter())
        .chain(completed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == helper_source))
            .count(),
        1,
        "the helper's tap cost is retained across serialization and never replayed"
    );
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == helper_source))
            .count(),
        1,
        "the helper produces exactly one mana for its committed Assist contribution"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the outer spell finalizes exactly once without another Assist offer"
    );
}

#[test]
fn nested_composite_effect_cost_serializes_all_suffixes_and_rider_once() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 2;
    let mana = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    };
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: mana.clone() },
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };
    let mut ability = ResolvedAbility::new(
        Effect::PayCost {
            cost: cost.clone(),
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    )));

    let expected_resume_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ],
    };

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut initial_events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut initial_events, 0)
        .expect("the nested cost reaches the source's replacement pause");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            &pending.resume,
            ManaAbilityResume::EffectPayCost { cost: paused_cost, .. }
                if paused_cost.as_ref() == &expected_resume_cost
        )
    ));

    let json =
        serde_json::to_string(runner.state()).expect("the nested composite cost root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the nested composite cost root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the redirected source resumes every enclosing composite suffix");

    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert_eq!(runner.state().players[P0.0 as usize].energy, 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "PayLife settles once before the +1-life rider settles once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::LifeChanged { amount: -2, .. }))
            .count(),
        1,
        "the nested PayLife suffix is paid exactly once"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::LifeChanged { amount: 1, .. }))
            .count(),
        1,
        "the rider runs exactly once after the complete nested cost"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the source's paid tap prefix is not replayed"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn mana_cost_post_replacement_named_choice_serializes_and_resumes_outer_payment_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    scenario
        .add_creature(P0, "Mana Cost Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(prompt_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches its mana-payment window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut activation_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut activation_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mandatory replacement delivers and reaches its post-effect prompt");
    assert!(matches!(
        paused,
        WaitingFor::NamedChoice { ref options, .. }
            if options == &vec!["first".to_string(), "second".to_string()]
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert_eq!(
        activation_events
            .iter()
            .filter(|event| matches!(event, GameEvent::ReplacementApplied { .. }))
            .count(),
        1,
        "the mandatory identity replacement applies exactly once before prompting"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        ) && cursor.remaining.is_empty()
    ));
    assert!(runner.state().pending_cast.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the post-effect prompt retains the typed mana-cost cursor on the wire");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the post-effect prompt restores with the typed mana-cost cursor");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("answering the post-effect prompt resumes the parked mana-cost root");

    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the parked cursor produces mana exactly once after the post-effect"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer mana payment spends the resumed mana once");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let events = activation_events
        .iter()
        .chain(resumed.events.iter())
        .chain(completed.events.iter());
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the post-effect pause cannot replay the mana source's paid tap cost"
    );
    assert_eq!(
        events
            .clone()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the post-effect pause cannot produce mana twice"
    );
    assert_eq!(
        events
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the original spell finalizes once after the post-effect prompt"
    );
}

#[test]
fn self_return_mana_cost_post_effect_serializes_without_advancing_planned_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Self-Return Post-Effect Mana Source", 1, 1)
        .as_land()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::ReturnToHand {
                        count: 1,
                        filter: Some(TargetFilter::SelfRef),
                        from_zone: None,
                    },
                ],
            }),
        )
        .id();
    let deferred_source = scenario
        .add_creature(P0, "Deferred Planned Mana Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();
    scenario
        .add_creature(P0, "Self-Return Post-Effect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(redirect_moved_to_with_post_effect(Zone::Hand, Zone::Hand));
    let spell = scenario
        .add_spell_to_hand(P0, "Two-Source Auto-Tap Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::PassPriority)
        .expect("the first auto-tapped source reaches its replacement post-effect");

    assert!(matches!(
        paused.waiting_for,
        WaitingFor::NamedChoice { ref options, .. }
            if options == &vec!["first".to_string(), "second".to_string()]
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert!(
        !runner.state().objects[&deferred_source].tapped,
        "the live post-effect prompt must stop the rest of the auto-tap plan"
    );
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. })
            if cursor.resolution_mode == ManaAbilityCostResolutionMode::AutoResolved
    ));
    assert!(runner.state().pending_replacement.is_none());

    let json = serde_json::to_string(runner.state())
        .expect("the self-return post-effect pause serializes");
    assert!(
        json.contains("AutoResolved"),
        "the serialized cursor must retain its typed auto-resolution mode"
    );
    let restored: GameState =
        serde_json::from_str(&json).expect("the self-return post-effect pause deserializes");
    let mut runner = GameRunner::from_state(restored);

    let resumed = runner
        .act(GameAction::ChooseOption {
            choice: "first".to_string(),
        })
        .expect("the post-effect response resumes the typed self-return cost root");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(
        !runner.state().objects[&deferred_source].tapped,
        "resuming the first source must not advance the next planned source"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the resumed source produces exactly its own mana before the outer payment continues"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the remaining planned source completes the outer payment");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().objects[&deferred_source].tapped);
    assert!(runner.state().pending_cost_move_resume.is_none());

    for (object_id, label) in [
        (source, "self-return source"),
        (deferred_source, "deferred planned source"),
    ] {
        assert_eq!(
            paused
                .events
                .iter()
                .chain(resumed.events.iter())
                .chain(completed.events.iter())
                .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id: tapped, .. } if *tapped == object_id))
                .count(),
            1,
            "the {label} is tapped exactly once across the paused auto-tap plan"
        );
        assert_eq!(
            paused
                .events
                .iter()
                .chain(resumed.events.iter())
                .chain(completed.events.iter())
                .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == object_id))
                .count(),
            1,
            "the {label} produces mana exactly once across the paused auto-tap plan"
        );
    }
}

#[test]
fn prevented_mana_cost_move_serializes_and_restores_mana_payment_root() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Prevented Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches its manual mana-payment window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    assert!(matches!(
        runner.state().pending_cast.as_deref(),
        Some(pending) if pending.object_id == spell && pending.card_id == card_id
    ));
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mana payment root pauses on its source cost move");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    stage_prevented_mana_cost_move(runner.state_mut(), source);

    let json = serde_json::to_string(runner.state())
        .expect("the staged prevented mana-payment root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the staged prevented mana-payment root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the prevented dispatcher restores the exact mana-payment prompt");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    assert!(matches!(
        runner.state().pending_cast.as_deref(),
        Some(pending) if pending.object_id == spell && pending.card_id == card_id
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "prevention must not replay the source's paid tap prefix"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "prevention resumes mana production exactly once"
    );
}

#[test]
fn prevented_mana_cost_move_serializes_and_restores_unless_payment_root() {
    let (scenario, source) = mana_self_exile_cost_redirect_witness();
    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let unless_cost = AbilityCost::Mana {
        cost: ManaCost::generic(1),
    };
    let pending_effect = ResolvedAbility::new(
        Effect::Unimplemented {
            name: "Prevented Unless Payment Witness".to_string(),
            description: None,
        },
        vec![],
        source,
        P0,
    );
    let mut initial_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::UnlessPayment {
            outer_player: Some(P0),
            cost: Box::new(unless_cost.clone()),
            pending_effect: Box::new(pending_effect.clone()),
            trigger_event: None,
            effect_description: Some("prevented unless payment witness".to_string()),
            remaining: vec![P1],
        },
        None,
    )
    .expect("the unless-payment root pauses on its source cost move");
    assert!(matches!(paused, WaitingFor::ReplacementChoice { .. }));
    stage_prevented_mana_cost_move(runner.state_mut(), source);

    let json = serde_json::to_string(runner.state())
        .expect("the staged prevented unless-payment root serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the staged prevented unless-payment root deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the prevented dispatcher restores the exact unless-payment prompt");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::UnlessPayment {
            player: P0,
            ref cost,
            pending_effect: ref resumed_effect,
            trigger_event: None,
            effect_description: Some(ref description),
            ref remaining,
        } if cost == &unless_cost
            && resumed_effect.as_ref() == &pending_effect
            && description == "prevented unless payment witness"
            && remaining == &vec![P1]
    ));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "prevention must not replay the source's paid tap prefix"
    );
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "prevention resumes mana production exactly once"
    );
}

#[test]
fn paused_mana_cost_events_scan_current_and_deferred_observers_once() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let observer = |mode, amount| {
        TriggerDefinition::new(mode)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::Any)
            .trigger_zones(vec![Zone::Battlefield])
    };
    scenario
        .add_creature(P1, "Deferred Tap Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(observer(TriggerMode::Taps, 1));
    scenario
        .add_creature(P0, "Current Mana Observer", 0, 0)
        .as_enchantment()
        .with_trigger_definition(observer(TriggerMode::ManaAdded, 2));

    let mut runner = scenario.build();
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut initial_events = Vec::new();
    activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut initial_events,
        ManaAbilityResume::Priority,
        None,
    )
    .expect("the source pauses after its initial tap event");

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement resume settles both event batches");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(runner.state().stack.len(), 2);
    for amount in [1, 2] {
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(
                            &ability.effect,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value },
                                ..
                            } if *value == amount
                        )
                ))
                .count(),
            1,
            "the observer for amount {amount} is placed exactly once"
        );
    }
    assert_eq!(
        initial_events
            .iter()
            .chain(resumed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the deferred tap event remains exactly-once owned by the cursor"
    );
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the current ManaAdded event is emitted and scanned exactly once"
    );
}

#[test]
fn pre_phyrexian_auto_tap_redirect_preserves_manual_payment_root_without_forcing_prompt() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Pre-Phyrexian Auto-Tap Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("a manual cast reaches its normal mana-payment window");
    let paused = runner
        .act(GameAction::PassPriority)
        .expect("pre-Phyrexian auto-tap reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. }) if matches!(
            pending.resume,
            ManaAbilityResume::ManaPayment {
                outer_player: Some(P0),
                convoke_mode: None,
            }
        )
    ));

    let json = serde_json::to_string(runner.state())
        .expect("the pre-Phyrexian source-cost pause serializes");
    let restored: GameState =
        serde_json::from_str(&json).expect("the pre-Phyrexian source-cost pause deserializes");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the source cost preserves the manual payment root");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: None,
        }
    ));
    let phyrexian = runner
        .act(GameAction::PassPriority)
        .expect("the preserved root computes the real Phyrexian payment prompt");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));
    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the real submitted Phyrexian choice finalizes the original cast");

    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == source))
            .count(),
        1,
        "the pre-Phyrexian source cost is paid exactly once"
    );
}

#[test]
fn automatic_phyrexian_cast_retries_the_original_payment_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Automatic Phyrexian Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianGreen],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let paused = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("automatic casting reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let phyrexian = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the original cast");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));

    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the automatic Phyrexian cast completes after the replacement answer");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the automatic cast finalizes exactly once"
    );
}

#[test]
fn automatic_ordinary_cast_retries_the_original_payment_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let spell = scenario
        .add_spell_to_hand(P0, "Automatic Ordinary Root Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let paused = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("automatic casting reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the original cast");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the automatic cast finalizes exactly once"
    );
}

#[test]
fn automatic_phyrexian_activation_retries_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Automatic Activation Root Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::PhyrexianGreen],
                    generic: 0,
                },
            }),
        )
        .id();

    let mut runner = scenario.build();
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("automatic activation reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let phyrexian = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the automatic source cost resumes the activation");
    assert!(matches!(
        phyrexian.waiting_for,
        WaitingFor::PhyrexianPayment { player: P0, .. }
    ));

    let completed = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("the automatic activation completes after the replacement answer");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(phyrexian.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the automatic activation reaches the stack exactly once"
    );
}

#[test]
fn automatic_ordinary_activation_retries_after_source_cost_redirect() {
    let (mut scenario, source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Automatic Ordinary Activation Root Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }),
        )
        .id();

    let mut runner = scenario.build();
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("ordinary activation reaches the source-cost replacement choice");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                pending.resume,
                ManaAbilityResume::FinalizePendingManaPayment { player: P0 }
            )
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirecting the ordinary source cost resumes the activation");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&activator].zone, Zone::Battlefield);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the ordinary activation reaches the stack exactly once"
    );
}

#[test]
fn targeted_mana_tap_hand_exile_cost_retries_after_source_cost_redirect_without_replaying_exile() {
    let (mut scenario, mana_source) = mana_self_exile_cost_redirect_witness();
    let activator = scenario
        .add_creature(P0, "Targeted Exile Cost Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                    damage_source: None,
                    excess: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::Green],
                            generic: 0,
                        },
                    },
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: Some(Zone::Hand),
                        filter: None,
                    },
                ],
            }),
        )
        .id();
    let target = scenario
        .add_creature(P1, "Targeted Exile Cost Target", 1, 1)
        .id();
    let fuel = scenario.add_card_to_hand(P0, "Targeted Exile Cost Fuel");

    let mut runner = scenario.build();
    let target_selection = runner
        .act(GameAction::ActivateAbility {
            source_id: activator,
            ability_index: 0,
        })
        .expect("the targeted activation announces before paying its costs");
    assert!(matches!(
        target_selection.waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
    let select_exile = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        })
        .expect("target selection surfaces the non-self hand exile cost first");
    assert!(matches!(
        select_exile.waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ExileFromZone { .. },
            ..
        }
    ));

    let fuel_paused = runner
        .act(GameAction::SelectCards { cards: vec![fuel] })
        .expect("the selected hand-exile cost reaches its redirect choice");
    assert!(matches!(
        fuel_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let source_paused = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the paid hand-exile prefix advances to the mana-source redirect");
    assert!(matches!(
        source_paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let completed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the mana-root resume must not replay the already paid hand-exile cost");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
    assert!(
        runner.state().objects[&activator].tapped,
        "the post-mana tap suffix is paid exactly once"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let stack_entry = runner
        .state()
        .stack
        .back()
        .expect("the activation reaches the stack after all cost suffixes settle");
    let StackEntryKind::ActivatedAbility { ability, .. } = &stack_entry.kind else {
        panic!(
            "expected activated ability on the stack, got {:?}",
            stack_entry.kind
        );
    };
    assert_eq!(ability.targets, vec![TargetRef::Object(target)]);
    assert_eq!(
        fuel_paused
            .events
            .iter()
            .chain(source_paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == fuel))
            .count(),
        1,
        "the selected hand-exile cost cannot replay after the mana source resumes"
    );
    assert_eq!(
        target_selection
            .events
            .iter()
            .chain(select_exile.events.iter())
            .chain(fuel_paused.events.iter())
            .chain(source_paused.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::AbilityActivated { source_id, .. } if *source_id == activator))
            .count(),
        1,
        "the target-first activation is announced exactly once"
    );
}

#[test]
fn committed_assist_source_cost_pause_rejects_cast_cancellation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper_source = scenario
        .add_creature(P1, "Assist Cancellation Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in [
        "First Assist Cancellation Redirect",
        "Second Assist Cancellation Redirect",
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Graveyard));
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Assist Cancellation Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Assist)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Assist spell reaches its helper offer");
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the assisting player");
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("commit the helper contribution");
    runner
        .act(GameAction::PassPriority)
        .expect("the helper source reaches its replacement choice");
    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the helper source resumes the committed Assist payment");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));

    let cancelled = runner.act(GameAction::CancelCast);
    assert!(matches!(
        cancelled,
        Err(engine::game::engine::EngineError::ActionNotAllowed(_))
    ));
    assert_eq!(runner.state().objects[&helper_source].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 1);
    assert!(runner.state().pending_cast.is_some());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the non-cancellable committed Assist payment still finalizes");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().players[P1.0 as usize].mana_pool.total(), 0);
}

#[test]
fn mana_cost_scry_post_effect_serializes_until_answered_then_resumes_root_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Scry Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let scry_card = scenario.add_card_to_library_top(P0, "Scry Post-Effect Card");
    scenario
        .add_creature(P0, "Scry Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(scry_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Scry Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let ability = runner.state().objects[&source].abilities[0].clone();
    let mut activation_events = Vec::new();
    let paused = activate_mana_ability(
        runner.state_mut(),
        source,
        P0,
        0,
        &ability,
        &mut activation_events,
        ManaAbilityResume::ManaPayment {
            outer_player: Some(P0),
            convoke_mode: None,
        },
        None,
    )
    .expect("the mandatory replacement delivers and reaches its Scry post-effect");
    assert!(matches!(
        paused,
        WaitingFor::ScryChoice { player: P0, ref cards } if cards == &vec![scry_card]
    ));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the Scry post-effect retains the parked cost root on the wire");
    let restored: GameState =
        serde_json::from_str(&json).expect("the Scry post-effect restores the parked cost root");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectCards {
            cards: vec![scry_card],
        })
        .expect("answering Scry resumes the parked mana-cost root");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the mana source resolves only after the Scry answer"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        activation_events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::ManaAdded { source_id, .. } if *source_id == source))
            .count(),
        1,
        "the Scry post-effect cannot replay mana production"
    );
}

#[test]
fn mana_cost_proliferate_post_effect_serializes_until_answered_then_resumes_root_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Proliferate Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    let proliferate_target = scenario
        .add_creature(P0, "Proliferate Post-Effect Target", 1, 1)
        .id();
    scenario.with_counter(proliferate_target, CounterType::Plus1Plus1, 1);
    scenario
        .add_creature(P0, "Proliferate Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(proliferate_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Proliferate Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the real mana-ability action reaches its Proliferate post-effect");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ProliferateChoice { player: P0, .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
    assert!(runner.state().pending_cost_move_resume.is_some());

    let json = serde_json::to_string(runner.state())
        .expect("the Proliferate post-effect retains the parked cost root on the wire");
    let restored: GameState = serde_json::from_str(&json)
        .expect("the Proliferate post-effect restores the parked cost root");
    let mut runner = GameRunner::from_state(restored);
    let resumed = runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(proliferate_target)],
        })
        .expect("answering Proliferate resumes the parked mana-cost root");

    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(
        runner.state().objects[&proliferate_target].counters[&CounterType::Plus1Plus1],
        2,
        "the interactive post-effect settles before the outer mana root resumes"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the mana source resolves only after the Proliferate answer"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original outer payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the Proliferate post-effect resumes the outer cast exactly once"
    );
}

#[test]
fn optional_post_effect_settles_before_resuming_the_parked_mana_root() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Optional Post-Effect Costed Mana Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    scenario
        .add_creature(P0, "Optional Post-Effect Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_gain_life_after_moved_to_exile());
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Post-Effect Mana Payment Witness", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell reaches manual mana payment");
    let paused = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("the mana ability's mandatory redirect reaches its optional post-effect");
    assert!(
        matches!(
            paused.waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "expected optional post-effect choice, got {:?}",
        paused.waiting_for
    );
    assert!(runner.state().pending_cost_move_resume.is_some());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let resumed = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("answering the optional post-effect resumes the parked mana root");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, 21);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(engine::types::mana::ManaType::Green),
        1,
        "the source resolves only after the optional effect is fully answered"
    );
    assert!(runner.state().pending_cost_move_resume.is_none());

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("the original payment spends the resumed mana");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        paused
            .events
            .iter()
            .chain(resumed.events.iter())
            .chain(completed.events.iter())
            .filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell))
            .count(),
        1,
        "the outer cast resumes exactly once after the optional post-effect"
    );
}

#[test]
fn delve_mana_payment_honors_moved_redirect_without_linking_redirected_fuel() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Delve Redirect Payment Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Delve)
        .id();
    let fuel = scenario
        .add_spell_to_graveyard(P0, "Redirected Delve Fuel", true)
        .id();
    for name in ["First Delve Exile Redirect", "Second Delve Exile Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_moved_to(Zone::Exile, Zone::Hand));
    }

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let announced = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("delve spell reaches its mana-payment window");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));

    let paused = runner
        .act(GameAction::TapForConvoke {
            object_id: fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("delve fuel must consult competing Moved redirects");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected delve fuel restores the mana-payment root");
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Hand);
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == fuel && link.source_id == spell),
        "fuel redirected away from exile must not be linked as exiled with the spell"
    );
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("redirected delve fuel still pays its generic cost component");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

#[test]
fn delve_murktide_link_tracks_only_fuel_delivered_to_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Murktide Regent", true)
        .with_mana_cost(ManaCost::generic(2))
        .with_keyword(Keyword::Delve)
        .id();
    let delivered_fuel = scenario
        .add_spell_to_graveyard(P0, "Delivered Delve Fuel", true)
        .id();
    let redirected_fuel = scenario
        .add_spell_to_graveyard(P0, "Redirected Murktide Fuel", true)
        .id();
    let first_redirect = scenario
        .add_creature(P0, "First Murktide Exile Redirect", 0, 0)
        .as_enchantment()
        .id();
    let second_redirect = scenario
        .add_creature(P0, "Second Murktide Exile Redirect", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Murktide-shaped delve spell reaches mana payment");
    runner
        .act(GameAction::TapForConvoke {
            object_id: delivered_fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("first delve fuel is delivered to exile");

    for redirect in [first_redirect, second_redirect] {
        runner
            .state_mut()
            .objects
            .get_mut(&redirect)
            .expect("redirect source remains on the battlefield")
            .replacement_definitions = vec![redirect_moved_to(Zone::Exile, Zone::Hand)].into();
    }

    let paused = runner
        .act(GameAction::TapForConvoke {
            object_id: redirected_fuel,
            mana_type: engine::types::mana::ManaType::Colorless,
        })
        .expect("second delve fuel must consult competing Moved redirects");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("redirected fuel resumes the Murktide-shaped mana payment");
    assert!(matches!(
        resumed.waiting_for,
        WaitingFor::ManaPayment {
            player: P0,
            convoke_mode: Some(engine::types::game_state::ConvokeMode::Delve),
        }
    ));
    assert_eq!(runner.state().objects[&delivered_fuel].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&redirected_fuel].zone, Zone::Hand);
    let tracked_ids: Vec<_> = runner
        .state()
        .exile_links
        .iter()
        .filter(|link| link.source_id == spell)
        .map(|link| link.exiled_id)
        .collect();
    assert_eq!(tracked_ids, vec![delivered_fuel]);
    assert_eq!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&spell)
            .cloned()
            .unwrap_or_default(),
        vec![delivered_fuel],
        "Murktide's tracked set contains precisely its delivered exile"
    );

    let completed = runner
        .act(GameAction::PassPriority)
        .expect("both delve components pay the generic mana after redirect");
    assert!(matches!(
        completed.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}
