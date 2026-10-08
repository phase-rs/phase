use engine::game::effects::resolve_ability_chain;
use engine::game::elimination::eliminate_player;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode,
    DiscardSelfScope, Effect, PlayerFilter, QuantityExpr, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, SacrificeCost, SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::{GameAction, ResolutionOptionalPaymentChoice};
use engine::types::game_state::{
    AutoMayChoice, LoopDetectSample, MayTriggerAutoChoiceKey, MayTriggerOrigin, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

fn discard() -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    }
}

fn optional_payment(source: ObjectId, costs: Vec<AbilityCost>) -> ResolvedAbility {
    let mut root = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::OneOf { costs },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    root.optional = true;
    let mut tail = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    tail.condition = Some(AbilityCondition::effect_performed());
    root.sub_ability = Some(Box::new(tail));
    root
}

fn optional_sacrifice_spell(payer: TargetFilter, count: u32) -> AbilityDefinition {
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: AbilityCost::OneOf {
                costs: vec![
                    sacrifice(count),
                    AbilityCost::Mana {
                        cost: ManaCost::generic(99),
                    },
                ],
            },
            scale: None,
            payer,
        },
    );
    root.optional = true;
    let mut payoff = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );
    payoff.condition = Some(AbilityCondition::effect_performed());
    let mut continuation = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::ScopedPlayer),
        },
    );
    continuation.player_scope = Some(PlayerFilter::Opponent);
    continuation.sub_link = SubAbilityLink::SequentialSibling;
    continuation.sub_ability = Some(Box::new(payoff));
    root.sub_ability = Some(Box::new(continuation));
    root
}

fn start_optional_sacrifice_spell(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    runner.resolve_top();
    assert!(
        runner.state().resolving_stack_entry.is_some(),
        "fixture must pause a real resolving spell carrier"
    );
    if let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for {
        let player = *player;
        apply(
            runner.state_mut(),
            player,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ResolutionOptionalPaymentChoice { .. }
    ));
}

fn runner_with_hand(card_count: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let cards = (0..card_count)
        .map(|index| scenario.add_card_to_hand(P0, &format!("Payment Card {index}")))
        .collect();
    (scenario.build(), source, cards)
}

fn cast_to_resolution_optional_payment(runner: &mut GameRunner, card: ObjectId) {
    let mut cast = runner.cast(card).commit();
    for _ in 0..32 {
        if matches!(
            cast.state().waiting_for,
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
        ) {
            return;
        }
        cast.act(GameAction::PassPriority)
            .expect("cast/ETB production path advances to optional payment");
    }
    panic!(
        "production path never reached optional payment: {:?}",
        cast.state().waiting_for
    );
}

fn pass_until_trigger_target_selection(runner: &mut GameRunner) {
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("reflexive trigger advances to its target selection");
    }
    panic!(
        "reflexive trigger never requested its target: {:?}",
        runner.state().waiting_for
    );
}

fn drain_without_reflexive_target(runner: &mut GameRunner) {
    for _ in 0..64 {
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. }
            ),
            "declined/impossible payment must never create Bullseye's reflexive target prompt"
        );
        if runner.state().stack.is_empty()
            && runner.state().pending_trigger.is_none()
            && runner.state().pending_trigger_order.is_none()
        {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("production stack drains without a reflexive trigger");
            }
            ref other => panic!("unexpected negative-path prompt while draining: {other:?}"),
        }
    }
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_trigger.is_none());
    assert!(runner.state().pending_trigger_order.is_none());
    assert!(runner.state().pending_trigger_entry.is_none());
    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
}

fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn optional_graveyard_prevention() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn sacrifice(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::Any, count))
}

#[test]
fn direct_resolution_paycost_cannot_silently_prepaid_non_self_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Direct Payment Source", 1, 1)
        .id();
    let fodder = scenario.add_creature(P0, "Direct Fodder", 1, 1).id();
    let mut runner = scenario.build();
    let mut hostile = optional_payment(source, vec![sacrifice(1)]);
    hostile.optional = false;
    let Effect::PayCost {
        cost: hostile_cost, ..
    } = &mut hostile.effect
    else {
        unreachable!();
    };
    *hostile_cost = sacrifice(1);

    resolve_ability_chain(runner.state_mut(), &hostile, &mut Vec::new(), 0).unwrap();
    assert!(runner.state().cost_payment_failed_flag);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
}

#[test]
fn saved_decline_skips_resolution_optional_payment_prompt_and_payoff() {
    let (mut runner, source, cards) = runner_with_hand(1);
    let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
    runner.state_mut().set_may_trigger_auto_choice(
        MayTriggerAutoChoiceKey {
            player: P0,
            source_id: source,
            origin: origin.clone(),
        },
        AutoMayChoice::Decline,
    );
    let mut ability = optional_payment(source, vec![discard()]);
    ability.set_may_trigger_origin_recursive(origin);

    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
}

#[test]
fn repeated_optional_sacrifice_is_not_advertised_without_typed_resume_support() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Repeated Payment Source", 1, 1)
        .id();
    let first = scenario.add_creature(P0, "First Fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second Fodder", 1, 1).id();
    let mut runner = scenario.build();
    let mut ability = optional_payment(source, vec![sacrifice(1)]);
    ability.repeat_for = Some(QuantityExpr::Fixed { value: 2 });

    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
}

#[test]
fn saved_accept_still_opens_resolution_optional_payment_branch_prompt() {
    let (mut runner, source, _) = runner_with_hand(1);
    let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
    runner.state_mut().set_may_trigger_auto_choice(
        MayTriggerAutoChoiceKey {
            player: P0,
            source_id: source,
            origin: origin.clone(),
        },
        AutoMayChoice::Accept,
    );
    let mut ability = optional_payment(source, vec![discard()]);
    ability.set_may_trigger_origin_recursive(origin);

    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ResolutionOptionalPaymentChoice { player: P0, .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
}

#[test]
fn parsed_trigger_reaches_optional_payment_through_cast_and_apply() {
    const ORACLE: &str =
        "When this creature enters, you may discard a card or pay {2}. If you do, you gain 3 life.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Production Payment", 1, 1, ORACLE)
        .id();
    scenario.add_card_to_hand(P0, "Payment Card");
    let mut runner = scenario.build();
    {
        let mut committed = runner.cast(creature).commit();
        for _ in 0..20 {
            if matches!(
                committed.state().waiting_for,
                WaitingFor::ResolutionOptionalPaymentChoice { .. }
            ) {
                break;
            }
            committed
                .act(GameAction::PassPriority)
                .expect("production cast/trigger pipeline advances");
        }
        assert!(matches!(
            committed.state().waiting_for,
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
        ));
    }
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .expect("parsed trigger payment action applies");
    assert_eq!(runner.state().players[P0.0 as usize].life, 23);
}

#[test]
fn kun_lun_warrior_decline_discard_and_sacrifice_paths() {
    const ORACLE: &str = "When this creature enters, you may sacrifice an artifact or discard a card. If you do, draw a card.";

    // Decline: no payment and no If-you-do draw.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warrior = scenario
        .add_creature_to_hand_from_oracle(P0, "K'un-Lun Warrior", 3, 3, ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Decline Top");
    scenario.add_card_to_hand(P0, "Decline Fodder");
    let mut runner = scenario.build();
    cast_to_resolution_optional_payment(&mut runner, warrior);
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Decline,
        },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&top].zone, Zone::Library);

    // Discard: branch index 1 discards exactly the chosen card, then draws once.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warrior = scenario
        .add_creature_to_hand_from_oracle(P0, "K'un-Lun Warrior", 3, 3, ORACLE)
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    let top = scenario.add_card_to_library_top(P0, "Discard Top");
    let mut runner = scenario.build();
    cast_to_resolution_optional_payment(&mut runner, warrior);
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 1 },
        },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&top].zone, Zone::Hand);

    // Sacrifice: branch index 0 offers only the artifact and draws once after
    // the canonical sacrifice cursor completes.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warrior = scenario
        .add_creature_to_hand_from_oracle(P0, "K'un-Lun Warrior", 3, 3, ORACLE)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Payment Artifact", "")
        .id();
    let nonartifact = scenario.add_creature(P0, "Ineligible Creature", 1, 1).id();
    let top = scenario.add_card_to_library_top(P0, "Sacrifice Top");
    let mut runner = scenario.build();
    cast_to_resolution_optional_payment(&mut runner, warrior);
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    let WaitingFor::PayCost { choices, count, .. } = &runner.state().waiting_for else {
        panic!("artifact sacrifice must use the canonical selector");
    };
    assert_eq!((*count, choices.as_slice()), (1, &[artifact][..]));
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![artifact],
        },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&nonartifact].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&top].zone, Zone::Hand);

    // No artifact and no hand card: the impossible optional payment declines
    // without exposing a prompt or drawing.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warrior = scenario
        .add_creature_to_hand_from_oracle(P0, "K'un-Lun Warrior", 3, 3, ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Unavailable Top");
    let mut runner = scenario.build();
    let mut cast = runner.cast(warrior).commit();
    for _ in 0..16 {
        if cast.state().stack.is_empty() {
            break;
        }
        assert!(!matches!(
            cast.state().waiting_for,
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
        ));
        cast.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(cast.state().objects[&warrior].zone, Zone::Battlefield);
    assert_eq!(cast.state().objects[&top].zone, Zone::Library);
}

#[test]
fn bullseye_when_you_do_creates_reflexive_trigger_then_targets() {
    const ORACLE: &str = "When Bullseye enters, you may sacrifice an artifact or discard a nonland card. When you do, Bullseye deals 2 damage to any target.\n{3}, {T}, Sacrifice an artifact or discard a nonland card: Bullseye deals 2 damage to any target.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bullseye = scenario
        .add_creature_to_hand_from_oracle(P0, "Bullseye, Death Dealer", 3, 3, ORACLE)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Bullseye Fodder", "")
        .id();
    let mut runner = scenario.build();
    cast_to_resolution_optional_payment(&mut runner, bullseye);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![artifact],
        },
    )
    .unwrap();
    pass_until_trigger_target_selection(&mut runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .unwrap();
    for _ in 0..8 {
        if runner.state().players[P1.0 as usize].life == 18 {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().players[P1.0 as usize].life, 18);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);

    // The nonland-discard sibling reaches the same delayed target seam.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bullseye = scenario
        .add_creature_to_hand_from_oracle(P0, "Bullseye, Death Dealer", 3, 3, ORACLE)
        .id();
    let discard = scenario.add_card_to_hand(P0, "Bullseye Discard");
    let mut runner = scenario.build();
    cast_to_resolution_optional_payment(&mut runner, bullseye);
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 1 },
        },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
    pass_until_trigger_target_selection(&mut runner);

    // Declining the parent payment creates no reflexive trigger and asks for no
    // target. A land-only hand likewise leaves no payable branch.
    for with_land in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bullseye = scenario
            .add_creature_to_hand_from_oracle(P0, "Bullseye, Death Dealer", 3, 3, ORACLE)
            .id();
        if with_land {
            scenario.add_land_to_hand(P0, "Only Land");
        } else {
            scenario.add_card_to_hand(P0, "Decline Nonland");
        }
        let mut runner = scenario.build();
        if with_land {
            let mut cast = runner.cast(bullseye).commit();
            for _ in 0..16 {
                if cast.state().stack.is_empty() {
                    break;
                }
                assert!(!matches!(
                    cast.state().waiting_for,
                    WaitingFor::ResolutionOptionalPaymentChoice { .. }
                        | WaitingFor::TriggerTargetSelection { .. }
                ));
                cast.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(cast.state().objects[&bullseye].zone, Zone::Battlefield);
            drop(cast);
            drain_without_reflexive_target(&mut runner);
            assert_eq!(runner.state().players[P1.0 as usize].life, 20);
        } else {
            cast_to_resolution_optional_payment(&mut runner, bullseye);
            apply(
                runner.state_mut(),
                P0,
                GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Decline,
                },
            )
            .unwrap();
            drain_without_reflexive_target(&mut runner);
            assert_eq!(runner.state().players[P1.0 as usize].life, 20);
        }
    }
}

#[test]
fn resolution_optional_discard_replacement_still_opens_if_you_do() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let card = scenario.add_card_to_hand(P0, "Payment Card");
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement());
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(source, vec![discard()]);
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("replacement-modified cost must pause on ReplacementChoice");
    };
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description == "Accept")
        .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: accept },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
}

#[test]
fn resolution_optional_oneof_surfaces_only_live_immediate_branches() {
    let (mut runner, source, _) = runner_with_hand(1);
    let ability = optional_payment(
        source,
        vec![
            discard(),
            AbilityCost::OneOf {
                costs: vec![discard()],
            },
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: None,
            },
        ],
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();

    let WaitingFor::ResolutionOptionalPaymentChoice {
        player,
        source_id,
        costs,
    } = &runner.state().waiting_for
    else {
        panic!("expected direct resolution payment choice");
    };
    assert_eq!((*player, *source_id), (P0, source));
    assert_eq!(
        costs.iter().map(|option| option.index).collect::<Vec<_>>(),
        vec![0, 2],
        "filtered branches retain their original server indices"
    );
    let json = serde_json::to_string(runner.state()).expect("choice state serializes");
    let _: engine::types::game_state::GameState =
        serde_json::from_str(&json).expect("choice state round-trips");
    let pay = GameAction::ChooseResolutionOptionalPaymentBranch {
        choice: ResolutionOptionalPaymentChoice::Pay { index: 2 },
    };
    assert_eq!(
        serde_json::to_value(&pay).unwrap(),
        serde_json::json!({
            "type": "ChooseResolutionOptionalPaymentBranch",
            "data": { "choice": { "type": "Pay", "data": { "index": 2 } } }
        })
    );
    assert_eq!(
        serde_json::from_value::<GameAction>(serde_json::to_value(&pay).unwrap()).unwrap(),
        pay
    );

    let before = serde_json::to_string(runner.state()).unwrap();
    assert!(apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
    assert!(apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 1 },
        },
    )
    .is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
    assert!(apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: usize::MAX },
        },
    )
    .is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
}

#[test]
fn resolution_optional_payment_revalidates_stale_affordability() {
    let (mut runner, source, cards) = runner_with_hand(1);
    let ability = optional_payment(source, vec![discard()]);
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    move_to_zone(
        runner.state_mut(),
        cards[0],
        Zone::Graveyard,
        &mut Vec::new(),
    );
    let before = serde_json::to_string(runner.state()).unwrap();

    assert!(apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
}

#[test]
fn selecting_root_branch_does_not_substitute_nested_oneof() {
    let (mut runner, source, cards) = runner_with_hand(2);
    let mut ability = optional_payment(source, vec![discard()]);
    ability.sub_ability = Some(Box::new(optional_payment(
        source,
        vec![
            discard(),
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: None,
            },
        ],
    )));
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. } | WaitingFor::DiscardChoice { .. }
    ) {
        apply(
            runner.state_mut(),
            P0,
            GameAction::SelectCards {
                cards: vec![cards[0]],
            },
        )
        .unwrap();
    }

    let WaitingFor::ResolutionOptionalPaymentChoice { costs, .. } = &runner.state().waiting_for
    else {
        panic!(
            "nested optional OneOf must remain a distinct prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        costs.iter().map(|option| option.index).collect::<Vec<_>>(),
        vec![0, 1]
    );
}

#[test]
fn resolution_optional_oneof_with_no_payable_branch_declines_without_prompt() {
    let (mut runner, source, _) = runner_with_hand(0);
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(
        source,
        vec![
            discard(),
            AbilityCost::Mana {
                cost: ManaCost::generic(99),
            },
        ],
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ResolutionOptionalPaymentChoice { .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
}

#[test]
fn resolution_optional_oneof_uses_paycost_player_reference() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let payer_card = scenario.add_card_to_hand(P1, "Payer Card");
    let mut runner = scenario.build();
    let mut ability = optional_payment(source, vec![discard()]);
    let Effect::PayCost { payer, .. } = &mut ability.effect else {
        unreachable!();
    };
    *payer = TargetFilter::Opponent;
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ResolutionOptionalPaymentChoice { player: P1, .. }
        ),
        "PayCost player reference must own the prompt, got {:?}",
        runner.state().waiting_for
    );
    let pay = GameAction::ChooseResolutionOptionalPaymentBranch {
        choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
    };
    assert!(apply(runner.state_mut(), P0, pay.clone()).is_err());
    apply(runner.state_mut(), P1, pay).unwrap();
    assert_eq!(runner.state().objects[&payer_card].zone, Zone::Graveyard);
}

#[test]
fn resolution_optional_oneof_decline_clears_loop_ring_and_cannot_replay() {
    let (mut runner, source, cards) = runner_with_hand(1);
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(source, vec![discard()]);
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    let sampled = runner.state().clone();
    runner
        .state_mut()
        .loop_detect_ring
        .push_back(std::sync::Arc::new(LoopDetectSample {
            normalized: sampled.clone(),
            live: sampled,
        }));
    let decline = GameAction::ChooseResolutionOptionalPaymentBranch {
        choice: ResolutionOptionalPaymentChoice::Decline,
    };
    apply(runner.state_mut(), P0, decline.clone()).expect("decline is legal");
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the optional-payment window can precede a life payment, so its answer must clear the ring"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    assert!(runner.state().players[P0.0 as usize]
        .hand
        .contains(&cards[0]));
    let after = serde_json::to_string(runner.state()).unwrap();
    assert!(apply(runner.state_mut(), P0, decline).is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), after);
}

#[test]
fn resolution_optional_phyrexian_payment_clears_ring_before_observable_life_move() {
    let (mut runner, source, _) = runner_with_hand(0);
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(
        source,
        vec![AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::PhyrexianBlue],
                generic: 0,
            },
        }],
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    let sampled = runner.state().clone();
    runner
        .state_mut()
        .loop_detect_ring
        .push_back(std::sync::Arc::new(LoopDetectSample {
            normalized: sampled.clone(),
            live: sampled,
        }));

    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .expect("choose the Phyrexian payment branch");
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the ring must be cleared before the selected branch can move life"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life + 1,
        "auto-paying 2 life and the 3-life conditional payoff happen during the answer"
    );
}

#[test]
fn resolution_optional_oneof_routes_discard_through_existing_payment() {
    let (mut runner, source, cards) = runner_with_hand(1);
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(source, vec![discard()]);
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .expect("selecting the advertised branch starts canonical payment");
    // The existing executor auto-selects when exactly one legal card exists.
    assert!(runner.state().players[P0.0 as usize]
        .graveyard
        .contains(&cards[0]));
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
}

#[test]
fn resolution_optional_oneof_routes_exile_through_existing_payment() {
    let (mut runner, source, cards) = runner_with_hand(1);
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(
        source,
        vec![AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Hand),
            filter: None,
        }],
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .expect("canonical exile payment starts");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. } | WaitingFor::EffectZoneChoice { .. }
    ) {
        apply(
            runner.state_mut(),
            P0,
            GameAction::SelectCards {
                cards: vec![cards[0]],
            },
        )
        .expect("canonical exile selection completes");
    }
    assert_eq!(
        runner.state().objects[&cards[0]].zone,
        Zone::Exile,
        "waiting after branch/selection: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
}

#[test]
fn resolution_optional_oneof_routes_mana_through_existing_payment() {
    let (mut runner, source, _) = runner_with_hand(0);
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Colorless, source, false, vec![]));
    let life = runner.state().players[P0.0 as usize].life;
    let ability = optional_payment(
        source,
        vec![AbilityCost::Mana {
            cost: ManaCost::generic(1),
        }],
    );
    resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();
    let pay = GameAction::ChooseResolutionOptionalPaymentBranch {
        choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
    };
    apply(runner.state_mut(), P0, pay.clone()).expect("canonical mana payment starts");
    if let WaitingFor::ManaPayment { .. } = runner.state().waiting_for {
        let pip_id = runner.state().players[P0.0 as usize].mana_pool.mana[0].pip_id;
        apply(runner.state_mut(), P0, GameAction::SpendPoolMana { pip_id })
            .expect("pool mana is a legal payment pin");
        apply(runner.state_mut(), P0, GameAction::PassPriority).expect("pinned payment finalizes");
    }
    assert!(runner.state().players[P0.0 as usize]
        .mana_pool
        .mana
        .is_empty());
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
    let after = serde_json::to_string(runner.state()).unwrap();
    assert!(apply(runner.state_mut(), P0, pay).is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), after);
}

#[test]
fn resolution_optional_oneof_routes_fixed_sacrifice_through_existing_cursor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let first = scenario.add_creature(P0, "First Fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second Fodder", 1, 1).id();
    let opponent = scenario.add_creature(P1, "Opponent Fodder", 1, 1).id();
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(2)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        min_count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("fixed sacrifice must surface the canonical PayCost selector");
    };
    assert_eq!((*player, *count, *min_count), (P0, 2, 2));
    assert!(choices.contains(&first) && choices.contains(&second));
    assert!(!choices.contains(&opponent));
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![first, second],
        },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .any(|record| record.object_id == first && record.co_departed == vec![second]));
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .any(|record| record.object_id == second && record.co_departed == vec![first]));
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
}

#[test]
fn resolution_optional_sacrifice_rejects_stale_control_before_performed_latch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(1)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&fodder)
        .unwrap()
        .controller = P1;
    let before = serde_json::to_string(runner.state()).unwrap();
    assert!(apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![fodder]
        },
    )
    .is_err());
    assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    assert!(runner.state().active_optional_effect_frame().is_some());
}

#[test]
fn resolution_optional_sacrifice_replacement_redirect_still_counts_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario
        .add_creature(P1, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement());
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(1)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![fodder],
        },
    )
    .unwrap();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("sacrifice payment must park on the existing replacement cursor");
    };
    assert!(matches!(
        runner.state().pending_cost_move_resume,
        Some(engine::types::game_state::PendingCostMoveResume::SacrificeForCost {
            pending: None,
            completion: engine::types::game_state::PendingSacrificeCostCompletion::ResolutionOptionalPayment { .. },
            ..
        })
    ));
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description == "Accept")
        .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: accept },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Exile);
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
    assert!(runner.state().pending_cost_move_resume.is_none());
}

#[test]
fn replacement_paused_resolution_sacrifice_stamps_the_full_selected_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let redirected = scenario
        .add_creature(P0, "Redirected Fodder", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let ordinary = scenario.add_creature(P0, "Ordinary Fodder", 1, 1).id();
    let mut runner = scenario.build();
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(2)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![redirected, ordinary],
        },
    )
    .unwrap();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("first selected sacrifice must pause on replacement");
    };
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description == "Accept")
        .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: accept },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&redirected].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&ordinary].zone, Zone::Graveyard);
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .any(|record| record.object_id == redirected && record.co_departed == vec![ordinary]));
    assert!(runner
        .state()
        .zone_changes_this_turn
        .iter()
        .any(|record| record.object_id == ordinary && record.co_departed == vec![redirected]));
    assert_eq!(runner.state().players[P0.0 as usize].life, 23);
}

#[test]
fn stale_replacement_prompt_abandons_real_carrier_and_settles_completed_prefix() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Controller, 3))
        .id();
    let warden = scenario
        .add_creature(P0, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let ordinary = scenario.add_creature(P0, "Ordinary Fodder", 1, 1).id();
    let paused = scenario.add_creature(P0, "Paused Fodder", 1, 1).id();
    let stale = scenario.add_creature(P0, "Stale Fodder", 1, 1).id();
    let observer = PlayerId(2);
    scenario.add_creature_from_oracle(
        observer,
        "Death Observer",
        1,
        1,
        "Whenever another creature dies, you gain 1 life.",
    );
    let mut runner = scenario.build();
    move_to_zone(runner.state_mut(), warden, Zone::Exile, &mut Vec::new());
    start_optional_sacrifice_spell(&mut runner, spell);
    move_to_zone(
        runner.state_mut(),
        warden,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![ordinary, paused, stale],
        },
    )
    .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!()
    };
    let decline = candidates
        .iter()
        .position(|candidate| candidate.description == "Decline")
        .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: decline },
    )
    .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .state_mut()
        .objects
        .get_mut(&stale)
        .unwrap()
        .incarnation += 1;
    let stale_incarnation = runner.state().objects[&stale].incarnation;
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: 0 },
    )
    .unwrap();

    assert_eq!(runner.state().objects[&ordinary].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&paused].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&stale].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&stale].incarnation,
        stale_incarnation
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[observer.0 as usize].life,
        21,
        "the completed sacrifice prefix must publish its death trigger exactly once"
    );
    assert_eq!(runner.state().players[P1.0 as usize].life, 20);
}

#[test]
fn serialized_replacement_pause_rejects_same_id_new_incarnation_suffix() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let first = scenario
        .add_creature(P0, "Replacement Fodder", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let stale = scenario.add_creature(P0, "Stale Fodder", 1, 1).id();
    let mut runner = scenario.build();
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(2)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![first, stale],
        },
    )
    .unwrap();

    let wire = serde_json::to_string(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_str(&wire).unwrap();
    runner
        .state_mut()
        .objects
        .get_mut(&stale)
        .unwrap()
        .incarnation += 1;
    let stale_incarnation = runner.state().objects[&stale].incarnation;
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: 0 },
    )
    .unwrap();

    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&stale].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&stale].incarnation,
        stale_incarnation
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
}

#[test]
fn resolution_optional_sacrifice_prevented_move_still_counts_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let fodder = scenario
        .add_creature(P0, "Protected Fodder", 1, 1)
        .with_replacement_definition(optional_graveyard_prevention())
        .id();
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(1)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![fodder],
        },
    )
    .unwrap();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!("preventing replacement must pause sacrifice payment");
    };
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description == "Accept")
        .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseReplacement { index: accept },
    )
    .unwrap();
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].life, life + 3);
}

#[test]
fn eliminated_resolution_sacrifice_payer_declines_real_stack_carrier_without_payoff() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Opponent, 1))
        .id();
    scenario.add_creature(P1, "Foreign Fodder", 1, 1);
    let mut runner = scenario.build();
    let p0_life = runner.state().players[P0.0 as usize].life;
    let p1_life = runner.state().players[P1.0 as usize].life;
    let p2 = PlayerId(2);
    let p2_life = runner.state().players[p2.0 as usize].life;
    start_optional_sacrifice_spell(&mut runner, spell);
    apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { player: P1, .. }
    ));

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());

    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].life, p0_life);
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life,
        "the continuation must not observe the departed payer as a living opponent"
    );
    assert_eq!(
        runner.state().players[p2.0 as usize].life,
        p2_life - 1,
        "canonical decline must resume against the post-departure opponent set"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn eliminated_resolution_sacrifice_payer_game_over_drops_staged_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Opponent, 1))
        .id();
    scenario.add_creature(P1, "Foreign Fodder", 1, 1);
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    start_optional_sacrifice_spell(&mut runner, spell);
    apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life,
        "a terminal game must not resume the staged continuation"
    );
}

#[test]
fn eliminated_resolution_sacrifice_controller_terminates_real_stack_selection_as_eliminated() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Opponent, 1))
        .id();
    scenario.add_creature(P1, "Foreign Fodder", 1, 1);
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    start_optional_sacrifice_spell(&mut runner, spell);
    apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { player: P1, .. }
    ));

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));
}

#[test]
fn eliminated_resolution_sacrifice_controller_terminates_real_stack_replacement_pause() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Opponent, 2))
        .id();
    let warden = scenario
        .add_creature(P0, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let first = scenario.add_creature(P1, "First Foreign Fodder", 1, 1).id();
    let second = scenario
        .add_creature(P1, "Second Foreign Fodder", 1, 1)
        .id();
    let p2 = PlayerId(2);
    scenario.add_creature_from_oracle(
        p2,
        "Death Observer",
        1,
        1,
        "Whenever another creature dies, you gain 1 life.",
    );
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    move_to_zone(runner.state_mut(), warden, Zone::Exile, &mut Vec::new());
    start_optional_sacrifice_spell(&mut runner, spell);
    move_to_zone(
        runner.state_mut(),
        warden,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P1,
        GameAction::SelectCards {
            cards: vec![first, second],
        },
    )
    .unwrap();
    let WaitingFor::ReplacementChoice {
        player: replacement_chooser,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("first sacrifice must pause on replacement");
    };
    let replacement_chooser = *replacement_chooser;
    let decline = candidates
        .iter()
        .position(|candidate| candidate.description == "Decline")
        .unwrap();
    apply(
        runner.state_mut(),
        replacement_chooser,
        GameAction::ChooseReplacement { index: decline },
    )
    .unwrap();
    assert!(runner.state().pending_replacement.is_some());
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(
            engine::types::game_state::PendingCostMoveResume::SacrificeForCost {
                paused_at_index: 1,
                ..
            }
        )
    ));
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);

    let mut events = Vec::new();
    eliminate_player(runner.state_mut(), P0, &mut events);
    runner.advance_until_stack_empty();

    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[p2.0 as usize].life,
        21,
        "the completed sacrifice prefix must still create its death trigger"
    );
}

#[test]
fn eliminated_resolution_sacrifice_payer_declines_real_stack_replacement_pause() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Optional Sacrifice", false)
        .with_mana_cost(ManaCost::generic(0))
        .with_ability_definition(optional_sacrifice_spell(TargetFilter::Opponent, 1))
        .id();
    let warden = scenario
        .add_creature(P0, "Graveyard Warden", 1, 1)
        .with_replacement_definition(optional_graveyard_exile_replacement())
        .id();
    let fodder = scenario.add_creature(P1, "Foreign Fodder", 1, 1).id();
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;
    let p2 = PlayerId(2);
    let p2_life = runner.state().players[p2.0 as usize].life;
    move_to_zone(runner.state_mut(), warden, Zone::Exile, &mut Vec::new());
    start_optional_sacrifice_spell(&mut runner, spell);
    move_to_zone(
        runner.state_mut(),
        warden,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P1,
        GameAction::SelectCards {
            cards: vec![fodder],
        },
    )
    .unwrap();
    assert!(runner.state().pending_replacement.is_some());
    assert!(runner.state().pending_cost_move_resume.is_some());

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());

    assert!(runner.state().active_optional_effect_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    assert_eq!(runner.state().players[p2.0 as usize].life, p2_life - 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn unrelated_elimination_preserves_resolution_sacrifice_root() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut runner = scenario.build();
    resolve_ability_chain(
        runner.state_mut(),
        &optional_payment(source, vec![sacrifice(1)]),
        &mut Vec::new(),
        0,
    )
    .unwrap();
    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        },
    )
    .unwrap();
    eliminate_player(runner.state_mut(), p2, &mut Vec::new());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            player: P0,
            kind: engine::types::game_state::PayCostKind::Sacrifice,
            ..
        }
    ));
    assert!(runner.state().active_optional_effect_frame().is_some());
    apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![fodder],
        },
    )
    .unwrap();
    assert_eq!(runner.state().players[P0.0 as usize].life, 23);
}
