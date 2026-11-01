//! Typed and parsed riders isolate the install/entry boundary. Every trigger
//! is produced by an actual creature cast.
//! CR 611.2d + CR 608.2h: resolution-owned quantities are determined once.
//! CR 614.1c: entering-object quantities remain entry replacement work.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastManaObjectScope, CastManaSpentMetric, ChoiceType,
    Comparator, ControllerRef, Effect, EffectScope, ObjectScope, PlayerFilter, PlayerRelation,
    PlayerScope, QuantityExpr, QuantityRef, TapStateChange, TargetFilter, TargetSelectionMode,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

fn quantity(qty: QuantityRef) -> QuantityExpr {
    QuantityExpr::Ref { qty }
}

fn population() -> QuantityExpr {
    quantity(QuantityRef::ObjectCount {
        filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
    })
}

fn counter(count: QuantityExpr) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count,
            target: TargetFilter::SelfRef,
        },
    )
}

fn source(scenario: &mut GameScenario, execute: AbilityDefinition) -> ObjectId {
    // Use the production parser for the spell-binding placeholder and lifecycle;
    // only the counter expression/chain is synthetic.
    const BREWING: &str = "When this enchantment enters, any number of target opponents each draw a card. Put an ingredient counter on this enchantment, then put an ingredient counter on it for each card drawn this way.\nWhenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of ingredient counters on this enchantment.";
    let mut trigger = engine::parser::parse_oracle_text(BREWING, "Communal Brewing", &[], &[], &[])
        .triggers
        .into_iter()
        .find(|trigger| trigger.mode == TriggerMode::SpellCast)
        .expect("Brewing must parse its cast trigger");
    let Effect::AddTargetReplacement {
        replacement,
        target: TargetFilter::None,
    } = trigger
        .execute
        .as_mut()
        .expect("trigger must install a rider")
        .effect
        .as_mut()
    else {
        panic!("Brewing must install a floating rider");
    };
    replacement.execute = Some(Box::new(execute));
    scenario
        .add_creature(P0, "Synthetic rider source", 2, 2)
        .with_subtypes(vec!["Dog"])
        .with_mana_cost(ManaCost::generic(1))
        .with_trigger_definition(trigger)
        .id()
}

fn entrant(scenario: &mut GameScenario) -> ObjectId {
    let id = scenario
        .add_creature_to_hand(P0, "Timing entrant", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 6,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Green, ManaType::Blue]
            .into_iter()
            .chain((0..6).map(|_| ManaType::Colorless))
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    id
}

fn install(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "cast produced its rider trigger"
    );
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1, "only the creature remains");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(runner.state().pending_damage_replacements.len(), 1);
    assert!(runner.state().current_trigger_event.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn finish(runner: &mut GameRunner, spell: ObjectId, expected: u32) {
    runner.advance_until_stack_empty();
    let object = &runner.state().objects[&spell];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "reach guard: entrant resolved"
    );
    assert_eq!(
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        expected,
    );
    assert_eq!(runner.state().stack.len(), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().pending_damage_replacements[0].is_consumed);
}

fn removal(scenario: &mut GameScenario, victim: ObjectId) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Synthetic population response", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::Destroy {
            target: TargetFilter::SpecificObject { id: victim },
            cant_regenerate: false,
        })
        .id()
}

fn remove_after_install(runner: &mut GameRunner, response: ObjectId, victim: ObjectId) {
    runner.cast(response).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 1);
}

fn other_dog_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    const RIDER: &str = "Whenever you cast a Human creature spell, that creature enters with an additional +1/+1 counter on it for each other Dog you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Other Dog rider source", 2, 2, RIDER)
        .with_subtypes(vec!["Dog"])
        .id();
    let dog = scenario
        .add_creature(P0, "Counted other Dog", 2, 2)
        .with_subtypes(vec!["Dog"])
        .id();
    let response = removal(&mut scenario, dog);
    let spell = scenario
        .add_creature_to_hand(P0, "Human entrant", 1, 1)
        .with_subtypes(vec!["Human"])
        .with_mana_cost(ManaCost::zero())
        .id();
    (scenario, spell, dog, response)
}

#[test]
fn other_dog_population_excludes_source_without_response() {
    let (scenario, spell, _, _) = other_dog_scenario();
    let mut runner = scenario.build();
    install(&mut runner, spell);
    finish(&mut runner, spell, 1);
}

#[test]
fn other_dog_removed_before_trigger_resolution_is_not_counted() {
    let (scenario, spell, dog, response) = other_dog_scenario();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    assert_eq!(runner.state().stack.len(), 2);
    runner.cast(response).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&dog].zone, Zone::Graveyard);
    assert_eq!(runner.state().stack.len(), 2);
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    finish(&mut runner, spell, 0);
}

#[test]
fn other_dog_removed_after_trigger_resolution_remains_counted() {
    let (scenario, spell, dog, response) = other_dog_scenario();
    let mut runner = scenario.build();
    install(&mut runner, spell);
    remove_after_install(&mut runner, response, dog);
    // CR 611.2d + CR 608.2h: "other" is source-relative, not an entry-time read.
    finish(&mut runner, spell, 1);
}

#[test]
fn recipient_controller_hand_size_is_frozen_without_an_entering_binding() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::HandSize {
            player: PlayerScope::RecipientController,
        })),
    );
    let dog = scenario.add_creature(P0, "Response victim", 2, 2).id();
    let response = removal(&mut scenario, dog);
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    remove_after_install(&mut runner, response, dog);
    // CR 611.2d + CR 608.2h: the response was still in hand at install.
    // RecipientController does not use the entering-object fallback.
    finish(&mut runner, spell, 1);
}

#[test]
fn nested_player_threshold_keeps_entering_object_mana_value_live() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::PlayerCount {
            filter: PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer,
                }),
                comparator: Comparator::LE,
                value: Box::new(QuantityExpr::Offset {
                    inner: Box::new(quantity(QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient,
                    })),
                    offset: 12,
                }),
            },
        })),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 614.1c: the preserved threshold is entrant MV8 + 12 = 20, so both
    // players at 20 life satisfy "life <= threshold". Freezing the recipient read
    // at install would use source MV1 + 12 = 13 and count neither player, so the
    // positive count of 2 also proves the player census itself was evaluated.
    finish(&mut runner, spell, 2);
}

#[test]
fn nested_player_threshold_freezes_event_mana_value_before_context_clears() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::PlayerCount {
            filter: PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer,
                }),
                comparator: Comparator::LE,
                value: Box::new(QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Sum {
                        exprs: vec![
                            quantity(QuantityRef::ObjectManaValue {
                                scope: ObjectScope::Recipient,
                            }),
                            quantity(QuantityRef::ObjectManaValue {
                                scope: ObjectScope::EventSource,
                            }),
                        ],
                    }),
                    offset: 4,
                }),
            },
        })),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    // install asserts the trigger context is cleared before the entrant resolves.
    install(&mut runner, spell);
    // CR 608.2h + CR 614.1c: entrant MV8 + snapshotted event MV8 + 4 = 20.
    // Both players at 20 life qualify; a late event read gives 12 and counts none.
    finish(&mut runner, spell, 2);
}

#[test]
fn nested_player_threshold_rebinds_scoped_hand_size_per_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::PlayerCount {
            filter: PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::LifeTotal {
                    player: PlayerScope::ScopedPlayer,
                }),
                comparator: Comparator::LE,
                value: Box::new(QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Sum {
                        exprs: vec![
                            quantity(QuantityRef::ObjectManaValue {
                                scope: ObjectScope::Recipient,
                            }),
                            quantity(QuantityRef::HandSize {
                                player: PlayerScope::ScopedPlayer,
                            }),
                        ],
                    }),
                    offset: 11,
                }),
            },
        })),
    );
    scenario.add_creature_to_hand(P1, "Candidate hand card", 1, 1);
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 608.2h + CR 614.1c: after casting, P0 has no cards and P1 has one.
    // Their candidate-relative thresholds are MV8 + 0 + 11 = 19 and
    // MV8 + 1 + 11 = 20. Only P1 qualifies; freezing HandSize would count none.
    finish(&mut runner, spell, 1);
}

#[test]
fn resolution_only_event_mana_value_survives_cleared_trigger_context() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::ObjectManaValue {
            scope: ObjectScope::EventSource,
        })),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 608.2h: read the triggering spell's MV8, not source MV1 or absent context.
    finish(&mut runner, spell, 8);
}

#[test]
fn resolution_only_triggering_spell_mana_spent_survives_cleared_context() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(quantity(QuantityRef::ManaSpentToCast {
            scope: CastManaObjectScope::TriggeringSpell,
            metric: CastManaSpentMetric::DistinctColors,
        })),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 608.2h: the trigger's spell payment is read during resolution.
    finish(&mut runner, spell, 2);
}

#[test]
fn tapped_then_dynamic_counter_prefix_freezes_population_before_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .sub_ability(counter(population())),
    );
    let victim = scenario.add_creature(P0, "Counted creature", 2, 2).id();
    let response = removal(&mut scenario, victim);
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    remove_after_install(&mut runner, response, victim);
    // CR 611.2d: the two-creature population was determined at install.
    finish(&mut runner, spell, 2);
    assert!(runner.state().objects[&spell].tapped);
}

#[test]
fn mixed_arithmetic_freezes_population_but_keeps_recipient_live() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(QuantityExpr::Sum {
            exprs: vec![
                population(),
                quantity(QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient,
                }),
            ],
        }),
    );
    let victim = scenario.add_creature(P0, "Counted creature", 2, 2).id();
    let response = removal(&mut scenario, victim);
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    remove_after_install(&mut runner, response, victim);
    // CR 611.2d + CR 614.1c: population stays two; recipient MV8 is read at entry.
    finish(&mut runner, spell, 10);
}

#[test]
fn mixed_arithmetic_event_mana_value_survives_cleared_trigger_context() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    source(
        &mut scenario,
        counter(QuantityExpr::Sum {
            exprs: vec![
                quantity(QuantityRef::ObjectManaValue {
                    scope: ObjectScope::EventSource,
                }),
                quantity(QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient,
                }),
            ],
        }),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 608.2h + CR 614.1c: the cast spell and entering recipient each have
    // MV8 (six generic, one green, one blue), even after trigger context clears.
    finish(&mut runner, spell, 16);
}

#[test]
fn source_arithmetic_is_frozen_before_source_counters_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = source(
        &mut scenario,
        counter(QuantityExpr::Offset {
            inner: Box::new(quantity(QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(CounterType::Plus1Plus1),
            })),
            offset: -1,
        }),
    );
    scenario.with_counter(source, CounterType::Plus1Plus1, 4);
    let response = scenario
        .add_spell_to_hand(P0, "Synthetic counter response", true)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::SpecificObject { id: source },
        })
        .id();
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    runner.cast(response).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&source].counters[&CounterType::Plus1Plus1],
        6
    );
    // CR 611.2d: preserve signed arithmetic's resolution-time result (4 - 1).
    finish(&mut runner, spell, 3);
}

#[test]
fn choice_boundary_leaves_post_entry_dynamic_work_unfrozen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let choose = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: ChoiceType::CreatureType {
                options: vec!["Dog".into(), "Wolf".into()],
            },
            // Merge collision: the branch's `chooser` field postdates this
            // upstream test. `You` is the historic controller-choice default.
            chooser: ControllerRef::You,
            persist: true,
            selection: TargetSelectionMode::Chosen,
        },
    )
    .sub_ability(counter(quantity(QuantityRef::ObjectCount {
        filter: TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Dog".into())
                .controller(ControllerRef::You),
        ),
    })));
    let source = source(
        &mut scenario,
        counter(QuantityExpr::Fixed { value: 1 }).sub_ability(choose),
    );
    let victim = scenario
        .add_creature(P0, "Counted creature", 2, 2)
        .with_subtypes(vec!["Dog"])
        .id();
    let response = removal(&mut scenario, victim);
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    remove_after_install(&mut runner, response, victim);
    runner.advance_until_stack_empty();
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "post-entry choice must be reached: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(options, &["Dog".to_string(), "Wolf".to_string()]);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&spell].counters[&CounterType::Plus1Plus1],
        1
    );
    // The prefix applied, but the dynamic tail is not evaluated until the choice.
    runner
        .act(GameAction::ChooseOption {
            choice: "Wolf".into(),
        })
        .expect("choose Wolf");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    // CR 608.2h: the tail now sees one Dog, not the two present at install.
    finish(&mut runner, spell, 2);
}

#[test]
fn fixed_counter_types_in_the_modifier_prefix_each_apply_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charge = CounterType::Generic("charge".into());
    source(
        &mut scenario,
        counter(QuantityExpr::Fixed { value: 3 }).sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: charge.clone(),
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            },
        )),
    );
    let spell = entrant(&mut scenario);
    let mut runner = scenario.build();
    install(&mut runner, spell);
    // CR 614.1c: both fixed modifications belong to the same entry event.
    finish(&mut runner, spell, 3);
    assert_eq!(runner.state().objects[&spell].counters[&charge], 2);
}
