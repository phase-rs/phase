//! Public activated ability payment for a concrete sacrifice count after X mana.
//! Lazotep Quarry exercises its printed activation through token resolution.
//! Fiend Artisan and a Cauldron-granted Whisper activation exercise tap observers
//! that leave while the remaining sacrifice costs are paid.
//! The remaining historical fixtures are synthetic lower-layer cost tests.

use std::ops::ControlFlow;
use std::panic::{catch_unwind, AssertUnwindSafe};

use engine::ai_support::legal_actions_full;
use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef, CounterCostSelection,
    Effect, ManaContribution, ManaProduction, ModalChoice, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementMode, SacrificeAggregateStat, SacrificeCost,
    SacrificeRequirement, TapCreaturesRequirement, TargetFilter, TargetRef, TypeFilter,
    TypedFilter, REMOVE_COUNTER_COST_X,
};
use engine::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActivationResidual, CastPaymentMode, CostResume, ExileLink, ExileLinkKind,
    PendingCostMoveResume, PersistedGameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

fn x_mana() -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X],
        },
    }
}

fn count_creatures(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .into(),
        count,
    ))
}

fn count_lands(count: u32) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(
        TypedFilter::land().controller(ControllerRef::You).into(),
        count,
    ))
}

fn draw_x(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(cost)
}

fn damage_x(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name: "X".into() },
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    )
    .cost(cost)
}

fn optional_graveyard_exile_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                actor: engine::types::ability::LibraryInstructionActor::Controller,
                library_shuffle: Default::default(),
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn choose_x_and_finish_mana(runner: &mut GameRunner, x: u32) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    let pending = runner.state().pending_cast.as_ref().unwrap();
    assert!(matches!(
        pending.activation_residual,
        ActivationResidual::XMana
    ));
    runner.act(GameAction::ChooseX { value: x }).unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
}

fn assert_x_guard_refuses(runner: &mut GameRunner, label: &str) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        runner.act(GameAction::ChooseX { value: 1 })
    }));
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(
                message,
                Some("non-self sacrifice/exile cost unhandled"),
                "{label}"
            );
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(
                message, "non-self sacrifice/exile cost unhandled",
                "{label}"
            );
        }
        Ok(other) => panic!("{label} unexpectedly proceeded: {other:?}"),
    }
}

#[test]
fn x_mana_count_two_pays_once_and_draws_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "X count source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(2)],
        }))
        .id();
    let first = scenario.add_creature(P0, "First fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second fodder", 1, 1).id();
    let third = scenario.add_creature(P0, "Unused fodder", 1, 1).id();
    let opposing = scenario.add_creature(P1, "Opponent creature", 1, 1).id();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 4);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected concrete Count2 sacrifice prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices.len(), 3);
    assert!(choices.contains(&first) && choices.contains(&second) && choices.contains(&third));
    assert!(!choices.contains(&opposing));
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    for (actor, cards) in [
        (P1, vec![first, second]),
        (P0, vec![]),
        (P0, vec![first]),
        (P0, vec![first, second, third]),
        (P0, vec![first, first]),
        (P0, vec![first, opposing]),
    ] {
        let mut invalid = GameRunner::from_state(runner.state().clone());
        let before = serde_json::to_value(invalid.state()).unwrap();
        assert!(apply(
            invalid.state_mut(),
            actor,
            GameAction::SelectCards { cards }
        )
        .is_err());
        assert_eq!(serde_json::to_value(invalid.state()).unwrap(), before);
    }
    let selected = vec![first, second];
    assert!(legal_actions_full(runner.state())
        .0
        .contains(&GameAction::SelectCards {
            cards: selected.clone()
        }));
    runner
        .act(GameAction::SelectCards { cards: selected })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let entry = runner.state().stack.back().unwrap();
    assert_eq!((entry.source_id, entry.controller), (source, P0));
    let ability = entry.ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    let paid: Vec<_> = ability
        .cost_paid_objects
        .iter()
        .map(|record| record.snapshot().unwrap().object_id)
        .collect();
    assert_eq!(paid, vec![first, second]);
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&third].zone, Zone::Battlefield);
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
}

#[test]
fn x_mana_count_zero_acknowledgment_keeps_announced_x() {
    for x in [0, 4] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, "Zero count source", "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(0)],
            }))
            .id();
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, x);
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Count0 acknowledgment: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 0, 0));
        assert!(choices.is_empty());
        assert!(matches!(
            spell.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(spell.ability.chosen_x, Some(x));
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, Some(x));
        assert!(ability.cost_paid_objects.is_empty());
        runner.resolve_top();
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + x as usize
        );
    }
}

#[test]
fn x_mana_repeated_count_keeps_the_payer_after_source_sacrifice() {
    for owner in [P0, P1] {
        for source_first in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let one = AbilityCost::Sacrifice(SacrificeCost::count(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .into(),
                1,
            ));
            let source = scenario
                .add_enchantment_from_oracle(owner, "Source selectable", "")
                .controlled_by(P0)
                .with_ability_definition(draw_x(AbilityCost::Composite {
                    costs: vec![x_mana(), one.clone(), one],
                }))
                .id();
            let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
            let opponent = scenario
                .add_enchantment_from_oracle(P1, "Opponent", "")
                .id();
            scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            choose_x_and_finish_mana(&mut runner, 4);
            let (first, second) = if source_first {
                (source, other)
            } else {
                (other, source)
            };
            for selected in [first, second] {
                let WaitingFor::PayCost {
                    player,
                    choices,
                    min_count,
                    count,
                    resume: CostResume::Spell { spell },
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "expected repeated Count1 prompt: {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!((*player, *min_count, *count), (P0, 1, 1));
                assert!(choices.contains(&selected));
                assert!(!choices.contains(&opponent));
                assert!(matches!(
                    spell.activation_residual,
                    ActivationResidual::XMana
                ));
                assert_eq!(spell.ability.chosen_x, Some(4));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![selected],
                    })
                    .unwrap();
            }
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            let paid: Vec<_> = ability
                .cost_paid_objects
                .iter()
                .map(|record| record.snapshot().unwrap().object_id)
                .collect();
            assert_eq!(paid, vec![first, second]);
            assert_eq!(ability.chosen_x, Some(4));
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
            runner.resolve_top();
            assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
        }
    }
}

#[test]
fn x_mana_targeted_count_keeps_target_and_records_crime_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Targeted X source", "")
        .with_ability_definition(damage_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(1)],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Cost fodder", 1, 1).id();
    let first = scenario.add_creature(P1, "Untargeted", 5, 5).id();
    let second = scenario.add_creature(P1, "Chosen target", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "target declaration must precede cost payment: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(second)],
        })
        .unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("targeted Count1 prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices, &vec![fodder]);
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(ability.targets, vec![TargetRef::Object(second)]);
    // CR 700.13: Targeting an opponent's permanent commits a crime as the
    // activated ability is put on the stack.
    assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&first].damage_marked, 0);
    assert_eq!(runner.state().objects[&second].damage_marked, 4);
    assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
}

// Verbatim MTGJSON Oracle text; the land and creature characteristics below
// also retain their printed types, mana costs, colors, and power/toughness.
const LAZOTEP_QUARRY: &str = "{T}: Add {C}.\n{T}, Sacrifice a creature: Add one mana of any color.\n{X}{2}, {T}, Sacrifice a Desert: Exile target creature card with mana value X from your graveyard. Create a token that's a copy of it, except it's a 4/4 black Zombie. Activate only as a sorcery.";

const FIEND_ARTISAN: &str = "This creature gets +1/+1 for each creature card in your graveyard.\n{X}{B/G}, {T}, Sacrifice another creature: Search your library for a creature card with mana value X or less, put it onto the battlefield, then shuffle. Activate only as a sorcery.";
const CAPTAIN_AMERICA: &str = "Vigilance\nWhenever a creature you control becomes tapped during your turn, if it's the first time that creature has become tapped this turn, untap it.";
const FALLOWSAGE: &str = "Whenever this creature becomes tapped, you may draw a card.";
const WHISPER: &str = "{T}, Sacrifice two creatures: Return target creature card from your graveyard to the battlefield.";
const AGATHAS_SOUL_CAULDRON: &str = "You may spend mana as though it were mana of any color to activate abilities of creatures you control.\nCreatures you control with +1/+1 counters on them have all activated abilities of all creature cards exiled with Agatha's Soul Cauldron.\n{T}: Exile target card from a graveyard. When a creature card is exiled this way, put a +1/+1 counter on target creature you control.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

fn assert_supported_abilities(runner: &GameRunner, source: ObjectId) {
    let object = &runner.state().objects[&source];
    let mut supported = |effect: &Effect| {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "{}: {effect:?}",
            object.name
        );
        ControlFlow::Continue(())
    };
    for ability in object.abilities.iter() {
        assert!(!ability
            .cost
            .as_ref()
            .is_some_and(AbilityCost::contains_unimplemented));
        let _ = visit_ability_def(ability, &mut supported);
    }
    for trigger in object.trigger_definitions.iter_all() {
        let _ = visit_trigger(trigger.definition(), &mut supported);
    }
    for definition in object.static_definitions.iter_all() {
        let _ = visit_static(definition, &mut supported);
    }
    for definition in object.replacement_definitions.iter_all() {
        let _ = visit_replacement(definition, &mut supported);
    }
}

fn sacrifice_activation_index(runner: &GameRunner, source: ObjectId) -> usize {
    assert_supported_abilities(runner, source);
    let indices: Vec<_> = runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .filter_map(|(index, ability)| {
            let mut sacrifice = false;
            if let Some(cost) = &ability.cost {
                cost.for_each_cost_node(&mut |node| {
                    sacrifice |= matches!(node, AbilityCost::Sacrifice(_));
                });
            }
            (ability.kind == AbilityKind::Activated && sacrifice).then_some(index)
        })
        .collect();
    assert_eq!(
        indices.len(),
        1,
        "one printed sacrifice activation must be reachable"
    );
    indices[0]
}

fn tap_events(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
                GameEvent::PermanentTapped { object_id, .. } if *object_id == source
            )
        })
        .count()
}

fn assert_finished(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn assert_one_observer_above_activation(runner: &GameRunner, source: ObjectId, observer: ObjectId) {
    assert_eq!(
        runner.state().stack.len(),
        2,
        "one captured trigger above the activation"
    );
    let activation = runner.state().stack.front().unwrap();
    assert!(matches!(
        activation.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!((activation.source_id, activation.controller), (source, P0));
    let trigger = runner.state().stack.back().unwrap();
    assert!(matches!(
        trigger.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!((trigger.source_id, trigger.controller), (observer, P0));
}

fn restore_payment(runner: &mut GameRunner) {
    let encoded =
        serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    *runner = GameRunner::from_state(
        serde_json::from_value::<PersistedGameState>(encoded)
            .unwrap()
            .into_game_state()
            .unwrap(),
    );
}

struct FiendCards {
    source: ObjectId,
    captain: ObjectId,
    fodder: [ObjectId; 2],
    opposing: ObjectId,
    library_bears: [ObjectId; 2],
    replacements: Option<[ObjectId; 2]>,
}

fn fiend_fixture(captain_controller: PlayerId, replacements: bool) -> (GameRunner, FiendCards) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Fiend Artisan", 1, 1, FIEND_ARTISAN)
        .with_subtypes(vec!["Nightmare"])
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::BlackGreen; 2],
        })
        .with_color(vec![ManaColor::Black, ManaColor::Green])
        .id();
    let captain = scenario
        .add_creature_from_oracle(
            captain_controller,
            "Captain America, Living Legend",
            3,
            4,
            CAPTAIN_AMERICA,
        )
        .as_legendary()
        .with_subtypes(vec!["Human", "Soldier", "Hero"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
        })
        .with_color(vec![ManaColor::White, ManaColor::Blue])
        .id();
    let [first, second, opposing] = [P0, P0, P1].map(|player| {
        scenario
            .add_creature(player, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_color(vec![ManaColor::Green])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id()
    });
    let library_bears = [0, 1].map(|_| {
        scenario
            .add_spell_to_library_top(P0, "Grizzly Bears", false)
            .as_creature()
            .with_subtypes(vec!["Bear"])
            .with_color(vec![ManaColor::Green])
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id()
    });
    let replacements = replacements.then(|| {
        let rest = scenario
            .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::White],
            })
            .with_color(vec![ManaColor::White])
            .id();
        let leyline = scenario
            .add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Black; 2],
            })
            .with_color(vec![ManaColor::Black])
            .id();
        [rest, leyline]
    });
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 6],
    );
    let mut runner = scenario.build();
    // The library builder supplies types/cost; set the printed creature P/T too.
    for id in library_bears {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.power = Some(2);
        object.toughness = Some(2);
        object.base_power = Some(2);
        object.base_toughness = Some(2);
    }
    assert_supported_abilities(&runner, source);
    assert_supported_abilities(&runner, captain);
    assert_eq!(
        runner.state().objects[&captain].trigger_definitions.len(),
        1
    );
    if let Some(ids) = replacements {
        for id in ids {
            assert_supported_abilities(&runner, id);
            assert_eq!(runner.state().objects[&id].replacement_definitions.len(), 1);
        }
    }
    (
        runner,
        FiendCards {
            source,
            captain,
            fodder: [first, second],
            opposing,
            library_bears,
            replacements,
        },
    )
}

fn fiend_announce(runner: &mut GameRunner, cards: &FiendCards) {
    let ability_index = sacrifice_activation_index(runner, cards.source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cards.source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P0, .. }
    ));
}

fn fiend_choose_x(runner: &mut GameRunner, cards: &FiendCards) {
    choose_x_and_finish_mana(runner, 2);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Fiend must offer its printed sacrifice cost: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(spell.object_id, cards.source);
    assert_eq!(spell.ability.chosen_x, Some(2));
    assert!(!choices.contains(&cards.source));
    assert!(!choices.contains(&cards.opposing));
    assert_eq!(
        choices.contains(&cards.captain),
        runner.state().objects[&cards.captain].controller == P0
    );
    assert!(choices.contains(&cards.fodder[0]) || choices.contains(&cards.fodder[1]));
    // CR 603.3: no tap observer can resolve while the cost choice is pending.
    assert!(runner.state().stack.is_empty());
    assert!(!runner.state().objects[&cards.source].tapped);
}

fn fiend_finish(
    runner: &mut GameRunner,
    cards: &FiendCards,
    searched: ObjectId,
    observes: bool,
    tap_count: u32,
) {
    assert!(runner.state().objects[&cards.source].tapped);
    if observes {
        assert_one_observer_above_activation(runner, cards.source, cards.captain);
        runner.resolve_top();
        // CR 113.7a + CR 603.4: the captured first-tap trigger survives its source's departure.
        assert!(
            !runner.state().objects[&cards.source].tapped,
            "Captain must untap Fiend even after being sacrificed"
        );
    } else {
        assert_eq!(runner.state().stack.len(), 1);
    }
    let entry = runner.state().stack.back().unwrap();
    assert!(matches!(
        entry.kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(entry.source_id, cards.source);
    assert_eq!(entry.ability().unwrap().chosen_x, Some(2));
    runner.resolve_top();
    let WaitingFor::SearchChoice {
        player,
        cards: choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Fiend's activation must resolve to a real search: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *count), (P0, 1));
    assert!(choices.contains(&searched));
    runner
        .act(GameAction::SelectCards {
            cards: vec![searched],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&searched].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&cards.source].tapped, !observes);
    assert_eq!(
        runner.state().object_tap_count_this_turn.get(&cards.source),
        Some(&tap_count)
    );
    assert_finished(runner);
}

#[test]
fn fiend_tap_observer_survives_selected_sacrifice_and_respects_controller() {
    for (controller, sacrifice_observer) in [(P0, true), (P0, false), (P1, false)] {
        let (mut runner, cards) = fiend_fixture(controller, false);
        fiend_announce(&mut runner, &cards);
        fiend_choose_x(&mut runner, &cards);
        let selected = if sacrifice_observer {
            cards.captain
        } else {
            cards.fodder[0]
        };
        let result = runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap();
        assert_eq!(tap_events(&result.events, cards.source), 1);
        let tap = result.events.iter().position(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == cards.source)).unwrap();
        let departure = result.events.iter().position(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. } if *object_id == selected)).unwrap();
        // CR 601.2h + CR 603.10: tap first while the selected observer still exists.
        assert!(tap < departure);
        assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        fiend_finish(
            &mut runner,
            &cards,
            cards.library_bears[0],
            controller == P0,
            1,
        );
    }
}

#[test]
fn fiend_second_real_payment_does_not_repeat_first_tap_trigger() {
    let (mut runner, cards) = fiend_fixture(P0, false);
    let mut events = Vec::new();
    for (index, selected) in cards.fodder.into_iter().enumerate() {
        fiend_announce(&mut runner, &cards);
        fiend_choose_x(&mut runner, &cards);
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![selected],
                })
                .unwrap()
                .events,
        );
        assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        // CR 603.4: the first completed untap is the reach guard for the second-tap negative.
        fiend_finish(
            &mut runner,
            &cards,
            cards.library_bears[index],
            index == 0,
            (index + 1) as u32,
        );
    }
    assert_eq!(tap_events(&events, cards.source), 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn fiend_tap_prefix_survives_replacement_choice_and_restore_once() {
    for replacement_index in [0, 1] {
        let (mut runner, cards) = fiend_fixture(P0, true);
        fiend_announce(&mut runner, &cards);
        fiend_choose_x(&mut runner, &cards);
        let mut events = runner
            .act(GameAction::SelectCards {
                cards: vec![cards.captain],
            })
            .unwrap()
            .events;
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("competing graveyard replacements must pause");
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2);
        let replacements = cards.replacements.unwrap();
        for id in replacements {
            assert!(candidates.iter().any(|candidate| candidate.source_id == id));
        }
        let choice = candidates
            .iter()
            .position(|candidate| candidate.source_id == replacements[replacement_index])
            .unwrap();
        assert!(runner.state().objects[&cards.source].tapped);
        assert!(runner.state().stack.is_empty());
        let Some(PendingCostMoveResume::SacrificeForCost {
            deferred_cost_events,
            ..
        }) = &runner.state().pending_cost_move_resume
        else {
            panic!("the existing sacrifice root must own the pause");
        };
        assert_eq!(
            tap_events(deferred_cost_events, cards.source),
            0,
            "tap prefix must not join the deferred departure fragment"
        );
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner.act(GameAction::CancelCast).is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        restore_payment(&mut runner);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: choice })
                .unwrap()
                .events,
        );
        assert_eq!(runner.state().objects[&cards.captain].zone, Zone::Exile);
        assert_eq!(tap_events(&events, cards.source), 1);
        fiend_finish(&mut runner, &cards, cards.library_bears[0], true, 1);
    }
}

#[test]
fn fiend_unpaid_cancel_and_invalid_selections_do_not_create_tap_observers() {
    let (mut valid, cards) = fiend_fixture(P0, false);
    fiend_announce(&mut valid, &cards);
    let mut cancelled = GameRunner::from_state(valid.state().clone());
    let result = cancelled.act(GameAction::CancelCast).unwrap();
    assert_eq!(tap_events(&result.events, cards.source), 0);
    assert!(!cancelled.state().objects[&cards.source].tapped);
    assert_eq!(
        cancelled.state().objects[&cards.captain].zone,
        Zone::Battlefield
    );
    assert!(!cancelled
        .state()
        .object_tap_count_this_turn
        .contains_key(&cards.source));
    assert!(cancelled.state().pending_cast.is_none());
    assert_finished(&cancelled);
    fiend_choose_x(&mut valid, &cards);
    for case in [
        "empty",
        "duplicate",
        "opponent",
        "departed",
        "tapped source",
    ] {
        let mut invalid = GameRunner::from_state(valid.state().clone());
        let selected = match case {
            "empty" => vec![],
            "duplicate" => vec![cards.captain, cards.captain],
            "opponent" => vec![cards.opposing],
            "departed" => {
                engine::game::zones::move_to_zone(
                    invalid.state_mut(),
                    cards.captain,
                    Zone::Graveyard,
                    &mut vec![],
                );
                vec![cards.captain]
            }
            "tapped source" => {
                invalid
                    .state_mut()
                    .objects
                    .get_mut(&cards.source)
                    .unwrap()
                    .tapped = true;
                vec![cards.captain]
            }
            _ => unreachable!(),
        };
        let before = serde_json::to_value(invalid.state()).unwrap();
        // CR 118.3 + CR 733.1: invalid payments leave the public action snapshot unchanged.
        assert!(
            invalid
                .act(GameAction::SelectCards { cards: selected })
                .is_err(),
            "{case}"
        );
        assert_eq!(
            serde_json::to_value(invalid.state()).unwrap(),
            before,
            "{case}"
        );
        assert!(!invalid
            .state()
            .object_tap_count_this_turn
            .contains_key(&cards.source));
        assert!(invalid.state().stack.is_empty());
    }
    valid
        .act(GameAction::SelectCards {
            cards: vec![cards.captain],
        })
        .unwrap();
    fiend_finish(&mut valid, &cards, cards.library_bears[0], true, 1);
}

#[test]
fn fallowsage_granted_whisper_cost_preserves_its_tap_trigger_after_sacrifice() {
    for (sacrifice_source, accept) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cauldron = scenario
            .add_artifact_from_oracle(P0, "Agatha's Soul Cauldron", AGATHAS_SOUL_CAULDRON)
            .as_legendary()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let whisper = scenario
            .add_creature_to_exile(P0, "Whisper, Blood Liturgist", 2, 2)
            .from_oracle_text(WHISPER)
            .as_legendary()
            .with_subtypes(vec!["Human", "Cleric"])
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Black],
            })
            .with_color(vec![ManaColor::Black])
            .id();
        let source = scenario
            .add_creature_from_oracle(P0, "Fallowsage", 2, 2, FALLOWSAGE)
            .with_plus_counters(1)
            .with_subtypes(vec!["Merfolk", "Wizard"])
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            })
            .with_color(vec![ManaColor::Blue])
            .id();
        let fodder = [0, 1].map(|_| {
            scenario
                .add_creature(P0, "Grizzly Bears", 2, 2)
                .with_subtypes(vec!["Bear"])
                .with_color(vec![ManaColor::Green])
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id()
        });
        let targets = [0, 1].map(|_| {
            scenario
                .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
                .with_subtypes(vec!["Bear"])
                .with_color(vec![ManaColor::Green])
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id()
        });
        let library_bears = [0, 1].map(|_| {
            scenario
                .add_spell_to_library_top(P0, "Grizzly Bears", false)
                .as_creature()
                .with_subtypes(vec!["Bear"])
                .with_color(vec![ManaColor::Green])
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id()
        });
        let mut runner = scenario.build();
        for id in library_bears {
            let object = runner.state_mut().objects.get_mut(&id).unwrap();
            object.power = Some(2);
            object.toughness = Some(2);
            object.base_power = Some(2);
            object.base_toughness = Some(2);
        }
        // CR 613.1f: established linked exile grants the actual printed activation.
        runner.state_mut().exile_links.push(ExileLink {
            exiled_id: whisper,
            source_id: cauldron,
            kind: ExileLinkKind::TrackedBySource,
        });
        runner.state_mut().layers_dirty.mark_full();
        engine::game::layers::evaluate_layers(runner.state_mut());
        assert_supported_abilities(&runner, cauldron);
        assert_supported_abilities(&runner, whisper);
        let ability_index = sacrifice_activation_index(&runner, source);
        assert_eq!(runner.state().objects[&source].trigger_definitions.len(), 1);
        assert!(runner.state().objects[&source]
            .granted_abilities_from
            .is_some_and(|first| ability_index >= first));
        let granting_object =
            runner.state().objects[&source].abilities[ability_index].granting_object;
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "Whisper must offer its two real graveyard targets: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert_eq!(target_slots[0].legal_targets.len(), 2);
        for target in targets {
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(target)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(targets[1])),
            })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!("granted Whisper must offer the real Count2 cost");
        };
        assert_eq!((*player, *min_count, *count), (P0, 2, 2));
        assert_eq!(choices.len(), 3);
        for id in [source, fodder[0], fodder[1]] {
            assert!(choices.contains(&id));
        }
        assert_eq!(spell.ability.targets, vec![TargetRef::Object(targets[1])]);
        // Whisper does not name its granter; preserve whatever binding the grant supplied.
        assert_eq!(spell.ability.context.granting_object, granting_object);
        assert!(!runner.state().objects[&source].tapped);
        assert!(runner.state().stack.is_empty());
        let selected = if sacrifice_source {
            vec![source, fodder[0]]
        } else {
            fodder.to_vec()
        };
        let events = runner
            .act(GameAction::SelectCards {
                cards: selected.clone(),
            })
            .unwrap()
            .events;
        assert_eq!(tap_events(&events, source), 1);
        assert_one_observer_above_activation(&runner, source, source);
        let ability = runner.state().stack.front().unwrap().ability().unwrap();
        assert_eq!(ability.context.granting_object, granting_object);
        assert_eq!(ability.targets, vec![TargetRef::Object(targets[1])]);
        assert_eq!(ability.cost_paid_objects.len(), 2);
        for (record, selected) in ability.cost_paid_objects.iter().zip(selected) {
            let paid = record.snapshot().unwrap();
            assert_eq!(paid.object_id, selected);
            assert_eq!(paid.lki.tapped, selected == source);
            assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        }
        assert_eq!(
            runner.state().objects[&source].zone,
            if sacrifice_source {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        runner.resolve_top();
        let WaitingFor::OptionalEffectChoice {
            player, source_id, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "the captured Fallowsage trigger must offer its draw: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *source_id), (P0, source));
        assert_eq!(runner.state().players[0].hand.len(), hand_before);
        // CR 603.2 + CR 113.7a: accepting the departed source's trigger still draws one.
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + usize::from(accept)
        );
        assert_eq!(runner.state().stack.len(), 1);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&targets[1]].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&targets[0]].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&whisper].zone, Zone::Exile);
        assert_eq!(
            runner.state().object_tap_count_this_turn.get(&source),
            Some(&1)
        );
        assert_finished(&runner);
    }
}

struct QuarryCards {
    source: ObjectId,
    other_desert: ObjectId,
    opposing_desert: ObjectId,
    bears: [ObjectId; 2],
    opposing_bears: ObjectId,
    giant: ObjectId,
    ability_index: usize,
}

fn quarry_fixture(source_owner: PlayerId) -> (GameRunner, QuarryCards) {
    let mut scenario = GameScenario::new();
    // CR 307.5: "Only as a sorcery" requires our main phase and an empty stack.
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_land_from_oracle(source_owner, "Lazotep Quarry", LAZOTEP_QUARRY)
        .with_subtypes(vec!["Desert"])
        .controlled_by(P0)
        .id();
    let other_desert = scenario
        .add_land_from_oracle(P0, "Lazotep Quarry", LAZOTEP_QUARRY)
        .with_subtypes(vec!["Desert"])
        .id();
    let opposing_desert = scenario
        .add_land_from_oracle(P1, "Lazotep Quarry", LAZOTEP_QUARRY)
        .with_subtypes(vec!["Desert"])
        .id();
    let [first, second, opposing_bears] = [P0, P0, P1].map(|owner| {
        scenario
            .add_creature_to_graveyard(owner, "Grizzly Bears", 2, 2)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .with_subtypes(vec!["Bear"])
            .id()
    });
    let giant = scenario
        .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .with_subtypes(vec!["Giant"])
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    // CR 205.3: production loads the creature-type vocabulary from card data;
    // scenarios do not derive it from the cards added to the fixture.
    runner.state_mut().all_creature_types = vec!["Bear".into(), "Giant".into(), "Zombie".into()];
    let x_abilities: Vec<_> = runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| {
            let mut has_x = false;
            if let Some(cost) = &ability.cost {
                cost.for_each_cost_node(&mut |node| {
                    has_x |= matches!(node, AbilityCost::Mana { cost } if cost.has_x());
                });
            }
            has_x
        })
        .collect();
    assert_eq!(
        x_abilities.len(),
        1,
        "must find Quarry's printed X activation"
    );
    let (ability_index, ability) = x_abilities[0];
    assert_eq!(ability.kind, AbilityKind::Activated);
    assert!(!ability.cost.as_ref().unwrap().contains_unimplemented());
    let _ = visit_ability_def(ability, &mut |effect: &Effect| {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "{ability:?}"
        );
        ControlFlow::Continue(())
    });
    (
        runner,
        QuarryCards {
            source,
            other_desert,
            opposing_desert,
            bears: [first, second],
            opposing_bears,
            giant,
            ability_index,
        },
    )
}

fn quarry_announce_x(runner: &mut GameRunner, cards: &QuarryCards) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: cards.source,
            ability_index: cards.ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P0, .. }
    ));
    // CR 107.3a + CR 107.3i: X is announced once and governs cost and target.
    choose_x_and_finish_mana(runner, 2);
    let WaitingFor::TargetSelection {
        player,
        pending_cast,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Quarry target choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(pending_cast.ability.chosen_x, Some(2));
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(target_slots.len(), 1);
    let legal = &target_slots[0].legal_targets;
    // CR 601.2c + CR 202.3: the chosen target must be a MV2 creature in our graveyard.
    for bear in cards.bears {
        assert!(legal.contains(&TargetRef::Object(bear)));
    }
    assert_eq!(legal.len(), 2);
    assert!(!legal.contains(&TargetRef::Object(cards.opposing_bears)));
    assert!(!legal.contains(&TargetRef::Object(cards.giant)));
}

fn quarry_choose_target(runner: &mut GameRunner, cards: &QuarryCards) {
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("expected target selection");
    };
    let target = target_slots[0]
        .legal_targets
        .iter()
        .find(|target| **target == TargetRef::Object(cards.bears[1]))
        .expect("second Bears must be offered")
        .clone();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Quarry sacrifice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices.len(), 2);
    assert!(choices.contains(&cards.source));
    assert!(choices.contains(&cards.other_desert));
    assert!(!choices.contains(&cards.opposing_desert));
    assert_eq!(spell.ability.chosen_x, Some(2));
    assert_eq!(
        spell.ability.targets,
        vec![TargetRef::Object(cards.bears[1])]
    );
}

fn quarry_pay_and_resolve(runner: &mut GameRunner, cards: &QuarryCards, sacrifice: ObjectId) {
    let sacrifice_owner = runner.state().objects[&sacrifice].owner;
    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrifice],
        })
        .unwrap();
    // CR 601.2h + CR 602.2b: {X}{2} with X=2 spends all four floating mana once.
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = runner.state().stack.back().unwrap();
    assert_eq!((entry.source_id, entry.controller), (cards.source, P0));
    let ability = entry.ability().unwrap();
    assert_eq!(ability.chosen_x, Some(2));
    assert_eq!(ability.targets, vec![TargetRef::Object(cards.bears[1])]);
    assert_eq!(ability.cost_paid_objects.len(), 1);
    let paid = ability.cost_paid_objects[0].snapshot().unwrap();
    assert_eq!(paid.object_id, sacrifice);
    assert_eq!((paid.lki.controller, paid.lki.owner), (P0, sacrifice_owner));
    // CR 107.5: self-sacrifice records a tapped source; another Desert does not tap.
    assert_eq!(paid.lki.tapped, sacrifice == cards.source);
    let sacrificed = &runner.state().objects[&sacrifice];
    assert_eq!(sacrificed.zone, Zone::Graveyard);
    // CR 701.21a: controller pays, but the sacrificed card goes to its owner's graveyard.
    assert_eq!(sacrificed.owner, sacrifice_owner);
    assert!(runner.state().players[sacrifice_owner.0 as usize]
        .graveyard
        .contains(&sacrifice));
    let retained = if sacrifice == cards.source {
        cards.other_desert
    } else {
        cards.source
    };
    assert_eq!(runner.state().objects[&retained].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&retained].tapped,
        retained == cards.source
    );
    assert_eq!(
        runner.state().objects[&cards.opposing_desert].zone,
        Zone::Battlefield
    );
    assert!(!runner.state().objects[&cards.opposing_desert].tapped);
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // CR 400.7j: copy the creature exiled by the effect, not the cost-paid Desert.
    assert_eq!(runner.state().objects[&cards.bears[1]].zone, Zone::Exile);
    for untouched in [cards.bears[0], cards.opposing_bears, cards.giant] {
        assert_eq!(runner.state().objects[&untouched].zone, Zone::Graveyard);
    }
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|object| object.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    // CR 707.2 + CR 707.9b: retain name/cost, replacing P/T, color, and creature subtype.
    assert_eq!(token.name, "Grizzly Bears");
    assert_eq!(token.controller, P0);
    assert_eq!((token.power, token.toughness), (Some(4), Some(4)));
    assert_eq!(token.color, vec![ManaColor::Black]);
    assert_eq!(token.card_types.core_types, vec![CoreType::Creature]);
    assert_eq!(token.card_types.subtypes, vec!["Zombie".to_string()]);
    assert_eq!(
        token.mana_cost,
        ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green]
        }
    );
}

#[test]
fn lazotep_quarry_x_tap_self_sacrifice_copies_selected_bears() {
    for owner in [P0, P1] {
        let (mut runner, cards) = quarry_fixture(owner);
        quarry_announce_x(&mut runner, &cards);
        quarry_choose_target(&mut runner, &cards);
        quarry_pay_and_resolve(&mut runner, &cards, cards.source);
    }
}

#[test]
fn lazotep_quarry_x_tap_other_desert_copies_selected_bears() {
    let (mut runner, cards) = quarry_fixture(P0);
    quarry_announce_x(&mut runner, &cards);
    quarry_choose_target(&mut runner, &cards);
    quarry_pay_and_resolve(&mut runner, &cards, cards.other_desert);
}

#[test]
fn lazotep_quarry_tapped_source_refuses_without_payment() {
    let (mut valid, cards) = quarry_fixture(P0);
    let mut invalid = GameRunner::from_state(valid.state().clone());
    invalid
        .state_mut()
        .objects
        .get_mut(&cards.source)
        .unwrap()
        .tapped = true;
    let before = serde_json::to_value(invalid.state()).unwrap();
    // CR 107.5 + CR 118.3: an already tapped source cannot pay {T}.
    assert!(invalid
        .act(GameAction::ActivateAbility {
            source_id: cards.source,
            ability_index: cards.ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err());
    assert_eq!(serde_json::to_value(invalid.state()).unwrap(), before);
    quarry_announce_x(&mut valid, &cards);
    quarry_choose_target(&mut valid, &cards);
    quarry_pay_and_resolve(&mut valid, &cards, cards.source);
}

#[test]
fn lazotep_quarry_invalid_or_stale_sacrifices_refuse_without_partial_payment() {
    let (mut valid, cards) = quarry_fixture(P0);
    quarry_announce_x(&mut valid, &cards);
    quarry_choose_target(&mut valid, &cards);
    for case in ["empty", "opponent", "departed", "control changed", "tapped"] {
        let mut invalid = GameRunner::from_state(valid.state().clone());
        let chosen = match case {
            "empty" => vec![],
            "opponent" => vec![cards.opposing_desert],
            "departed" => {
                engine::game::zones::move_to_zone(
                    invalid.state_mut(),
                    cards.source,
                    Zone::Graveyard,
                    &mut vec![],
                );
                vec![cards.source]
            }
            "control changed" => {
                let source = invalid.state_mut().objects.get_mut(&cards.source).unwrap();
                source.controller = P1;
                source.base_controller = Some(P1);
                vec![cards.source]
            }
            "tapped" => {
                invalid
                    .state_mut()
                    .objects
                    .get_mut(&cards.source)
                    .unwrap()
                    .tapped = true;
                vec![cards.source]
            }
            _ => unreachable!(),
        };
        let before = serde_json::to_value(invalid.state()).unwrap();
        // CR 118.3 + CR 601.2h + CR 701.21a: reject incomplete or no-longer-payable selections.
        assert!(
            invalid
                .act(GameAction::SelectCards { cards: chosen })
                .is_err(),
            "{case}"
        );
        assert_eq!(
            serde_json::to_value(invalid.state()).unwrap(),
            before,
            "{case}"
        );
        assert!(invalid.state().stack.is_empty());
        assert!(matches!(
            invalid.state().waiting_for,
            WaitingFor::PayCost { .. }
        ));
    }
    quarry_pay_and_resolve(&mut valid, &cards, cards.source);
}

#[test]
fn lazotep_quarry_targets_only_announced_x_in_controllers_graveyard() {
    let (mut valid, cards) = quarry_fixture(P1);
    quarry_announce_x(&mut valid, &cards);
    for target in [cards.opposing_bears, cards.giant] {
        let mut invalid = GameRunner::from_state(valid.state().clone());
        let before = serde_json::to_value(invalid.state()).unwrap();
        assert!(invalid
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target))
            })
            .is_err());
        assert_eq!(serde_json::to_value(invalid.state()).unwrap(), before);
    }
    quarry_choose_target(&mut valid, &cards);
    quarry_pay_and_resolve(&mut valid, &cards, cards.source);
}

#[test]
fn x_mana_replacement_pause_restores_payer_and_announced_x() {
    // No printed card was identified with X mana, two separately encoded Count
    // legs, and draw-X. Whisper's printed {T}/Count2 is the gameplay witness above.
    // This existing typed fixture protects the serialized residual representation:
    // CR 601.2h + CR 602.2b require the complete cost, with no replay of paid legs.
    for (with_tap, accept_first) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let one = AbilityCost::Sacrifice(SacrificeCost::count(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .into(),
            1,
        ));
        let mut costs = vec![x_mana(), one.clone(), one.clone()];
        if with_tap {
            costs.insert(1, AbilityCost::Tap);
        }
        let source = scenario
            .add_enchantment_from_oracle(P1, "Replaced source", "")
            .controlled_by(P0)
            .with_ability_definition(draw_x(AbilityCost::Composite { costs }))
            .id();
        let other = scenario.add_enchantment_from_oracle(P0, "Other", "").id();
        scenario
            .add_creature(P1, "Optional graveyard redirect", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_redirect());
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, 4);
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!("first Count1 prompt: {:?}", runner.state().waiting_for);
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices.len(), 2);
        assert!(choices.contains(&source));
        assert!(choices.contains(&other));
        let locked_cost = serde_json::to_value(&spell.activation_cost_snapshot).unwrap();
        assert!(!locked_cost.is_null());
        let mut events = runner
            .act(GameAction::SelectCards {
                cards: vec![source],
            })
            .unwrap()
            .events;
        assert_eq!(tap_events(&events, source), usize::from(with_tap));
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("source sacrifice must pause for optional replacement");
        };
        let choice = candidates
            .iter()
            .position(|candidate| {
                candidate.description == if accept_first { "Accept" } else { "Decline" }
            })
            .unwrap();
        assert!(runner.state().pending_cost_move_resume.is_some());
        // CR 614.6: Accepting the optional replacement substitutes exile for
        // the graveyard move. The saved cost cursor resumes under the payer.
        let encoded =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_value::<PersistedGameState>(encoded)
            .unwrap()
            .into_game_state()
            .unwrap();
        runner = GameRunner::from_state(restored);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: choice })
                .unwrap()
                .events,
        );
        assert_eq!(
            runner.state().objects[&source].zone,
            if accept_first {
                Zone::Exile
            } else {
                Zone::Graveyard
            }
        );
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "second Count1 prompt after source exit: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![other]);
        assert!(matches!(
            spell.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(spell.ability.chosen_x, Some(4));
        assert_eq!(spell.object_id, source);
        assert_eq!(spell.activation_cost.as_ref(), Some(&one));
        assert_eq!(
            serde_json::to_value(&spell.activation_cost_snapshot).unwrap(),
            locked_cost
        );
        let paid_source = spell.ability.cost_paid_objects[0].snapshot().unwrap();
        assert_eq!(paid_source.object_id, source);
        assert_eq!(paid_source.lki.tapped, with_tap);
        // The engine's existing committed-payment policy rejects cancellation.
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner.act(GameAction::CancelCast).is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        events.extend(
            runner
                .act(GameAction::SelectCards { cards: vec![other] })
                .unwrap()
                .events,
        );
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("second sacrifice must pause for optional replacement");
        };
        let decline = candidates
            .iter()
            .position(|candidate| candidate.description == "Decline")
            .unwrap();
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index: decline })
                .unwrap()
                .events,
        );
        assert_eq!(runner.state().objects[&other].zone, Zone::Graveyard);
        let entry = runner.state().stack.back().unwrap();
        assert_eq!((entry.source_id, entry.controller), (source, P0));
        let ability = entry.ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        let paid: Vec<_> = ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect();
        assert_eq!(paid, vec![source, other]);
        assert_eq!(tap_events(&events, source), usize::from(with_tap));
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
        assert_finished(&runner);
    }
}

#[test]
fn x_mana_unsupported_siblings_refuse_before_count_payment() {
    for (label, unsupported) in [
        (
            "zero life",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 0 },
            },
        ),
        ("unbounded sacrifice", count_creatures(u32::MAX)),
        (
            "graveyard exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
        ),
        (
            "unresolved choice",
            AbilityCost::OneOf {
                costs: vec![count_creatures(1)],
            },
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, label, "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1), unsupported],
            }))
            .id();
        let fodder = scenario.add_creature(P0, "Count fodder", 1, 1).id();
        scenario.add_creature(P0, "Second fodder", 1, 1);
        scenario.add_land_to_graveyard(P0, "Exile candidate");
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        // CR 118.3 + CR 601.2h: an unmodeled sacrifice sibling that competes
        // with the fixed creature leg for the same creatures fails closed at
        // the activation gate, before the X guard is reached.
        let overlaps_count_leg = matches!(label, "unbounded sacrifice" | "unresolved choice");
        let started = runner.act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        });
        if overlaps_count_leg {
            assert!(started.is_err(), "{label}");
        } else {
            started.unwrap();
            assert!(matches!(
                runner
                    .state()
                    .pending_cast
                    .as_ref()
                    .unwrap()
                    .activation_residual,
                ActivationResidual::XMana
            ));
            assert_x_guard_refuses(&mut runner, label);
        }
        assert!(runner.state().stack.is_empty(), "{label}");
        assert_eq!(
            runner.state().objects[&source].zone,
            Zone::Battlefield,
            "{label}"
        );
        assert_eq!(
            runner.state().objects[&fodder].zone,
            Zone::Battlefield,
            "{label}"
        );
    }
}

#[test]
fn x_mana_nested_zero_and_positive_counts_each_pay_once() {
    for zero_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let suffix = if zero_first {
            vec![count_creatures(0), count_creatures(1)]
        } else {
            vec![count_creatures(1), count_creatures(0)]
        };
        let source = scenario
            .add_enchantment_from_oracle(P0, "Nested X source", "")
            .with_ability_definition(draw_x(AbilityCost::Composite {
                costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
            }))
            .id();
        let fodder = scenario.add_creature(P0, "Nested fodder", 1, 1).id();
        scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, 4);
        for count_expected in if zero_first { [0, 1] } else { [1, 0] } {
            let WaitingFor::PayCost {
                player,
                choices,
                min_count,
                count,
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("nested Count prompt: {:?}", runner.state().waiting_for);
            };
            assert_eq!(*player, P0);
            assert_eq!((*min_count, *count), (count_expected, count_expected));
            assert!(matches!(
                spell.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(spell.ability.chosen_x, Some(4));
            let selected = if count_expected == 0 {
                vec![]
            } else {
                assert_eq!(choices, &vec![fodder]);
                vec![fodder]
            };
            runner
                .act(GameAction::SelectCards { cards: selected })
                .unwrap();
        }
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        assert_eq!(
            ability
                .cost_paid_objects
                .iter()
                .map(|record| record.snapshot().unwrap().object_id)
                .collect::<Vec<_>>(),
            vec![fodder]
        );
        assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
    }
}

#[test]
fn x_mana_count_two_after_costed_mana_source_pays_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Outer X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TypedFilter::land().controller(ControllerRef::You).into(),
                    2,
                )),
            ],
        }))
        .id();
    let mana_source = scenario
        .add_land_from_oracle(P0, "Costed double-green source", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green, ManaColor::Green],
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
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }),
        )
        .id();
    let lands: Vec<_> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let mut runner = scenario.build();
    for land in lands.iter().skip(2) {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_residual,
        ActivationResidual::XMana
    ));
    runner
        .state_mut()
        .pending_cast
        .as_mut()
        .unwrap()
        .payment_mode = CastPaymentMode::Manual;
    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player: P0, .. }
        ),
        "expected manual mana window, got {:?}",
        runner.state().waiting_for
    );
    // CR 601.2g + CR 602.2b + CR 605.3a: Activate an available mana
    // ability inside the outer activation's mana-payment window.
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped
        .get(&mana_source)
        .into_iter()
        .flatten()
        .find(|action| {
            matches!(
                action,
                GameAction::ActivateManaSource { .. } | GameAction::TapLandForMana { .. }
            )
        })
        .cloned()
        .expect("engine-authored costed mana source");
    runner.act(action).unwrap();
    assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    for land in lands.iter().take(2) {
        let (_, _, grouped) = legal_actions_full(runner.state());
        let action = grouped
            .get(land)
            .into_iter()
            .flatten()
            .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
            .cloned()
            .expect("engine-authored basic-land mana source");
        runner.act(action).unwrap();
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);
    runner.act(GameAction::PassPriority).unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "outer Count2 sacrifice prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices.len(), 4);
    assert!(matches!(
        spell.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(spell.ability.chosen_x, Some(4));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    let selected = lands[2..].to_vec();
    assert!(legal_actions_full(runner.state())
        .0
        .contains(&GameAction::SelectCards {
            cards: selected.clone()
        }));
    runner
        .act(GameAction::SelectCards {
            cards: selected.clone(),
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(
        ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect::<Vec<_>>(),
        selected
    );
    assert!(lands[..2]
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
}

#[test]
fn x_mana_count_preserves_announced_modal_choice() {
    for with_x in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cost = if with_x {
            AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            }
        } else {
            count_creatures(1)
        };
        let source = scenario
            .add_enchantment_from_oracle(P0, "Modal Count source", "")
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )
                .cost(cost)
                .with_modal(
                    ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 2,
                        mode_descriptions: vec!["One life".into(), "Four life".into()],
                        ..ModalChoice::default()
                    },
                    vec![
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        ),
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 4 },
                                player: TargetFilter::Controller,
                            },
                        ),
                    ],
                ),
            )
            .id();
        let fodder = scenario.add_creature(P0, "Modal fodder", 1, 1).id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        let life_before = runner.state().players[0].life;
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let mut selected_mode = false;
        let mut selected_x = false;
        for _ in 0..3 {
            match &runner.state().waiting_for {
                WaitingFor::AbilityModeChoice { .. } => {
                    // CR 601.2b + CR 602.2b: The announced mode belongs to
                    // this activation through its later cost payment.
                    runner
                        .act(GameAction::SelectModes { indices: vec![1] })
                        .unwrap();
                    selected_mode = true;
                }
                WaitingFor::ChooseXValue { .. } => {
                    assert!(with_x);
                    runner.act(GameAction::ChooseX { value: 4 }).unwrap();
                    selected_x = true;
                }
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                WaitingFor::PayCost { .. } => break,
                waiting => panic!("modal activation did not reach Count payment: {waiting:?}"),
            }
        }
        assert!(selected_mode);
        assert_eq!(selected_x, with_x);
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!("modal Count payment: {:?}", runner.state().waiting_for);
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![fodder]);
        assert_eq!(spell.ability.chosen_x, with_x.then_some(4));
        assert_eq!(spell.ability.selected_mode_labels, vec!["Four life"]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(ability.chosen_x, with_x.then_some(4));
        assert_eq!(ability.selected_mode_labels, vec!["Four life"]);
        assert_eq!(
            ability.cost_paid_objects[0].snapshot().unwrap().object_id,
            fodder
        );
        runner.resolve_top();
        assert_eq!(runner.state().players[0].life, life_before + 4);
    }
}

#[test]
fn x_mana_pure_unresolved_one_of_keeps_its_existing_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Pure choice X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                AbilityCost::OneOf {
                    costs: vec![count_creatures(1)],
                },
            ],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Pure choice fodder", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 4);
    let WaitingFor::ActivationCostOneOfChoice {
        player,
        costs,
        pending_cast,
    } = &runner.state().waiting_for
    else {
        panic!("pure OneOf choice: {:?}", runner.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(costs, &vec![count_creatures(1)]);
    assert_eq!(pending_cast.ability.chosen_x, Some(4));
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::None
    ));
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ChooseActivationCostBranch { index: 0 })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "selected OneOf Count payment: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *min_count, *count), (P0, 1, 1));
    assert_eq!(choices, &vec![fodder]);
    assert_eq!(spell.ability.chosen_x, Some(4));
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(4));
    assert_eq!(
        ability.cost_paid_objects[0].snapshot().unwrap().object_id,
        fodder
    );
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
}

#[test]
fn x_mana_count_two_at_zero_still_pays_two_and_ignores_cast_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Count2 zero X source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(2)],
        }))
        .id();
    let first = scenario.add_creature(P0, "First zero X fodder", 1, 1).id();
    let second = scenario.add_creature(P0, "Second zero X fodder", 1, 1).id();
    scenario.with_library_top(P0, &["A", "B"]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .cost_x_paid = Some(9);
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    choose_x_and_finish_mana(&mut runner, 0);
    let WaitingFor::PayCost {
        player,
        choices,
        min_count,
        count,
        resume: CostResume::Spell { spell },
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Count2 at X0: {:?}", runner.state().waiting_for);
    };
    assert_eq!((*player, *min_count, *count), (P0, 2, 2));
    assert_eq!(choices, &vec![first, second]);
    assert_eq!(spell.ability.chosen_x, Some(0));
    assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
    runner
        .act(GameAction::SelectCards {
            cards: vec![first, second],
        })
        .unwrap();
    let ability = runner.state().stack.back().unwrap().ability().unwrap();
    assert_eq!(ability.chosen_x, Some(0));
    assert_eq!(
        ability
            .cost_paid_objects
            .iter()
            .map(|record| record.snapshot().unwrap().object_id)
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    runner.resolve_top();
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
}

#[test]
fn x_mana_two_activations_on_one_source_keep_independent_announcements() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Repeated zero Count source", "")
        .with_ability_definition(draw_x(AbilityCost::Composite {
            costs: vec![x_mana(), count_creatures(0)],
        }))
        .id();
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .cost_x_paid = Some(9);
    let hand_before = runner.state().players[0].hand.len();
    for (x, expected_hand) in [(4, hand_before + 4), (0, hand_before + 4)] {
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        choose_x_and_finish_mana(&mut runner, x);
        let WaitingFor::PayCost {
            player,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "successive Count0 payment: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 0, 0));
        assert_eq!((spell.object_id, spell.ability.chosen_x), (source, Some(x)));
        assert_eq!(runner.state().objects[&source].cost_x_paid, Some(9));
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .unwrap();
        assert_eq!(
            runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .chosen_x,
            Some(x)
        );
        runner.resolve_top();
        assert_eq!(runner.state().players[0].hand.len(), expected_hand);
        assert!(runner.state().stack.is_empty());
    }
}

#[test]
fn x_mana_distinct_creature_and_land_counts_resume_in_both_orders() {
    for reverse in [false, true] {
        for nested in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let mut suffix = vec![count_creatures(1), count_lands(1)];
            if reverse {
                suffix.reverse();
            }
            let cost = if nested {
                AbilityCost::Composite {
                    costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
                }
            } else {
                let mut costs = vec![x_mana()];
                costs.extend(suffix);
                AbilityCost::Composite { costs }
            };
            let source = scenario
                .add_enchantment_from_oracle(P0, "Distinct Count X source", "")
                .with_ability_definition(draw_x(cost))
                .id();
            let creature = scenario.add_creature(P0, "Only creature", 1, 1).id();
            let land = scenario.add_basic_land(P0, ManaColor::Green);
            scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            choose_x_and_finish_mana(&mut runner, 4);
            let expected = if reverse {
                [land, creature]
            } else {
                [creature, land]
            };
            for id in expected {
                let WaitingFor::PayCost {
                    player,
                    choices,
                    min_count,
                    count,
                    resume: CostResume::Spell { spell },
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("distinct Count payment: {:?}", runner.state().waiting_for);
                };
                assert_eq!((*player, *min_count, *count), (P0, 1, 1));
                assert_eq!(choices, &vec![id]);
                assert_eq!(
                    (spell.object_id, spell.activation_ability_index),
                    (source, Some(0))
                );
                assert_eq!(spell.ability.chosen_x, Some(4));
                assert!(matches!(
                    spell.activation_residual,
                    ActivationResidual::XMana
                ));
                runner
                    .act(GameAction::SelectCards { cards: vec![id] })
                    .unwrap();
                assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
            }
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(ability.chosen_x, Some(4));
            assert_eq!(
                ability
                    .cost_paid_objects
                    .iter()
                    .map(|record| record.snapshot().unwrap().object_id)
                    .collect::<Vec<_>>(),
                expected
            );
            runner.resolve_top();
            assert_eq!(runner.state().players[0].hand.len(), hand_before + 4);
            assert!(runner.state().stack.is_empty());
        }
    }
}

#[test]
fn x_mana_target_and_crime_survive_optional_replacement_restore() {
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_from_oracle(P0, "Replaced targeted X source", "")
            .with_ability_definition(damage_x(AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            }))
            .id();
        let fodder = scenario
            .add_creature(P0, "Replaced payment fodder", 1, 1)
            .id();
        let first = scenario
            .add_creature(P1, "Untargeted replacement control", 5, 5)
            .id();
        let second = scenario
            .add_creature(P1, "Targeted replacement control", 5, 5)
            .id();
        scenario
            .add_creature(P1, "Optional graveyard redirect", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_redirect());
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
        );
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { .. }
        ));
        runner.act(GameAction::ChooseX { value: 4 }).unwrap();
        let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
            panic!(
                "targeted replacement declaration: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(pending_cast.ability.chosen_x, Some(4));
        assert!(matches!(
            pending_cast.activation_residual,
            ActivationResidual::XMana
        ));
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(second)],
            })
            .unwrap();
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::PayCost {
            player,
            choices,
            min_count,
            count,
            resume: CostResume::Spell { spell },
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "targeted replacement cost: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *min_count, *count), (P0, 1, 1));
        assert_eq!(choices, &vec![fodder]);
        assert_eq!(spell.ability.chosen_x, Some(4));
        assert_eq!(spell.ability.targets, vec![TargetRef::Object(second)]);
        assert!(spell.crime_candidate);
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!("optional replacement must pause the targeted payment");
        };
        let choice = candidates
            .iter()
            .position(|candidate| {
                candidate.description == if accept { "Accept" } else { "Decline" }
            })
            .unwrap();
        let encoded =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let restored = serde_json::from_value::<PersistedGameState>(encoded)
            .unwrap()
            .into_game_state()
            .unwrap();
        runner = GameRunner::from_state(restored);
        runner
            .act(GameAction::ChooseReplacement { index: choice })
            .unwrap();
        assert_eq!(
            runner.state().objects[&fodder].zone,
            if accept { Zone::Exile } else { Zone::Graveyard }
        );
        let entry = runner.state().stack.back().unwrap();
        assert_eq!((entry.source_id, entry.controller), (source, P0));
        let ability = entry.ability().unwrap();
        assert_eq!(ability.chosen_x, Some(4));
        assert_eq!(ability.targets, vec![TargetRef::Object(second)]);
        assert_eq!(
            ability.cost_paid_objects[0].snapshot().unwrap().object_id,
            fodder
        );
        assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
        runner.resolve_top();
        assert_eq!(runner.state().objects[&first].damage_marked, 0);
        assert_eq!(runner.state().objects[&second].damage_marked, 4);
        assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
    }
}

#[test]
fn x_mana_mixed_requirement_life_and_exile_shapes_refuse_atomically() {
    let creature: TargetFilter = TypedFilter::creature()
        .controller(ControllerRef::You)
        .into();
    let cases = vec![
        (
            "fixed life zero",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 0 },
            },
            1,
        ),
        (
            "fixed life one",
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            1,
        ),
        ("unbounded sacrifice", count_creatures(u32::MAX), 1),
        (
            "aggregate sacrifice",
            AbilityCost::Sacrifice(SacrificeCost::new(
                creature.clone(),
                SacrificeRequirement::Aggregate {
                    stat: SacrificeAggregateStat::TotalPower,
                    comparator: Comparator::GE,
                    value: 1,
                },
            )),
            1,
        ),
        (
            "hand exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: None,
            },
            1,
        ),
        (
            "graveyard exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
            1,
        ),
        (
            "battlefield exile",
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Battlefield),
                filter: Some(creature.clone()),
            },
            1,
        ),
    ];
    for (label, unsupported, count) in cases {
        for reversed in [false, true] {
            for nested in [false, true] {
                let mut suffix = vec![count_creatures(count), unsupported.clone()];
                if reversed {
                    suffix.reverse();
                }
                let cost = if nested {
                    AbilityCost::Composite {
                        costs: vec![x_mana(), AbilityCost::Composite { costs: suffix }],
                    }
                } else {
                    let mut costs = vec![x_mana()];
                    costs.extend(suffix);
                    AbilityCost::Composite { costs }
                };
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_enchantment_from_oracle(P0, label, "")
                    .with_ability_definition(draw_x(cost))
                    .id();
                let fodder = scenario.add_creature(P0, "Count fodder", 1, 1).id();
                let second = scenario.add_creature(P0, "Second fodder", 1, 1).id();
                let hand = scenario.add_card_to_hand(P0, "Hand exile candidate");
                let graveyard = scenario
                    .add_land_to_graveyard(P0, "Graveyard exile candidate")
                    .id();
                scenario.with_mana_pool(
                    P0,
                    vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
                );
                let mut runner = scenario.build();
                // CR 118.3 + CR 601.2h: an unmodeled sacrifice sibling that
                // competes with the fixed creature leg fails closed at the
                // activation gate, before the X guard is reached.
                let overlaps_count_leg =
                    matches!(label, "unbounded sacrifice" | "aggregate sacrifice");
                let started = runner.act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                });
                if overlaps_count_leg {
                    assert!(started.is_err(), "{label}/{reversed}/{nested}");
                } else {
                    started.unwrap();
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
                        "{label}/{reversed}/{nested}"
                    );
                    assert!(matches!(
                        runner
                            .state()
                            .pending_cast
                            .as_ref()
                            .unwrap()
                            .activation_residual,
                        ActivationResidual::XMana
                    ));
                    assert_x_guard_refuses(&mut runner, &format!("{label}/{reversed}/{nested}"));
                }
                assert!(
                    runner.state().stack.is_empty(),
                    "{label}/{reversed}/{nested}"
                );
                assert_eq!(runner.state().players[0].life, 20);
                assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
                assert!(!runner.state().objects[&source].tapped);
                for id in [fodder, second] {
                    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
                    assert!(!runner.state().objects[&id].tapped);
                }
                assert_eq!(runner.state().objects[&hand].zone, Zone::Hand);
                assert_eq!(runner.state().objects[&graveyard].zone, Zone::Graveyard);
            }
        }
    }
}

#[test]
fn x_mana_targeted_tap_creatures_sibling_refuses_before_payment() {
    run_targeted_tap_creatures_sibling(true);
}

#[test]
fn x_mana_targeted_tap_creatures_sibling_refuses_after_slot_choice() {
    run_targeted_tap_creatures_sibling(false);
}

fn run_targeted_tap_creatures_sibling(bulk: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Targeted mixed TapCreatures", "")
        .with_ability_definition(damage_x(AbilityCost::Composite {
            costs: vec![
                x_mana(),
                count_creatures(1),
                AbilityCost::TapCreatures {
                    requirement: TapCreaturesRequirement::Count { count: 1 },
                    filter: TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                },
            ],
        }))
        .id();
    let fodder = scenario.add_creature(P0, "Untapped fodder", 1, 1).id();
    let target = scenario.add_creature(P1, "Opponent target", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    runner.act(GameAction::ChooseX { value: 1 }).unwrap();
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "target declaration must precede costs: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.ability.chosen_x, Some(1));
    assert!(matches!(
        pending_cast.activation_cost.as_ref(),
        Some(AbilityCost::Composite { costs })
            if costs.iter().any(|cost| matches!(cost, AbilityCost::Sacrifice(_)))
                && costs.iter().any(|cost| matches!(cost, AbilityCost::TapCreatures { .. }))
    ));
    let action = if bulk {
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        }
    } else {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        }
    };
    let result = catch_unwind(AssertUnwindSafe(|| runner.act(action)));
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(message, Some("non-self sacrifice/exile cost unhandled"));
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(message, "non-self sacrifice/exile cost unhandled");
        }
        Ok(other) => panic!("targeted TapCreatures unexpectedly proceeded: {other:?}"),
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&fodder].tapped);
    assert_eq!(runner.state().objects[&target].damage_marked, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_target_x0_refuses() {
    run_modal_symbolic_counter_sibling(true, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_target_x1_refuses() {
    run_modal_symbolic_counter_sibling(true, 1);
}

#[test]
fn x_mana_modal_symbolic_counter_no_target_x0_refuses() {
    run_modal_symbolic_counter_sibling(false, 0);
}

#[test]
fn x_mana_modal_symbolic_counter_no_target_x1_refuses() {
    run_modal_symbolic_counter_sibling(false, 1);
}

fn run_modal_symbolic_counter_sibling(targeted: bool, x: u32) {
    let charge = CounterType::Generic("charge".into());
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cost = AbilityCost::Composite {
        costs: vec![
            x_mana(),
            count_creatures(1),
            AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_X,
                counter_type: CounterMatch::OfType(charge.clone()),
                target: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))),
                selection: CounterCostSelection::SingleObject,
            },
        ],
    };
    let mode = if targeted {
        damage_x(cost.clone())
    } else {
        draw_x(cost.clone())
    };
    let source = scenario
        .add_enchantment_from_oracle(P0, "Modal symbolic cost", "")
        .with_ability_definition(mode.clone().with_modal(
            ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Other mode".into(), "Chosen mode".into()],
                ..ModalChoice::default()
            },
            vec![
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ),
                mode,
            ],
        ))
        .id();
    let fodder = scenario.add_creature(P0, "Cost fodder", 1, 1).id();
    let battery = scenario
        .add_creature(P0, "Counter battery", 1, 1)
        .as_artifact()
        .id();
    let victim = scenario.add_creature(P1, "Chosen victim", 5, 5).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&battery)
        .unwrap()
        .counters
        .insert(charge.clone(), 2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AbilityModeChoice { .. }
    ));
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .unwrap();
    let WaitingFor::ChooseXValue { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "modal X must be announced: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(
        pending_cast.activation_residual,
        ActivationResidual::XMana
    ));
    assert_eq!(pending_cast.object_id, source);
    assert_eq!(pending_cast.chosen_modes, vec![1]);
    let result = catch_unwind(AssertUnwindSafe(|| {
        runner.act(GameAction::ChooseX { value: x })
    }));
    if targeted {
        let Ok(Ok(_)) = result else {
            panic!("targeted modal X must reach target selection: {result:?}");
        };
        let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
            panic!(
                "targeted modal X must declare target: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(pending_cast.ability.chosen_x, Some(x));
        assert!(matches!(
            pending_cast.activation_residual,
            ActivationResidual::XMana
        ));
        assert_eq!(pending_cast.chosen_modes, vec![1]);
        let result = catch_unwind(AssertUnwindSafe(|| {
            runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(victim)],
            })
        }));
        assert_modal_sibling_refusal(result, targeted, x);
    } else {
        assert_modal_sibling_refusal(result, targeted, x);
    }
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&battery].counters.get(&charge),
        Some(&2)
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&victim].damage_marked, 0);
}

fn assert_modal_sibling_refusal(
    result: Result<
        Result<engine::types::game_state::ActionResult, EngineError>,
        Box<dyn std::any::Any + Send>,
    >,
    targeted: bool,
    x: u32,
) {
    match result {
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied());
            assert_eq!(message, Some("non-self sacrifice/exile cost unhandled"), "targeted={targeted} x={x}");
        }
        Ok(Err(EngineError::ActionNotAllowed(message))) => {
            assert_eq!(message, "non-self sacrifice/exile cost unhandled", "targeted={targeted} x={x}");
        }
        Ok(other) => panic!("modal symbolic sibling reached payment before validation targeted={targeted} x={x}: {other:?}"),
    }
}

#[test]
fn x_mana_modal_count_only_pays_after_mode_x_and_optional_target() {
    for targeted in [false, true] {
        for x in [0, 1] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let cost = AbilityCost::Composite {
                costs: vec![x_mana(), count_creatures(1)],
            };
            let chosen_mode = if targeted {
                damage_x(cost.clone())
            } else {
                draw_x(cost.clone())
            };
            let source = scenario
                .add_enchantment_from_oracle(P0, "Modal Count only", "")
                .with_ability_definition(chosen_mode.clone().with_modal(
                    ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 2,
                        mode_descriptions: vec!["Other mode".into(), "Chosen mode".into()],
                        ..ModalChoice::default()
                    },
                    vec![
                        AbilityDefinition::new(
                            AbilityKind::Activated,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        ),
                        chosen_mode,
                    ],
                ))
                .id();
            let fodder = scenario.add_creature(P0, "Modal Count fodder", 1, 1).id();
            let victim = scenario.add_creature(P1, "Modal Count victim", 5, 5).id();
            scenario.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Green, ObjectId(9000), false, vec![]); 4],
            );
            if !targeted {
                scenario.with_library_top(P0, &["Draw A", "Draw B"]);
            }
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::AbilityModeChoice { .. }
            ));
            runner
                .act(GameAction::SelectModes { indices: vec![1] })
                .unwrap();
            let WaitingFor::ChooseXValue { pending_cast, .. } = &runner.state().waiting_for else {
                panic!(
                    "modal Count-only X announcement: {:?}",
                    runner.state().waiting_for
                );
            };
            assert!(matches!(
                pending_cast.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(pending_cast.chosen_modes, vec![1]);
            runner.act(GameAction::ChooseX { value: x }).unwrap();
            if targeted {
                let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for
                else {
                    panic!(
                        "modal Count-only target declaration: {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(pending_cast.ability.chosen_x, Some(x));
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(victim)],
                    })
                    .unwrap();
            }
            if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                runner.act(GameAction::PassPriority).unwrap();
            }
            let WaitingFor::PayCost {
                player,
                choices,
                min_count,
                count,
                resume: CostResume::Spell { spell },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("modal Count-only payment: {:?}", runner.state().waiting_for);
            };
            assert_eq!((*player, *min_count, *count), (P0, 1, 1));
            assert_eq!(choices, &vec![fodder]);
            assert_eq!(spell.object_id, source);
            assert_eq!(spell.activation_ability_index, Some(0));
            assert!(matches!(
                spell.activation_residual,
                ActivationResidual::XMana
            ));
            assert_eq!(spell.ability.chosen_x, Some(x));
            // CR 601.2b/c + CR 602.2b: The chosen mode and announced X are
            // bound before targets and remain on the selected ability.
            assert_eq!(spell.ability.selected_mode_labels, vec!["Chosen mode"]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .unwrap();
            assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
            assert_eq!(runner.state().stack.len(), 1);
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(ability.chosen_x, Some(x));
            assert_eq!(ability.selected_mode_labels, vec!["Chosen mode"]);
            assert_eq!(
                ability.cost_paid_objects[0].snapshot().unwrap().object_id,
                fodder
            );
            if targeted {
                assert_eq!(ability.targets, vec![TargetRef::Object(victim)]);
            }
            runner.resolve_top();
            if targeted {
                assert_eq!(runner.state().objects[&victim].damage_marked, x);
                assert_eq!(runner.state().players[0].crimes_committed_this_turn, 1);
            } else {
                assert_eq!(
                    runner.state().players[0].hand.len(),
                    hand_before + x as usize
                );
            }
        }
    }
}
