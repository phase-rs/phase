//! Counted plural return-to-hand costs (Ensnare / Thwart class) and the
//! CR 603.10a co-departure stamp on multi-permanent cost moves.
//!
//! Before the fix, "You may return two Islands you control to their owner's
//! hand rather than pay this spell's mana cost." parsed as
//! `AbilityCost::EffectCost { Bounce }`, which the spell payer silently skipped:
//! the spell was free and the Islands stayed on the battlefield.
//!
//! NOTE: the Oracle constants below come from the task author (and Bull
//! Elephant's from an existing repository test); they could not be verified
//! against Scryfall/MTGJSON in the authoring environment.
//!
//! CR 118.9: alternative costs. CR 601.2h: the total cost is paid.
//! CR 400.3: an object put into a hand goes to its owner's hand.
//! CR 603.10a: permanents that leave the battlefield simultaneously "look back"
//! at each other.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, CostObjectCount, Effect,
    FilterProp, QuantityExpr, SpellCastingOption, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use std::sync::Arc;

const ENSNARE: &str = "You may return two Islands you control to their owner's hand rather than pay this spell's mana cost.\nTap all creatures.";
const THWART: &str = "You may return three Islands you control to their owner's hand rather than pay this spell's mana cost.\nCounter target spell.";
const DAZE: &str = "You may return an Island you control to its owner's hand rather than pay this spell's mana cost.\nCounter target spell unless its controller pays {1}.";

fn ensnare_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 3,
    }
}

/// Ensnare in P0's hand, `islands` P0 Islands, an opponent creature and an
/// opponent Island. Returns (runner, ensnare, p0 islands, opponent creature,
/// opponent island).
fn ensnare_board(
    islands: usize,
    plains: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ensnare = scenario
        .add_spell_to_hand_from_oracle(P0, "Ensnare", true, ENSNARE)
        .with_mana_cost(ensnare_mana_cost())
        .id();
    let p0_islands: Vec<ObjectId> = (0..islands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Blue))
        .collect();
    for _ in 0..plains {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let opp_creature = scenario.add_creature(P1, "Opposing Bear", 2, 2).id();
    let opp_island = scenario.add_basic_land(P1, ManaColor::Blue);
    (
        scenario.build(),
        ensnare,
        p0_islands,
        opp_creature,
        opp_island,
    )
}

fn land_you_control() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land).controller(ControllerRef::You))
}

/// A leaves-the-battlefield observer on `host`: whenever a land leaves the
/// battlefield, its controller gains 1 life. The host itself departs with the
/// group, so it sees its co-departed siblings only through the CR 603.10a
/// look-back stamp.
fn attach_land_departure_observer(runner: &mut GameRunner, host: ObjectId) {
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
        .origin(Zone::Battlefield)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("observer host exists");
    obj.trigger_definitions.push(trigger.clone());
    Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
}

/// A no-target instant whose only castable-for-free route is `alt`.
fn instant_with_alt_cost(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand(P0, name, 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::generic(2))
        .id()
}

fn push_alt_cost(runner: &mut GameRunner, spell: ObjectId, cost: AbilityCost) {
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell exists")
        .casting_options
        .push(SpellCastingOption::alternative_cost(cost));
}

/// Battlefield-origin `ZoneChanged` events for `id` in `events`, with their
/// destination and `co_departed` group.
fn departures(events: &[GameEvent], id: ObjectId) -> Vec<(Zone, Vec<ObjectId>)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to,
                record,
            } if *object_id == id => Some((*to, record.co_departed.clone())),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A2: full-card parse wires the counted cost as the alternative cost.
// ---------------------------------------------------------------------------

#[test]
fn ensnare_and_thwart_parse_counted_island_return_alt_cost() {
    let mut scenario = GameScenario::new();
    let ensnare = scenario
        .add_spell_to_hand_from_oracle(P0, "Ensnare", true, ENSNARE)
        .id();
    let thwart = scenario
        .add_spell_to_hand_from_oracle(P0, "Thwart", true, THWART)
        .id();
    let daze = scenario
        .add_spell_to_hand_from_oracle(P0, "Daze", true, DAZE)
        .id();
    let runner = scenario.build();

    let alt_count = |id: ObjectId| -> u32 {
        let obj = &runner.state().objects[&id];
        match obj.casting_options.first().and_then(|o| o.cost.as_ref()) {
            Some(AbilityCost::ReturnToHand {
                count,
                filter: Some(TargetFilter::Typed(tf)),
                from_zone: None,
            }) => {
                assert_eq!(tf.get_subtype(), Some("Island"), "{}", obj.name);
                assert_eq!(tf.controller, Some(ControllerRef::You), "{}", obj.name);
                *count
            }
            other => panic!(
                "{}: expected counted ReturnToHand alt cost, got {other:?}",
                obj.name
            ),
        }
    };
    assert_eq!(alt_count(ensnare), 2);
    assert_eq!(alt_count(thwart), 3);
    // Reach guard: the singular Daze shape still lowers to count 1.
    assert_eq!(alt_count(daze), 1);

    // Vacuity guard: nothing on Ensnare fell to a parser gap.
    let ensnare_obj = &runner.state().objects[&ensnare];
    assert!(
        !format!("{:?}", ensnare_obj.abilities).contains("Unimplemented"),
        "Ensnare's abilities must not contain a parser gap: {:?}",
        ensnare_obj.abilities
    );
}

// ---------------------------------------------------------------------------
// R1: the alternative cost returns exactly two Islands, then taps all creatures.
// ---------------------------------------------------------------------------

#[test]
fn ensnare_alternative_cost_returns_both_islands_then_taps_all() {
    let (mut runner, ensnare, islands, opp_creature, _) = ensnare_board(2, 0);
    let (a, b) = (islands[0], islands[1]);

    let outcome = runner
        .cast(ensnare)
        .accept_optional()
        .pay_cost_with(&[a, b])
        .resolve();

    outcome.assert_zone(&[a, b], Zone::Hand);
    assert!(
        outcome.state().objects[&opp_creature].tapped,
        "Ensnare must tap all creatures"
    );
    // Reach guard: Ensnare resolved.
    outcome.assert_zone(&[ensnare], Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// R2: the prompt asks for exactly two Islands you control and refuses one.
// ---------------------------------------------------------------------------

#[test]
fn ensnare_alternative_cost_prompts_for_two_islands() {
    let (mut runner, ensnare, islands, _, opp_island) = ensnare_board(2, 0);
    let (a, b) = (islands[0], islands[1]);
    let card_id = runner.state().objects[&ensnare].card_id;

    // Explicit `act`: this row asserts the prompt shape the driver hides.
    let result = runner
        .act(GameAction::CastSpell {
            object_id: ensnare,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Ensnare");
    assert!(
        matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "two Islands must offer the alternative cost, got {:?}",
        result.waiting_for
    );
    let result = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept the alternative cost");
    match &result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            choices,
            count,
            ..
        } => {
            assert_eq!(*count, 2);
            assert!(choices.contains(&a) && choices.contains(&b));
            assert!(
                !choices.contains(&opp_island),
                "an opponent's Island is not an Island you control"
            );
        }
        other => panic!("expected PayCost ReturnToHand count 2, got {other:?}"),
    }

    let refused = runner.act(GameAction::SelectCards { cards: vec![a] });
    assert!(refused.is_err(), "one Island must not pay a count-2 cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            count: 2,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// R3: with one Island the alternative cost is not offered.
// ---------------------------------------------------------------------------

#[test]
fn ensnare_one_island_pays_printed_cost_without_alt_offer() {
    let (mut runner, ensnare, islands, _, _) = ensnare_board(1, 3);
    let island = islands[0];
    let card_id = runner.state().objects[&ensnare].card_id;

    // Explicit `act`: this row asserts the ABSENCE of the optional-cost prompt.
    let result = runner
        .act(GameAction::CastSpell {
            object_id: ensnare,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ensnare is castable for its printed cost");
    assert!(
        !matches!(result.waiting_for, WaitingFor::OptionalCostChoice { .. }),
        "one Island cannot pay a count-2 return, so the alternative cost must not be offered"
    );
    assert_eq!(runner.state().objects[&island].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&ensnare].zone, Zone::Stack);
}

// ---------------------------------------------------------------------------
// R4: "their owner's hand" means each permanent's owner.
// ---------------------------------------------------------------------------

#[test]
fn counted_return_cost_sends_each_island_to_its_owners_hand() {
    let (mut runner, ensnare, islands, _, _) = ensnare_board(2, 0);
    let (a, b) = (islands[0], islands[1]);
    // Island `b` is owned by P1 but controlled by P0.
    runner
        .state_mut()
        .objects
        .get_mut(&b)
        .expect("island b exists")
        .owner = P1;

    let outcome = runner
        .cast(ensnare)
        .accept_optional()
        .pay_cost_with(&[a, b])
        .resolve();

    outcome.assert_zone(&[a, b], Zone::Hand);
    let state = outcome.state();
    assert!(state.players[0].hand.contains(&a), "a goes to P0's hand");
    assert!(
        state.players[1].hand.contains(&b),
        "b goes to its owner's (P1's) hand"
    );
    assert!(!state.players[0].hand.contains(&b));
}

// ---------------------------------------------------------------------------
// R5: a counted return cost on an activated ability.
// ---------------------------------------------------------------------------

fn counted_return_activation_board(lands: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Counted Return Activation Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(parse_oracle_cost(
                "Return two lands you control to their owner's hand",
            )),
        )
        .id();
    let lands: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    (scenario.build(), source, lands)
}

#[test]
fn counted_return_activation_cost_returns_both_lands() {
    let (mut runner, source, lands) = counted_return_activation_board(2);
    let (a, b) = (lands[0], lands[1]);
    let life_before = runner.life(P0);

    // Explicit `act`: this row asserts the prompt shape and a refusal.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate the counted-return ability");
    match &result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            count,
            choices,
            ..
        } => {
            assert_eq!(*count, 2);
            assert!(choices.contains(&a) && choices.contains(&b));
        }
        other => panic!("expected PayCost ReturnToHand count 2, got {other:?}"),
    }
    assert!(
        runner
            .act(GameAction::SelectCards { cards: vec![a] })
            .is_err(),
        "one land must not pay a count-2 return"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![a, b] })
        .expect("two lands pay the count-2 return");
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&b].zone, Zone::Hand);
    assert!(runner.state().players[0].hand.contains(&a));
    assert!(runner.state().players[0].hand.contains(&b));

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), life_before + 1, "the ability resolved");

    // Negative: one land cannot pay, so activation is refused.
    let (mut runner, source, _) = counted_return_activation_board(1);
    let refused = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
    });
    assert!(
        matches!(
            refused,
            Err(engine::game::engine::EngineError::ActionNotAllowed(_))
        ),
        "one land cannot pay a count-2 return, got {refused:?}"
    );
}

// ---------------------------------------------------------------------------
// B1: permanents returned together by one cost are stamped co-departed.
// ---------------------------------------------------------------------------

fn return_cost_departure_board(
    islands: usize,
    count: u32,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = instant_with_alt_cost(&mut scenario, "Counted Return Witness");
    let lands: Vec<ObjectId> = (0..islands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Blue))
        .collect();
    let mut runner = scenario.build();
    push_alt_cost(
        &mut runner,
        spell,
        AbilityCost::ReturnToHand {
            count,
            filter: Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Land)
                    .subtype("Island".to_string())
                    .controller(ControllerRef::You),
            )),
            from_zone: None,
        },
    );
    attach_land_departure_observer(&mut runner, lands[0]);
    (runner, spell, lands)
}

#[test]
fn counted_return_cost_co_departed_observer_fires_per_island() {
    let (mut runner, spell, lands) = return_cost_departure_board(2, 2);
    let (a, b) = (lands[0], lands[1]);
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .pay_cost_with(&[a, b])
        .resolve();

    outcome.assert_zone(&[a, b], Zone::Hand);
    assert_eq!(
        departures(outcome.events(), a),
        vec![(Zone::Hand, vec![b])],
        "a's departure must name b as co-departed"
    );
    assert_eq!(
        departures(outcome.events(), b),
        vec![(Zone::Hand, vec![a])],
        "b's departure must name a as co-departed"
    );
    assert_eq!(
        outcome.life_delta(P0),
        2,
        "CR 603.10a: the observer on a sees both Islands leave"
    );

    // Negative sibling: a count-1 return has no co-departure group.
    let (mut runner, spell, lands) = return_cost_departure_board(1, 1);
    let a = lands[0];
    let outcome = runner
        .cast(spell)
        .accept_optional()
        .pay_cost_with(&[a])
        .resolve();
    assert_eq!(departures(outcome.events(), a), vec![(Zone::Hand, vec![])]);
    assert_eq!(outcome.life_delta(P0), 1);
}

// ---------------------------------------------------------------------------
// B2: the same seam stamps a count-2 battlefield exile cost.
// ---------------------------------------------------------------------------

#[test]
fn counted_exile_cost_co_departed_observer_fires_per_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = instant_with_alt_cost(&mut scenario, "Counted Exile Witness");
    let a = scenario.add_basic_land(P0, ManaColor::Green);
    let b = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    push_alt_cost(
        &mut runner,
        spell,
        AbilityCost::Exile {
            count: 2,
            zone: None,
            filter: Some(land_you_control()),
        },
    );
    attach_land_departure_observer(&mut runner, a);

    let outcome = runner
        .cast(spell)
        .accept_optional()
        .pay_cost_with(&[a, b])
        .resolve();

    outcome.assert_zone(&[a, b], Zone::Exile);
    assert_eq!(
        departures(outcome.events(), a),
        vec![(Zone::Exile, vec![b])]
    );
    assert_eq!(
        departures(outcome.events(), b),
        vec![(Zone::Exile, vec![a])]
    );
    assert_eq!(
        outcome.life_delta(P0),
        2,
        "CR 603.10a: the observer on a sees both lands exiled together"
    );
}

// ---------------------------------------------------------------------------
// B3: the stamp group holds only pre-move battlefield residents.
// ---------------------------------------------------------------------------

fn craft_with_creature_materials() -> TargetFilter {
    let battlefield = TargetFilter::Typed(
        TypedFilter::permanent()
            .with_type(TypeFilter::Creature)
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }]),
    );
    let graveyard = TargetFilter::Typed(
        TypedFilter::card()
            .with_type(TypeFilter::Creature)
            .properties(vec![
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
            ]),
    );
    TargetFilter::Or {
        filters: vec![battlefield, graveyard],
    }
}

#[test]
fn craft_mixed_zone_materials_do_not_cross_stamp_graveyard_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blade = scenario
        .add_creature(P0, "Mixed Craft Witness", 0, 0)
        .as_artifact()
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
                    AbilityCost::Mana {
                        cost: ManaCost::generic(1),
                    },
                    AbilityCost::ExileMaterials {
                        materials: craft_with_creature_materials(),
                        count: CostObjectCount::exactly(2),
                    },
                ],
            })
            .sorcery_speed(),
        )
        .id();
    let c = scenario.add_creature(P0, "Battlefield Material", 2, 2).id();
    let g = scenario
        .add_creature_to_graveyard(P0, "Graveyard Material", 2, 2)
        .id();
    let mut runner = scenario.build();
    {
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .expect("P0 exists")
            .mana_pool;
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    // Explicit `act`: the selection spans two zones.
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: blade,
            ability_index: 0,
        })
        .expect("activate the craft-shaped ability");
    match &result.waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileMaterials { .. },
            choices,
            ..
        } => assert!(choices.contains(&c) && choices.contains(&g)),
        other => panic!("expected ExileMaterials PayCost, got {other:?}"),
    }
    let result = runner
        .act(GameAction::SelectCards { cards: vec![c, g] })
        .expect("select one battlefield and one graveyard material");

    // Reach guards: both moves happened in this action.
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Graveyard),
            to: Zone::Exile,
            ..
        } if *object_id == g
    )));
    let c_departures = departures(&result.events, c);
    assert_eq!(c_departures.len(), 1, "c leaves the battlefield once");
    let (to, co_departed) = &c_departures[0];
    assert_eq!(*to, Zone::Exile);
    assert!(
        !co_departed.contains(&g),
        "a graveyard card is never a battlefield co-departure"
    );
    assert!(
        co_departed.is_empty(),
        "one battlefield permanent forms no co-departure group"
    );
}
