use engine::database::synthesis::synthesize_plot;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CastingPermission, ChoiceType, Effect,
    ModalChoice, QuantityExpr, ReplacementDefinition, ReplacementMode, SpellCastingOption,
    TargetFilter, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, PayCostKind, PendingCostMoveResume, WaitingFor,
};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
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
        runner.state().pending_cost_move_resume,
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
