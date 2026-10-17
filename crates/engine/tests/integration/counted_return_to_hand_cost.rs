//! Counted plural return-to-hand costs (Ensnare / Thwart class) and the
//! CR 603.10a co-departure stamp on multi-permanent cost moves.
//!
//! Before the fix, "You may return two Islands you control to their owner's
//! hand rather than pay this spell's mana cost." parsed as
//! `AbilityCost::EffectCost { Bounce }`, which the spell payer silently skipped:
//! the spell was free and the Islands stayed on the battlefield.
//!
//! NOTE: The Oracle constants below equal the `data/card-data.json` export
//! (MTGJSON-derived) text for Ensnare, Thwart and Daze; Gush's equals Scryfall's.
//!
//! CR 118.9: alternative costs. CR 601.2h: the total cost is paid.
//! CR 400.3: an object put into a hand goes to its owner's hand.
//! CR 603.10a: permanents that leave the battlefield simultaneously "look back"
//! at each other.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, CostObjectCount, Effect,
    FilterProp, QuantityExpr, ReplacementDefinition, SpellCastingOption, TargetFilter,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
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
            min_count,
            ..
        } => {
            assert_eq!(*count, 2);
            assert_eq!(*min_count, 2, "the prompt requires the exact count");
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

// ---------------------------------------------------------------------------
// C-A2: a count-3 cost refuses a repeated permanent (Thwart-sized).
// ---------------------------------------------------------------------------

#[test]
fn counted_return_cost_refuses_a_repeated_permanent() {
    let (mut runner, spell, lands) = return_cost_departure_board(3, 3);
    let (a, b, c) = (lands[0], lands[1], lands[2]);
    let card_id = runner.state().objects[&spell].card_id;

    // Explicit `act`: this row asserts a refusal at the payment boundary.
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the counted-return witness");
    let result = runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept the alternative cost");
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                count: 3,
                ..
            }
        ),
        "reach guard: the count-3 prompt, got {:?}",
        result.waiting_for
    );

    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![a, b, b]
            })
            .is_err(),
        "[a, b, b] must not pay a count-3 cost"
    );
    for id in [a, b, c] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![a, b, c],
        })
        .expect("three distinct lands pay");
    for id in [a, b, c] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
    }
}

// ---------------------------------------------------------------------------
// C-A1: the activation route keeps the group across a replacement pause.
// ---------------------------------------------------------------------------

/// "If a land would be put into a hand from anywhere, exile it instead",
/// scoped to `id`.
fn redirect_land_to_exile(id: ObjectId) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Hand)
        .valid_card(TargetFilter::SpecificObject { id })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
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

/// The counted-return activation with each listed land carrying two competing
/// replacements. Returns (life gained, persisted departure records of the
/// lands, pauses taken).
fn counted_return_activation_with_pauses(
    redirect: &[usize],
) -> (i32, Vec<(ObjectId, Vec<ObjectId>)>, usize) {
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
    let lands: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    for &index in redirect {
        for name in ["First Redirect", "Second Redirect"] {
            scenario
                .add_creature(P0, &format!("{name} {index}"), 0, 0)
                .as_enchantment()
                .with_replacement_definition(redirect_land_to_exile(lands[index]));
        }
    }
    let mut runner = scenario.build();
    attach_land_departure_observer(&mut runner, lands[0]);
    let life0 = runner.life(P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate the counted-return ability");
    runner
        .act(GameAction::SelectCards {
            cards: lands.clone(),
        })
        .expect("select both lands");
    let mut pauses = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        pauses += 1;
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose the replacement");
    }
    runner.advance_until_stack_empty();

    let mut records: Vec<(ObjectId, Vec<ObjectId>)> = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .filter(|record| {
            record.from_zone == Some(Zone::Battlefield) && lands.contains(&record.object_id)
        })
        .map(|record| (record.object_id, record.co_departed.clone()))
        .collect();
    records.sort();
    let mut expected = lands.clone();
    expected.sort();
    assert_eq!(
        records.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected,
        "reach guard: both lands left the battlefield"
    );
    (runner.life(P0) - life0, records, pauses)
}

#[test]
fn counted_return_activation_group_survives_a_replacement_pause() {
    // Unpaused sibling: the ability's own life (1) plus the observer seeing
    // both lands (2). The pause shapes must reach the same total.
    let (life, records, pauses) = counted_return_activation_with_pauses(&[]);
    assert_eq!(pauses, 0);
    assert_eq!(life, 3);
    let (a, b) = (records[0].0, records[1].0);
    assert_eq!(records, vec![(a, vec![b]), (b, vec![a])]);

    for (label, redirect, expected_pauses) in [("second", vec![1], 1), ("both", vec![0, 1], 2)] {
        let (life, records, pauses) = counted_return_activation_with_pauses(&redirect);
        assert_eq!(
            pauses, expected_pauses,
            "{label}: pause count (reach guard)"
        );
        let (a, b) = (records[0].0, records[1].0);
        assert_eq!(
            records,
            vec![(a, vec![b]), (b, vec![a])],
            "{label}: the persisted records stay mutual"
        );
        assert_eq!(life, 3, "{label}: CR 603.10a observer sees both lands");
    }
}

/// A target-bearing counted-return activation (the Flooded Shoreline shape:
/// "Return two lands you control to their owner's hand: Destroy target
/// creature"), with the first land carrying the departure observer and the
/// listed lands each carrying two competing replacements. Returns (life
/// gained, persisted departure records of the lands, pauses taken, the
/// opposing creature's zone).
fn targeted_counted_return_activation_with_pauses(
    redirect: &[usize],
) -> (i32, Vec<(ObjectId, Vec<ObjectId>)>, usize, Zone) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Counted Return Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    ),
                    cant_regenerate: false,
                },
            )
            .cost(parse_oracle_cost(
                "Return two lands you control to their owner's hand",
            )),
        )
        .id();
    let victim = scenario.add_creature(P1, "Opposing Victim", 2, 2).id();
    let lands: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    for &index in redirect {
        for name in ["First Redirect", "Second Redirect"] {
            scenario
                .add_creature(P0, &format!("{name} {index}"), 0, 0)
                .as_enchantment()
                .with_replacement_definition(redirect_land_to_exile(lands[index]));
        }
    }
    let mut runner = scenario.build();
    attach_land_departure_observer(&mut runner, lands[0]);
    let life0 = runner.life(P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate the targeted counted-return ability");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(victim)],
            })
            .expect("announce the target");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                count: 2,
                min_count: 2,
                ..
            }
        ),
        "the activation prompt requires the exact count, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: lands.clone(),
        })
        .expect("select both lands");
    let mut pauses = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        pauses += 1;
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose the replacement");
    }
    runner.advance_until_stack_empty();

    let mut records: Vec<(ObjectId, Vec<ObjectId>)> = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .filter(|record| {
            record.from_zone == Some(Zone::Battlefield) && lands.contains(&record.object_id)
        })
        .map(|record| (record.object_id, record.co_departed.clone()))
        .collect();
    records.sort();
    let mut expected = lands.clone();
    expected.sort();
    assert_eq!(
        records.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected,
        "reach guard: both lands left the battlefield"
    );
    let victim_zone = runner.state().objects[&victim].zone;
    (runner.life(P0) - life0, records, pauses, victim_zone)
}

#[test]
fn targeted_counted_return_activation_group_survives_a_replacement_pause() {
    // CR 603.10a + CR 603.2c: the observer on the first land triggers once for
    // each of the two lands that left together, however the departures were
    // split across actions by a replacement choice.
    let (life, records, pauses, victim) = targeted_counted_return_activation_with_pauses(&[]);
    assert_eq!(pauses, 0);
    assert_eq!(life, 2, "unpaused: the observer sees both lands");
    assert_eq!(victim, Zone::Graveyard, "the announced target is destroyed");
    let (a, b) = (records[0].0, records[1].0);
    assert_eq!(records, vec![(a, vec![b]), (b, vec![a])]);

    for (label, redirect, expected_pauses) in [("second", vec![1], 1), ("both", vec![0, 1], 2)] {
        let (life, records, pauses, victim) =
            targeted_counted_return_activation_with_pauses(&redirect);
        assert_eq!(
            pauses, expected_pauses,
            "{label}: pause count (reach guard)"
        );
        let (a, b) = (records[0].0, records[1].0);
        assert_eq!(
            records,
            vec![(a, vec![b]), (b, vec![a])],
            "{label}: the persisted records stay mutual"
        );
        assert_eq!(life, 2, "{label}: the observer triggers once per land");
        assert_eq!(victim, Zone::Graveyard, "{label}: the ability resolved");
    }
}

// ---------------------------------------------------------------------------
// The payer leaves the game while a counted return is paused (CR 800.4a).
// ---------------------------------------------------------------------------

/// A three-player board: P0 activates the target-bearing counted-return
/// ability, the second land carries two competing replacements (the pause),
/// and P1's observer gains life whenever any land leaves the battlefield.
/// Pauses on the second land, optionally round-trips the paused state through
/// serde, then P0 concedes. Returns (P1 life gained, stack depth after the
/// triggers settle, the first land's zone, the second land's zone).
fn concede_during_counted_return_pause(round_trip: bool) -> (i32, usize, Zone, Zone) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Targeted Counted Return Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    ),
                    cant_regenerate: false,
                },
            )
            .cost(parse_oracle_cost(
                "Return two lands you control to their owner's hand",
            )),
        )
        .id();
    let victim = scenario.add_creature(P1, "Opposing Victim", 2, 2).id();
    let observer_host = scenario
        .add_creature(P1, "Land Departure Watcher", 1, 1)
        .id();
    let lands: Vec<ObjectId> = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    for name in ["First Redirect", "Second Redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_land_to_exile(lands[1]));
    }
    let mut runner = scenario.build();
    attach_land_departure_observer(&mut runner, observer_host);
    let life0 = runner.life(P1);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate the targeted counted-return ability");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(victim)],
            })
            .expect("announce the target");
    }
    runner
        .act(GameAction::SelectCards {
            cards: lands.clone(),
        })
        .expect("select both lands");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the second land's move paused, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&lands[0]].zone,
        Zone::Hand,
        "reach guard: the first land already left"
    );
    if round_trip {
        let json = serde_json::to_string(runner.state()).expect("serialize the paused state");
        assert!(
            json.contains("deferred_cost_events"),
            "reach guard: the Cast root carries the completed prefix"
        );
        runner = GameRunner::from_state(
            serde_json::from_str(&json).expect("deserialize the paused state"),
        );
    }

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes during the replacement choice");
    assert!(runner.state().players[0].is_eliminated);
    runner.advance_until_stack_empty();
    (
        runner.life(P1) - life0,
        runner.state().stack.len(),
        // The conceding player's objects have left the game with them; zone
        // reads fall back to the recorded zone.
        runner
            .state()
            .objects
            .get(&lands[0])
            .map_or(Zone::Exile, |o| o.zone),
        runner
            .state()
            .objects
            .get(&lands[1])
            .map_or(Zone::Exile, |o| o.zone),
    )
}

#[test]
fn conceding_during_a_counted_return_pause_publishes_the_completed_prefix_once() {
    // CR 800.4a + CR 603.2: the first land's departure already happened, so a
    // surviving player's leaves-the-battlefield observer triggers for it once,
    // although the payer then leaves and the paused move is abandoned.
    for round_trip in [false, true] {
        let (gained, stack, _, _) = concede_during_counted_return_pause(round_trip);
        assert_eq!(
            gained, 1,
            "round_trip={round_trip}: P1's observer triggers exactly once"
        );
        assert_eq!(stack, 0, "round_trip={round_trip}: the stack settled");
    }
}

// ---------------------------------------------------------------------------
// P4: the printed activated counted-return costs (verbatim card-data text).
// ---------------------------------------------------------------------------

fn find_return_to_hand_cost(cost: &AbilityCost) -> Option<(u32, Option<TargetFilter>)> {
    match cost {
        AbilityCost::ReturnToHand { count, filter, .. } => Some((*count, filter.clone())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_return_to_hand_cost),
        _ => None,
    }
}

#[test]
fn printed_activated_counted_return_costs_carry_their_counts() {
    let cards: [(&str, &str, u32); 7] = [
        (
            "Flooded Shoreline",
            "{U}{U}, Return two Islands you control to their owner's hand: Return target creature to its owner's hand.",
            2,
        ),
        (
            "Multani, Yavimaya's Avatar",
            "Reach, trample\nMultani gets +1/+1 for each land you control and each land card in your graveyard.\n{1}{G}, Return two lands you control to their owner's hand: Return this card from your graveyard to your hand.",
            2,
        ),
        (
            "Soratami Seer",
            "Flying\n{4}, Return two lands you control to their owner's hand: Discard all the cards in your hand, then draw that many cards.",
            2,
        ),
        (
            "Uyo, Silent Prophet",
            "Flying\n{2}, Return two lands you control to their owner's hand: Copy target instant or sorcery spell. You may choose new targets for the copy.",
            2,
        ),
        (
            "Soratami Mirror-Mage",
            "Flying\n{3}, Return three lands you control to their owner's hand: Return target creature to its owner's hand.",
            3,
        ),
        (
            "Pearl Lake Ancient",
            "Flash\nThis spell can't be countered.\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nReturn three lands you control to their owner's hand: Return this creature to its owner's hand.",
            3,
        ),
        (
            "Icebreaker Kraken",
            "Affinity for snow lands (This spell costs {1} less to cast for each snow land you control.)\nWhen this creature enters, artifacts and creatures target opponent controls don't untap during that player's next untap step.\nReturn three snow lands you control to their owner's hand: Return this creature to its owner's hand.",
            3,
        ),
    ];
    let mut scenario = GameScenario::new();
    let ids: Vec<(ObjectId, &str, u32)> = cards
        .iter()
        .map(|(name, text, count)| {
            (
                scenario.add_creature_from_oracle(P0, name, 1, 1, text).id(),
                *name,
                *count,
            )
        })
        .collect();
    let runner = scenario.build();
    for (id, name, count) in ids {
        let found = runner.state().objects[&id]
            .abilities
            .iter()
            .filter_map(|ability| ability.cost.as_ref())
            .find_map(find_return_to_hand_cost);
        let (found_count, filter) =
            found.unwrap_or_else(|| panic!("{name}: no counted ReturnToHand activation cost"));
        assert_eq!(found_count, count, "{name}");
        let Some(TargetFilter::Typed(tf)) = filter else {
            panic!("{name}: expected a typed land filter, got {filter:?}");
        };
        if name == "Flooded Shoreline" {
            assert_eq!(tf.get_subtype(), Some("Island"));
        }
        if name == "Icebreaker Kraken" {
            assert!(
                tf.properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::HasSupertype {
                        value: Supertype::Snow
                    }
                )),
                "{name}: snow lands, got {:?}",
                tf.properties
            );
        }
    }
}
