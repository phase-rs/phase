//! #9884: synthetic classifier-contract fixtures, driven through casting or
//! activation. Asymmetric populations distinguish the recipient from the source.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction, CardTypeSetSource,
    ControllerRef, Duration, Effect, FilterProp, ObjectProperty, ObjectScope, PlayerScope,
    PropertyAggregate, PtValue, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

fn other_creatures() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Another]),
    )
}

fn quantity(qty: QuantityRef) -> QuantityExpr {
    QuantityExpr::Ref { qty }
}

fn counter_count(
    state: &engine::types::game_state::GameState,
    id: ObjectId,
    kind: CounterType,
) -> u32 {
    state.objects[&id].counters.get(&kind).copied().unwrap_or(0)
}

// CR 608.2h + CR 613.4c: resolve the boost for its recipient, not the
// creature that generated the ability. Source contributes 5, target 2, third 3.
#[test]
fn issue_9884_pump_aggregate_excludes_recipient_not_source() {
    for property in [ObjectProperty::ManaValue, ObjectProperty::Power] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let amount = quantity(QuantityRef::PropertyAggregate(
            PropertyAggregate::new(
                AggregateFunction::Sum,
                property,
                CardTypeSetSource::Objects {
                    filter: other_creatures(),
                },
            )
            .unwrap(),
        ));
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Pump {
                power: PtValue::Quantity(amount.clone()),
                toughness: PtValue::Quantity(amount),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .cost(AbilityCost::Tap)
        .duration(Duration::UntilEndOfTurn);
        let source = scenario
            .add_creature(P0, "Aggregate source", 5, 5)
            .with_mana_cost(ManaCost::Cost {
                generic: 5,
                shards: vec![],
            })
            .with_ability_definition(ability)
            .id();
        let target = scenario
            .add_creature(P0, "Aggregate recipient", 2, 2)
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })
            .id();
        scenario
            .add_creature(P0, "Third contribution", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            });
        let mut runner = scenario.build();
        let outcome = runner.activate(source, 0).target_object(target).resolve();
        assert_eq!(
            outcome.state().objects[&target].power,
            Some(10),
            "{property:?}: 2 + (5 + 3), not 2 + (2 + 3)"
        );
        assert_eq!(outcome.state().objects[&target].toughness, Some(10));
        assert_eq!(outcome.state().objects[&source].power, Some(5));
    }
}

// CR 122.1 + CR 608.2h: count the other creatures' Time counters before
// applying additions. X must survive the per-recipient resolver branch.
#[test]
fn issue_9884_put_counter_all_keeps_x_and_excludes_each_recipient() {
    for with_x in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let empty = scenario.add_creature(P0, "Zero time counters", 2, 2).id();
        let holder = scenario.add_creature(P0, "Two time counters", 2, 2).id();
        scenario.with_counter(holder, CounterType::Time, 2);
        let others = quantity(QuantityRef::CountersOnObjects {
            counter_type: Some(CounterType::Time),
            filter: other_creatures(),
        });
        let count = if with_x {
            QuantityExpr::Sum {
                exprs: vec![quantity(QuantityRef::Variable { name: "X".into() }), others],
            }
        } else {
            others
        };
        let spell = scenario
            .add_spell_to_hand(P0, "Recipient counters", false)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: if with_x {
                    vec![ManaCostShard::X]
                } else {
                    vec![]
                },
            })
            .with_ability(Effect::PutCounterAll {
                counter_type: CounterType::Plus1Plus1,
                count,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            })
            .id();
        scenario.with_mana_pool(
            P0,
            (0..5)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let cast = runner.cast(spell);
        let outcome = if with_x {
            cast.x(5).resolve()
        } else {
            cast.resolve()
        };
        let x = if with_x { 5 } else { 0 };
        assert_eq!(
            counter_count(outcome.state(), empty, CounterType::Plus1Plus1),
            x + 2
        );
        assert_eq!(
            counter_count(outcome.state(), holder, CounterType::Plus1Plus1),
            x,
            "the recipient's own two counters do not contribute"
        );
        assert_eq!(counter_count(outcome.state(), holder, CounterType::Time), 2);
    }
}

// CR 404.1 + CR 608.2h: each recipient reads its own controller's graveyard.
#[test]
fn issue_9884_player_scope_varies_between_recipient_controllers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own recipient", 2, 2).id();
    let opposing = scenario.add_creature(P1, "Opposing recipient", 2, 2).id();
    scenario.add_creature_to_graveyard(P0, "Own graveyard card", 1, 1);
    for _ in 0..3 {
        scenario.add_creature_to_graveyard(P1, "Opposing graveyard card", 1, 1);
    }
    let spell = scenario
        .add_spell_to_hand(P0, "Controller graveyards", false)
        .with_ability(Effect::PutCounterAll {
            counter_type: CounterType::Plus1Plus1,
            count: quantity(QuantityRef::GraveyardSize {
                player: PlayerScope::RecipientController,
            }),
            target: TargetFilter::Typed(TypedFilter::creature()),
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        counter_count(outcome.state(), own, CounterType::Plus1Plus1),
        1
    );
    assert_eq!(
        counter_count(outcome.state(), opposing, CounterType::Plus1Plus1),
        3
    );
}

// Classifier contract: CanEnchant consults an Aura's runtime enchant filter.
// Here the recipient and ParentTarget are the same creature; the hidden Another
// predicate fails only when the recipient is bound (base: evaluated against the
// Aura, so both Auras pass).
#[test]
fn issue_9884_can_enchant_reads_hidden_recipient_filter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P0, "Enchant reference and recipient", 2, 2)
        .id();
    scenario
        .add_enchantment_from_oracle(P0, "Hidden Another Aura", "")
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Another]),
        )));
    scenario
        .add_enchantment_from_oracle(P0, "Unconditional Aura control", "")
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )));
    let amount = quantity(QuantityRef::ObjectCount {
        filter: TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::CanEnchant {
            target: Box::new(TargetFilter::ParentTarget),
        }])),
    });
    let spell = scenario
        .add_spell_to_hand(P0, "Hidden filter pump", false)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: PtValue::Quantity(amount),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            )
            .duration(Duration::UntilEndOfTurn),
        )
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert_eq!(
        outcome.state().objects[&target].power,
        Some(3),
        "only the unconditional Aura can enchant the bound recipient, not both Auras"
    );
}

// Classifier contract, not a printed card: composite characteristic reads stay
// unbound while a pure population-identity rider binds the uncreated token.
// CR 614.1c + CR 608.2h: both entry amounts use the pre-entry population.
#[test]
fn issue_9884_token_entry_composite_stays_unbound_with_identity_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let other_count = quantity(QuantityRef::ObjectCount {
        filter: other_creatures(),
    });
    let mixed = QuantityExpr::Sum {
        exprs: vec![
            other_count.clone(),
            quantity(QuantityRef::Intensity {
                scope: ObjectScope::Recipient,
            }),
        ],
    };
    let source = scenario
        .add_creature(P0, "Creature token source", 2, 2)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Token {
                    name: "Classifier token".into(),
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    types: vec!["Creature".into()],
                    colors: vec![],
                    keywords: vec![],
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    owner: TargetFilter::Controller,
                    attach_to: None,
                    enters_attacking: false,
                    attack_destination:
                        engine::types::ability::TokenAttackDestination::ControllerChooses,
                    supertypes: vec![],
                    static_abilities: vec![],
                    enter_with_counters: vec![
                        (CounterType::Plus1Plus1, mixed),
                        (CounterType::Time, other_count),
                    ],
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();
    scenario.add_creature(P0, "Other one", 2, 2);
    scenario.add_creature(P0, "Other two", 2, 2);
    let mut runner = scenario.build();
    let outcome = runner.activate(source, 0).resolve();
    let token = *outcome
        .state()
        .last_created_token_ids
        .last()
        .expect("created token");
    assert_eq!(
        counter_count(outcome.state(), token, CounterType::Plus1Plus1),
        2,
        "unbound composite excludes the creature source: N-1"
    );
    assert_eq!(
        counter_count(outcome.state(), token, CounterType::Time),
        3,
        "pure identity rider binds the uncreated token and counts all N creatures"
    );
}
