//! Issue #5963: sacrificing a creature-that-is-a-Food to a mana ability (Gilded
//! Goose) must still fire "whenever one or more creatures you control die"
//! (Scavenger's Talent L1) and "whenever you sacrifice a permanent" (L2).
//!
//! The load-bearing regression is `food_sacrifice_during_mana_payment_fires_triggers`:
//! when the mana ability is activated *during mana payment* for a spell, the
//! `PayCost` sacrifice resume did not scan its cost events for triggers (the
//! post-action pipeline is skipped for non-`Priority` resumes), so both death
//! and sacrifice observers were silently dropped.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
    FilterProp, ManaContribution, ManaProduction, QuantityExpr, SacrificeCost, StaticDefinition,
    TapCreaturesRequirement, TargetFilter, TriggerConstraint, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::CoreType;
use engine::types::TriggerCondition;

const L1_GAIN: i32 = 50;
const L2_GAIN: i32 = 7;

fn talent_triggers() -> (TriggerDefinition, TriggerDefinition) {
    // L1: Whenever one or more creatures you control die, gain L1_GAIN (batched).
    let mut l1 = TriggerDefinition::new(TriggerMode::ChangesZoneAll)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Creature).controller(ControllerRef::You),
        ))
        .trigger_zones(vec![Zone::Battlefield])
        .constraint(TriggerConstraint::OncePerTurn)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: L1_GAIN },
                player: TargetFilter::Controller,
            },
        ));
    l1.batched = true;

    // L2: Whenever you sacrifice a permanent, gain L2_GAIN.
    let l2 = TriggerDefinition::new(TriggerMode::Sacrificed)
        .valid_card(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Permanent).controller(ControllerRef::You),
        ))
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: L2_GAIN },
                player: TargetFilter::Controller,
            },
        ));

    (l1, l2)
}

fn goose_mana_ability() -> AbilityDefinition {
    // {T}, Sacrifice a Food: Add C. (Gilded Goose's mana ability, colorless to
    // avoid the color choice.)
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
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
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Food".to_string()))),
                1,
            )),
        ],
    })
}

const TAP_GAIN: i32 = 11;

/// Five real colors — an `AnyOneColor` production over more than one option
/// forces the `WaitingFor::ChooseManaColor` prompt (the path the prior
/// double-collection defect lived on), unlike the colorless `Add {C}` form.
fn any_color_options() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

/// `{T}, Tap another creature you control: Add one mana of any color.` (Coalition
/// Relic / Kilo shape). The tapped creature's becomes-tapped observer must fire
/// exactly once through the `ChooseManaColor` settlement boundary.
fn tap_another_mana_ability_any_color() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: any_color_options(),
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
            AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(1),
                filter: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
            },
        ],
    })
}

fn drive_and_gain(
    runner: &mut engine::game::scenario::GameRunner,
    familiar: engine::types::identifiers::ObjectId,
) -> i32 {
    let life_before = runner.state().players[0].life;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                assert!(
                    choices.contains(&familiar),
                    "Food creature should be a legal sacrifice for the mana ability; choices={choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![familiar],
                    })
                    .expect("pay sacrifice-a-Food cost");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }
    runner.state().players[0].life - life_before
}

#[test]
fn intrinsic_food_creature_sacrificed_to_mana_ability_fires_both_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let (l1, l2) = talent_triggers();
    scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_trigger_definition(l1)
        .with_trigger_definition(l2);

    scenario
        .add_creature(P0, "Gilded Goose", 0, 2)
        .as_artifact()
        .with_ability_definition(goose_mana_ability());

    // Cauldron Familiar as a creature that is also a Food (so it is a legal
    // "Sacrifice a Food" target while remaining a creature).
    let familiar = scenario
        .add_creature(P0, "Cauldron Familiar", 1, 1)
        .with_subtypes(vec!["Food"])
        .id();

    let mut runner = scenario.build();
    let goose = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Gilded Goose")
        .copied()
        .unwrap();

    runner
        .act(GameAction::ActivateAbility {
            source_id: goose,
            ability_index: 0,
        })
        .expect("activate Gilded Goose mana ability");

    let gained = drive_and_gain(&mut runner, familiar);

    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Graveyard,
        "familiar should be sacrificed"
    );
    assert_eq!(
        gained,
        L1_GAIN + L2_GAIN,
        "both creature-death (L1) and sacrifice (L2) triggers must fire (intrinsic); gained={gained}"
    );
}

#[test]
fn ygra_food_creature_sacrificed_to_mana_ability_fires_both_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let (l1, l2) = talent_triggers();
    scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_trigger_definition(l1)
        .with_trigger_definition(l2);

    scenario
        .add_creature(P0, "Gilded Goose", 0, 2)
        .as_artifact()
        .with_ability_definition(goose_mana_ability());

    // Ygra: Other creatures are Food artifacts in addition to their other types.
    scenario
        .add_creature(P0, "Ygra, Eater of All", 4, 4)
        .with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature).properties(vec![FilterProp::Another]),
                ))
                .modifications(vec![
                    ContinuousModification::AddSubtype {
                        subtype: "Food".to_string(),
                    },
                    ContinuousModification::AddType {
                        core_type: CoreType::Artifact,
                    },
                ]),
        );

    // Cauldron Familiar as a plain creature made a Food by Ygra.
    let familiar = scenario.add_creature(P0, "Cauldron Familiar", 1, 1).id();

    let mut runner = scenario.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    let goose = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Gilded Goose")
        .copied()
        .unwrap();

    // Sanity: Ygra's continuous effect made the familiar a Food creature.
    assert!(
        runner.state().objects[&familiar]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Food"),
        "Ygra should make the familiar a Food"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: goose,
            ability_index: 0,
        })
        .expect("activate Gilded Goose mana ability");

    let gained = drive_and_gain(&mut runner, familiar);

    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Graveyard,
        "familiar should be sacrificed"
    );
    assert_eq!(
        gained,
        L1_GAIN + L2_GAIN,
        "both creature-death (L1) and sacrifice (L2) triggers must fire (Ygra); gained={gained}"
    );
}

const SCAVENGER_ORACLE: &str = "(Gain the next level as a sorcery to add its ability.)\nWhenever one or more creatures you control die, create a Food token. This ability triggers only once each turn.\n{1}{B}: Level 2\nWhenever you sacrifice a permanent, target player mills two cards.\n{2}{B}: Level 3\nAt the beginning of your end step, you may sacrifice three other nonland permanents. If you do, return a creature card from your graveyard to the battlefield with a finality counter on it.";

const YGRA_ORACLE: &str = "Ward—Sacrifice a Food.\nOther creatures are Food artifacts in addition to their other types and have \"{2}, {T}, Sacrifice this permanent: You gain 3 life.\"\nWhenever a Food is put into a graveyard from the battlefield, put two +1/+1 counters on Ygra.";

/// Faithful reproduction: Scavenger's Talent parsed from Oracle text (Class at
/// level 2), Ygra parsed from Oracle text (its real continuous type-add + its
/// own competing "Food to graveyard" trigger), a Gilded-Goose-style mana
/// ability, and a plain Cauldron Familiar made Food by Ygra.
#[test]
fn faithful_scavengers_talent_and_ygra_food_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let talent = scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(SCAVENGER_ORACLE)
        .id();

    scenario
        .add_creature(P0, "Gilded Goose", 0, 2)
        .as_artifact()
        .with_ability_definition(goose_mana_ability());

    scenario
        .add_creature(P0, "Ygra, Eater of All", 4, 4)
        .from_oracle_text(YGRA_ORACLE);

    let familiar = scenario.add_creature(P0, "Cauldron Familiar", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&talent)
        .unwrap()
        .class_level = Some(2);
    engine::game::layers::evaluate_layers(runner.state_mut());

    let goose = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Gilded Goose")
        .copied()
        .unwrap();

    assert!(
        runner.state().objects[&familiar]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Food"),
        "Ygra should make the familiar a Food"
    );

    let food_before = count_food_tokens(&runner);

    runner
        .act(GameAction::ActivateAbility {
            source_id: goose,
            ability_index: 0,
        })
        .expect("activate Gilded Goose mana ability");

    let mut saw_sacrifice_trigger = false;
    for _i in 0..128 {
        if runner
            .state()
            .stack
            .iter()
            .any(|e| matches!(&e.kind, engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == talent)
                && matches!(&e.kind, engine::types::game_state::StackEntryKind::TriggeredAbility { condition: Some(TriggerCondition::ClassLevelGE { level: 2 }), .. }))
        {
            saw_sacrifice_trigger = true;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                assert!(
                    choices.contains(&familiar),
                    "Food creature should be a legal sacrifice; choices={choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![familiar],
                    })
                    .expect("pay sacrifice-a-Food cost");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose trigger target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Graveyard,
        "familiar should be sacrificed"
    );

    let food_after = count_food_tokens(&runner);
    assert!(
        food_after > food_before,
        "Scavenger's Talent L1 (creatures you control die) must create a Food token when a \
         Food-creature is sacrificed to a mana ability; food_before={food_before} food_after={food_after}"
    );
    assert!(
        saw_sacrifice_trigger,
        "Scavenger's Talent L2 (you sacrifice a permanent) must fire at level 2"
    );
}

/// Reproduction of the real interaction: Gilded Goose's mana ability is
/// activated DURING mana payment for a spell. Sacrificing the Food-creature to
/// pay the mana-ability cost must still fire Scavenger's Talent's death (L1) and
/// sacrifice (L2) triggers once the spell finishes being cast.
#[test]
fn food_sacrifice_during_mana_payment_fires_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);

    let talent = scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Class"])
        .from_oracle_text(SCAVENGER_ORACLE)
        .id();

    scenario
        .add_creature(P0, "Gilded Goose", 0, 2)
        .as_artifact()
        .with_ability_definition(goose_mana_ability());

    scenario
        .add_creature(P0, "Ygra, Eater of All", 4, 4)
        .from_oracle_text(YGRA_ORACLE);

    let familiar = scenario.add_creature(P0, "Cauldron Familiar", 1, 1).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&talent)
        .unwrap()
        .class_level = Some(2);
    engine::game::layers::evaluate_layers(runner.state_mut());

    let goose = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Gilded Goose")
        .copied()
        .unwrap();

    let food_before = count_food_tokens(&runner);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce spell cast");

    // During mana payment, activate Gilded Goose (Tap + Sacrifice a Food) to
    // float the mana.
    runner
        .act(GameAction::ActivateAbility {
            source_id: goose,
            ability_index: 0,
        })
        .expect("activate Gilded Goose mana ability during payment");

    let mut saw_sacrifice_trigger = false;
    for _ in 0..128 {
        if runner.state().stack.iter().any(|e| {
            matches!(&e.kind, engine::types::game_state::StackEntryKind::TriggeredAbility { source_id, condition: Some(TriggerCondition::ClassLevelGE { level: 2 }), .. } if *source_id == talent)
        }) {
            saw_sacrifice_trigger = true;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } if choices.contains(&familiar) => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![familiar],
                    })
                    .expect("pay sacrifice-a-Food cost");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose trigger target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Graveyard,
        "familiar should be sacrificed for the mana-ability cost"
    );
    let food_after = count_food_tokens(&runner);
    assert!(
        food_after > food_before,
        "Scavenger's Talent L1 must fire when a Food-creature is sacrificed to a mana ability \
         used during mana payment; food_before={food_before} food_after={food_after}"
    );
    assert!(
        saw_sacrifice_trigger,
        "Scavenger's Talent L2 must fire when a permanent is sacrificed during mana payment"
    );
}

fn count_food_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let o = &runner.state().objects[id];
            o.is_token && o.card_types.subtypes.iter().any(|s| s == "Food")
        })
        .count()
}

/// Drive an in-flight cast to completion, answering the mana-ability cost
/// selection with `cost_target`, choosing the first offered color for every
/// `ChooseManaColor` prompt, and recording whether any color prompt was seen.
/// Panics if the cost target is ever illegal (guards a silently-skipped cost).
fn drive_payment_to_priority(
    runner: &mut engine::game::scenario::GameRunner,
    cost_target: engine::types::identifiers::ObjectId,
) -> bool {
    let mut saw_color_choice = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                assert!(
                    choices.contains(&cost_target),
                    "mana-ability cost must offer the intended target; choices={choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cost_target],
                    })
                    .expect("pay the mana-ability cost");
            }
            WaitingFor::ChooseManaColor {
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } => {
                saw_color_choice = true;
                assert!(
                    options.len() > 1,
                    "an any-color prompt must offer more than one color; options={options:?}"
                );
                let picked = options[0];
                assert!(
                    picked == ManaType::White
                        || picked == ManaType::Blue
                        || picked == ManaType::Black
                        || picked == ManaType::Red
                        || picked == ManaType::Green,
                    "expected a colored option, got {picked:?}"
                );
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(picked),
                        count: 1,
                    })
                    .expect("choose a color for the any-color mana ability");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose trigger target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    saw_color_choice
}

/// Reviewer follow-up (MED): prove EXACTLY-once cost-event trigger collection on
/// the actual #5963 fix path (colorless `Add {C}`, resolved through the
/// non-`Priority` fall-through scan of `finish_mana_ability_cost_payment`). Uses
/// the synthetic distinct-amount triggers so the life delta pins each trigger to
/// a single resolution: L1 (creature died, +50) and L2 (permanent sacrificed,
/// +7) sum to 57. A double-collected L2 would read 64, a dropped batch 7/50.
#[test]
fn food_sacrifice_during_payment_fires_both_triggers_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);

    let (l1, l2) = talent_triggers();
    scenario
        .add_creature(P0, "Scavenger's Talent", 0, 0)
        .as_enchantment()
        .with_trigger_definition(l1)
        .with_trigger_definition(l2);

    let goose = scenario
        .add_creature(P0, "Gilded Goose", 0, 2)
        .as_artifact()
        .with_ability_definition(goose_mana_ability())
        .id();

    // Cauldron Familiar is both a creature (its death fires L1) and a Food (a
    // legal "Sacrifice a Food" target for the mana-ability cost).
    let familiar = scenario
        .add_creature(P0, "Cauldron Familiar", 1, 1)
        .with_subtypes(vec!["Food"])
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce spell cast");
    runner
        .act(GameAction::ActivateAbility {
            source_id: goose,
            ability_index: 0,
        })
        .expect("activate the colorless mana ability during payment");

    drive_payment_to_priority(&mut runner, familiar);

    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Graveyard,
        "familiar sacrificed for the mana-ability cost"
    );
    assert_eq!(
        runner.state().players[0].life - life_before,
        L1_GAIN + L2_GAIN,
        "L1 (creature died) and L2 (permanent sacrificed) must each resolve EXACTLY once \
         when the sacrifice pays a mana-ability cost during mana payment"
    );
}

/// Reviewer follow-up (MED): the crux of the prior blocker was the any-color
/// prompt path collecting the cost batch TWICE. Drive an `AnyOneColor` mana
/// ability through `WaitingFor::ChooseManaColor` DURING mana payment and assert
/// its cost-event observer fires EXACTLY once. The cost taps a becomes-tapped
/// observer (`TriggerMode::Taps`); a double scan of the `ChooseManaColor` branch
/// would read 22 instead of 11. This is the exact-once tap-another-creature
/// case the reviewer asked to retain, exercising the color-choice ownership
/// boundary directly (a state-change cost, so it is unaffected by the separate
/// logical-zone-settlement gap that sacrifice-cost + color-prompt exposes).
#[test]
fn tap_another_creature_for_mana_during_payment_fires_becomes_tapped_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);

    let relic = scenario
        .add_creature(P0, "Mana Relic", 0, 3)
        .as_artifact()
        .with_ability_definition(tap_another_mana_ability_any_color())
        .id();

    // "Whenever ~ becomes tapped, you gain TAP_GAIN life." Self-referential so
    // only the observer's own tap fires it (the source's own {T} does not).
    let observer_trigger = TriggerDefinition::new(TriggerMode::Taps)
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: TAP_GAIN },
                player: TargetFilter::Controller,
            },
        ));
    let observer = scenario
        .add_creature(P0, "Tap Observer", 2, 2)
        .with_trigger_definition(observer_trigger)
        .id();
    let _ = relic;

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce spell cast");
    runner
        .act(GameAction::ActivateAbility {
            source_id: relic,
            ability_index: 0,
        })
        .expect("activate the tap-another-creature mana ability during payment");

    let saw_color_choice = drive_payment_to_priority(&mut runner, observer);

    assert!(
        saw_color_choice,
        "the AnyOneColor mana ability must route through the ChooseManaColor prompt"
    );
    assert!(
        runner.state().objects[&observer].tapped,
        "the observer creature was tapped to pay the mana-ability cost"
    );
    assert_eq!(
        runner.state().players[0].life - life_before,
        TAP_GAIN,
        "the becomes-tapped observer must fire EXACTLY once through the ChooseManaColor \
         settlement boundary during mana payment (double-scan would read {})",
        TAP_GAIN * 2
    );
}
