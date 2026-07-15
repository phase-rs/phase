use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, SpellCastingOption, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

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
