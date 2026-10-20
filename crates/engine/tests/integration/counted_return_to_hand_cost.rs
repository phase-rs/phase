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
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, CostObjectCount,
    DelayedTriggerCondition, Effect, FilterProp, QuantityExpr, ReplacementDefinition,
    SpellCastingOption, TargetFilter, TargetRef, TriggerDefinition, TypeFilter, TypedFilter,
    WheneverEventExpiry,
};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, CostResume, PayCostKind, PendingCostMoveResume, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use std::sync::Arc;

const ENSNARE: &str = "You may return two Islands you control to their owner's hand rather than pay this spell's mana cost.\nTap all creatures.";
const THWART: &str = "You may return three Islands you control to their owner's hand rather than pay this spell's mana cost.\nCounter target spell.";
const DAZE: &str = "You may return an Island you control to its owner's hand rather than pay this spell's mana cost.\nCounter target spell unless its controller pays {1}.";
const P2: PlayerId = PlayerId(2);

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
    attach_departure_observer(
        runner,
        host,
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
        1,
        None,
    );
}

fn attach_departure_observer(
    runner: &mut GameRunner,
    host: ObjectId,
    subject: TargetFilter,
    amount: i32,
    destination: Option<Zone>,
) {
    let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(subject)
        .origin(Zone::Battlefield)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
        ));
    trigger.destination = destination;
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("observer host exists");
    Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger);
    obj.materialize_base_trigger_definitions();
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
/// and P1's observer watches only the first land's departure.
/// Pauses on the second land, optionally round-trips the paused state through
/// serde, then P0 concedes. Returns (P1 life gained, stack depth after the
/// triggers settle, first-land hand deliveries, second-land hand deliveries).
fn concede_during_counted_return_pause(round_trip: bool) -> (i32, usize, usize, usize) {
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
    attach_departure_observer(
        &mut runner,
        observer_host,
        TargetFilter::SpecificObject { id: lands[0] },
        1,
        Some(Zone::Hand),
    );
    assert_eq!(
        runner.state().objects[&observer_host].trigger_definitions[0]
            .definition
            .valid_card,
        Some(TargetFilter::SpecificObject { id: lands[0] })
    );
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
    let paid_events = runner
        .act(GameAction::SelectCards {
            cards: lands.clone(),
        })
        .expect("select both lands")
        .events;
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
    assert_eq!(departures(&paid_events, lands[0]).len(), 1);
    assert_eq!(runner.state().objects[&lands[1]].zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().pending_replacement.as_ref().map(|p| &p.proposed),
        Some(engine::types::proposed_event::ProposedEvent::ZoneChange {
            object_id, from: Zone::Battlefield, to: Zone::Hand, ..
        }) if *object_id == lands[1]
    ));
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

    let conceded_events = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes during the replacement choice")
        .events;
    assert!(runner.state().players[0].is_eliminated);
    assert!(!runner.state().players[1].is_eliminated);
    assert!(!runner.state().players[2].is_eliminated);
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
    let action_events: Vec<_> = paid_events.into_iter().chain(conceded_events).collect();
    let hand_moves = |id| {
        departures(&action_events, id)
            .iter()
            .filter(|(to, _)| *to == Zone::Hand)
            .count()
    };
    assert_eq!(hand_moves(lands[0]), 1);
    assert_eq!(hand_moves(lands[1]), 0);
    assert!(
        departures(&action_events, lands[1])
            .iter()
            .any(|(to, _)| *to == Zone::Exile),
        "the actual cleanup event identifies the unpaid land's destination"
    );
    let recorded_hand_moves = |id| {
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|r| {
                r.object_id == id
                    && r.from_zone == Some(Zone::Battlefield)
                    && r.to_zone == Zone::Hand
            })
            .count()
    };
    let first_hand_moves = recorded_hand_moves(lands[0]);
    let second_hand_moves = recorded_hand_moves(lands[1]);
    assert_eq!(first_hand_moves, 1);
    assert_eq!(second_hand_moves, 0);
    runner.advance_until_stack_empty();
    (
        runner.life(P1) - life0,
        runner.state().stack.len(),
        first_hand_moves,
        second_hand_moves,
    )
}

#[test]
fn conceding_during_a_counted_return_pause_publishes_the_completed_prefix_once() {
    // CR 800.4a + CR 603.2: the first land's departure already happened, so a
    // surviving player's leaves-the-battlefield observer triggers for it once,
    // although the payer then leaves and the paused move is abandoned.
    for round_trip in [false, true] {
        let (gained, stack, first, second) = concede_during_counted_return_pause(round_trip);
        assert_eq!(first, 1);
        assert_eq!(second, 0);
        assert_eq!(
            gained, 1,
            "round_trip={round_trip}: P1's observer triggers exactly once"
        );
        assert_eq!(stack, 0, "round_trip={round_trip}: the stack settled");
    }
}

// ---------------------------------------------------------------------------
// Delayed prefix/current ownership through real announcement boundaries.
// ---------------------------------------------------------------------------

const MOUNT: &str = "Flying\n{T}: Target creature you control with toughness 2 or less gets +1/+1 and gains flying until end of turn. When this creature leaves the battlefield this turn, sacrifice that creature. When the creature leaves the battlefield this turn, sacrifice this creature.";
const ASHAYA: &str = "Ashaya's power and toughness are each equal to the number of lands you control.\nNontoken creatures you control are Forest lands in addition to their other types. (They're still affected by summoning sickness.)";
const MIRROR_MAGE: &str = "Flying\n{3}, Return three lands you control to their owner's hand: Return target creature to its owner's hand.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";

fn gain_life_definition(amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: TargetFilter::Controller,
        },
    )
}

/// Typed mechanism fixture: installation still happens by resolving a real
/// activated ability, rather than injecting a delayed registration or queue.
fn recurring_observer(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    trigger: TriggerDefinition,
    amount: i32,
) -> ObjectId {
    scenario
        .add_creature(player, name, 1, 1)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WheneverEvent {
                    trigger: Box::new(trigger),
                    expiry: WheneverEventExpiry::EndOfTurn,
                },
                effect: Box::new(gain_life_definition(amount)),
                uses_tracked_set: false,
            },
        ))
        .id()
}

fn departure_trigger(subject: ObjectId) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SpecificObject { id: subject })
        .origin(Zone::Battlefield)
        .trigger_zones(vec![Zone::Battlefield])
}

fn pass_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..3 {
        assert!(
            runner.state().stack.is_empty(),
            "setup priority must not resolve work"
        );
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass setup priority");
    }
    panic!("requested setup player never received priority");
}

fn round_trip_paused(runner: GameRunner, round_trip: bool) -> GameRunner {
    if round_trip {
        GameRunner::from_state(
            serde_json::from_str(
                &serde_json::to_string(runner.state()).expect("serialize entire paused game"),
            )
            .expect("deserialize entire paused game"),
        )
    } else {
        runner
    }
}

struct MountCostBoard {
    runner: GameRunner,
    mage: ObjectId,
    mount: ObjectId,
    borrowed: ObjectId,
    land: ObjectId,
    ashaya: ObjectId,
    victim: ObjectId,
    ordinary_current: ObjectId,
    ordinary_prefix: ObjectId,
    delayed_current: ObjectId,
    decoy: ObjectId,
    finishing: Option<ObjectId>,
    paid_events: Vec<GameEvent>,
    life_before: [i32; 3],
}

fn assert_mount_pause(board: &MountCostBoard) {
    let state = board.runner.state();
    assert_eq!(state.active_player, P0);
    assert_eq!(state.seat_order, vec![P0, P1, P2]);
    assert!(state.format_config.command_zone);
    assert!(state.objects[&board.ashaya].is_commander);
    assert_eq!(state.objects[&board.ashaya].owner, P0);
    assert_eq!(state.objects[&board.ashaya].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.borrowed].owner, P1);
    assert_eq!(state.objects[&board.borrowed].zone, Zone::Hand);
    assert!(state.players[1].hand.contains(&board.borrowed));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    let replacement = state
        .pending_replacement
        .as_ref()
        .expect("real commander proposal");
    assert!(replacement.is_optional);
    assert_eq!(replacement.choice_player, Some(P0));
    assert_eq!(replacement.candidates.len(), 1);
    assert!(matches!(replacement.proposed,
        ProposedEvent::ZoneChange { object_id, from: Zone::Battlefield, to: Zone::Hand, .. }
        if object_id == board.ashaya));
    let Some(PendingCostMoveResume::Cast {
        chosen,
        paused_at_index,
        deferred_cost_events,
        departure_record_indices,
        pending,
        ..
    }) = &state.pending_cost_move_resume
    else {
        panic!("actual counted Cast root");
    };
    assert_eq!(chosen, &vec![board.borrowed, board.land, board.ashaya]);
    assert_eq!(*paused_at_index, 2);
    assert_eq!(departure_record_indices.len(), 2);
    assert!(serde_json::to_value(pending.as_ref().unwrap())
        .expect("serialize actual pending activation")
        .get("activation_trigger_collection")
        .is_some_and(|value| value.is_object()));
    assert_eq!(departures(deferred_cost_events, board.borrowed).len(), 1);
    assert_eq!(departures(deferred_cost_events, board.land).len(), 1);
    assert!(departures(deferred_cost_events, board.ashaya).is_empty());
    let record = state
        .zone_changes_this_turn
        .iter()
        .find(|r| r.object_id == board.borrowed && r.from_zone == Some(Zone::Battlefield))
        .expect("paid borrowed departure");
    assert_eq!(record.controller, P0);
    assert_eq!(record.owner, P1);
    assert_eq!(
        record
            .trigger_source_context
            .as_ref()
            .unwrap()
            .lki
            .controller,
        P0
    );
    assert!(record
        .trigger_definitions
        .iter()
        .any(|t| t.definition.valid_card
            == Some(TargetFilter::SpecificObject { id: board.borrowed })));
    // CR 603.7c + CR 603.7e: the chosen creature and creating P1 authority
    // survive Act of Treason and the whole-state serialization boundary.
    let bindings: Vec<_> = state
        .delayed_triggers
        .iter()
        .filter(|d| {
            d.source_id == board.mount
                && d.controller == P1
                && matches!(
            &d.condition, DelayedTriggerCondition::WhenLeavesPlayFiltered {
                filter: TargetFilter::SpecificObject { id }
            } if *id == board.borrowed)
        })
        .collect();
    assert_eq!(
        bindings.len(),
        1,
        "the final Mount clause names the borrowed creature"
    );
    assert!(
        state
            .delayed_triggers
            .iter()
            .any(|d| d.source_id == board.mount
                && d.controller == P1
                && matches!(
                    d.condition,
                    DelayedTriggerCondition::WhenLeavesPlayFiltered {
                        filter: TargetFilter::SelfRef
                    }
                )),
        "the reverse Mount clause watches Mount itself"
    );
    assert!(state
        .delayed_triggers
        .iter()
        .any(|d| d.source_id == board.delayed_current
            && d.controller == P2
            && !d.one_shot
            && matches!(&d.condition,
        DelayedTriggerCondition::WheneverEvent { trigger, .. }
        if trigger.valid_card == Some(TargetFilter::SpecificObject { id: board.ashaya }))));
}

fn mount_cost_board(
    round_trip: bool,
    finishing_observer: bool,
    current_destination: Zone,
) -> MountCostBoard {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 9937);
    scenario.at_phase(Phase::PreCombatMain);
    let mount = scenario
        .add_creature(P1, "Phantasmal Mount", 1, 1)
        .from_oracle_text_with_keywords(&["Flying"], MOUNT)
        .id();
    let borrowed = scenario
        .add_creature(P1, "Borrowed paid creature", 1, 1)
        .id();
    let ashaya = scenario
        .add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA)
        .as_legendary()
        .commander()
        .id();
    let mage = scenario
        .add_creature(P0, "Soratami Mirror-Mage", 2, 1)
        .from_oracle_text_with_keywords(&["Flying"], MIRROR_MAGE)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let victim = scenario.add_creature(P2, "Unpaid target", 2, 2).id();
    let ordinary_current = scenario.add_creature(P1, "O1 current", 1, 1).id();
    let ordinary_prefix = scenario.add_creature(P2, "O2 prefix", 1, 1).id();
    let delayed_current = recurring_observer(
        &mut scenario,
        P2,
        "D2 current",
        departure_trigger(ashaya).destination(current_destination),
        19,
    );
    let decoy_subject = scenario
        .add_creature(P1, "Unselected decoy subject", 1, 1)
        .id();
    let decoy = recurring_observer(
        &mut scenario,
        P1,
        "Decoy delayed",
        departure_trigger(decoy_subject),
        23,
    );
    let finishing = finishing_observer.then(|| {
        recurring_observer(
            &mut scenario,
            P2,
            "Unclaimed finishing activation",
            TriggerDefinition::new(TriggerMode::AbilityActivated)
                .valid_card(TargetFilter::SpecificObject { id: mage }),
            29,
        )
    });
    let act = scenario
        .add_spell_to_hand_from_oracle(P0, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..12)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    for source in [Some(delayed_current), Some(decoy), finishing]
        .into_iter()
        .flatten()
    {
        let player = runner.state().objects[&source].controller;
        pass_to(&mut runner, player);
        runner.activate(source, 0).resolve();
    }
    pass_to(&mut runner, P1);
    runner.activate(mount, 0).target_object(borrowed).resolve();
    assert_eq!(runner.state().objects[&borrowed].controller, P1);
    pass_to(&mut runner, P0);
    let outcome = runner.cast(act).target_object(borrowed).resolve();
    outcome.assert_zone(&[act], Zone::Graveyard);
    assert_eq!(outcome.state().objects[&borrowed].controller, P0);
    assert_eq!(outcome.state().objects[&borrowed].owner, P1);
    attach_departure_observer(
        &mut runner,
        borrowed,
        TargetFilter::SpecificObject { id: borrowed },
        11,
        Some(Zone::Hand),
    );
    attach_departure_observer(
        &mut runner,
        ordinary_current,
        TargetFilter::SpecificObject { id: ashaya },
        13,
        Some(current_destination),
    );
    attach_departure_observer(
        &mut runner,
        ordinary_prefix,
        TargetFilter::SpecificObject { id: borrowed },
        17,
        Some(Zone::Hand),
    );
    let life_before = [runner.life(P0), runner.life(P1), runner.life(P2)];
    pass_to(&mut runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mage,
            ability_index: 0,
        })
        .expect("announce printed Mirror-Mage");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .expect("choose announced victim by intent");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            count: 3,
            min_count: 3,
            ..
        }
    ));
    let paid_events = runner
        .act(GameAction::SelectCards {
            cards: vec![borrowed, land, ashaya],
        })
        .expect("pay two departures then reach commander choice")
        .events;
    let mut board = MountCostBoard {
        runner,
        mage,
        mount,
        borrowed,
        land,
        ashaya,
        victim,
        ordinary_current,
        ordinary_prefix,
        delayed_current,
        decoy,
        finishing,
        paid_events,
        life_before,
    };
    assert_mount_pause(&board);
    board.runner = round_trip_paused(board.runner, round_trip);
    assert_mount_pause(&board);
    board
}

/// Inspect the real returned ordering boundary before answering any prompt.
/// CR 603.3b: all controllers share this batch; life totals cannot prove order.
fn order_mount_boundary(board: &mut MountCostBoard, abandoned: bool) -> Vec<ObjectId> {
    let mut expected = if abandoned {
        vec![(board.mount, P1), (board.ordinary_prefix, P2)]
    } else {
        vec![
            (board.borrowed, P0),
            (board.mount, P1),
            (board.ordinary_current, P1),
            (board.ordinary_prefix, P2),
            (board.delayed_current, P2),
        ]
    };
    if !abandoned {
        if let Some(id) = board.finishing {
            expected.push((id, P2));
        }
    }
    let state = board.runner.state();
    let mut observed: Vec<_> = state
        .deferred_triggers
        .iter()
        .map(|c| (c.pending.source_id, c.pending.controller))
        .collect();
    if let Some(order) = &state.pending_trigger_order {
        observed.extend(
            order
                .groups
                .iter()
                .flat_map(|g| g.triggers.iter())
                .map(|c| (c.pending.source_id, c.pending.controller)),
        );
    }
    observed.extend(state.stack.iter().filter_map(|s| match s.kind {
        engine::types::game_state::StackEntryKind::TriggeredAbility { .. } => {
            Some((s.source_id, s.controller))
        }
        _ => None,
    }));
    observed.sort();
    expected.sort();
    assert_eq!(
        observed, expected,
        "exact source/controller batch before payload resolution"
    );
    for (source, controller) in &expected {
        let context = state
            .pending_trigger_order
            .iter()
            .flat_map(|o| &o.groups)
            .flat_map(|g| &g.triggers)
            .chain(state.deferred_triggers.iter())
            .find(|c| c.pending.source_id == *source && c.pending.controller == *controller);
        if let Some(context) = context {
            let subject = if *source == board.ordinary_current || *source == board.delayed_current {
                board.ashaya
            } else {
                board.borrowed
            };
            if Some(*source) != board.finishing {
                assert!(context.trigger_events.iter().any(|e|
                    matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. }
                    if *object_id == subject)));
            }
        }
    }
    assert_eq!(board.runner.life(P1), board.life_before[1]);
    assert_eq!(board.runner.life(P2), board.life_before[2]);
    assert_eq!(state.objects[&board.mount].zone, Zone::Battlefield);
    if !abandoned {
        assert_eq!(board.runner.life(P0), board.life_before[0]);
        assert_eq!(state.objects[&board.victim].zone, Zone::Battlefield);
        assert_eq!(state.stack[0].source_id, board.mage);
    }
    // Choose a deliberate source-based permutation for each controller.
    for _ in 0..3 {
        let WaitingFor::OrderTriggers { triggers, .. } = &board.runner.state().waiting_for else {
            break;
        };
        let mut order: Vec<_> = (0..triggers.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(triggers[i].source_id));
        board
            .runner
            .act(GameAction::OrderTriggers { order })
            .expect("controller orders own sources");
    }
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let actual: Vec<_> = board
        .runner
        .state()
        .stack
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility { .. }
            )
        })
        .map(|s| (s.source_id, s.controller))
        .collect();
    // Index zero chooses the bottom of this controller's stack slot.
    let mut placement = expected;
    placement.sort_by_key(|(source, controller)| (*controller, std::cmp::Reverse(*source)));
    assert_eq!(actual, placement, "AP then P1 then P2, bottom to top");
    actual.into_iter().rev().map(|(source, _)| source).collect()
}

fn resolve_mount_batch(board: &mut MountCostBoard, expected_resolution: &[ObjectId]) {
    for source in expected_resolution {
        // Mount's reverse clause can now observe Mount's sacrifice. It names
        // the already-returned creature and cannot move that new hand object.
        if board.runner.state().stack.last().unwrap().source_id != *source {
            assert_eq!(
                board.runner.state().stack.last().unwrap().source_id,
                board.mount
            );
            board.runner.resolve_top();
            assert_eq!(
                board.runner.state().objects[&board.borrowed].zone,
                Zone::Hand
            );
        }
        assert_eq!(
            board.runner.state().stack.last().unwrap().source_id,
            *source
        );
        board.runner.resolve_top();
    }
    board.runner.advance_until_stack_empty();
    assert!(board.runner.state().stack.is_empty());
}

fn mount_completion(round_trip: bool, command: bool, finishing: bool) {
    let mut board = mount_cost_board(
        round_trip,
        finishing,
        if command { Zone::Command } else { Zone::Hand },
    );
    let result = board
        .runner
        .act(GameAction::ChooseReplacement {
            index: usize::from(!command),
        })
        .expect("accept or decline real commander replacement");
    assert_eq!(
        board.runner.state().objects[&board.ashaya].zone,
        if command { Zone::Command } else { Zone::Hand }
    );
    assert_eq!(departures(&result.events, board.ashaya).len(), 1);
    for id in [board.borrowed, board.land, board.ashaya] {
        let record = board
            .runner
            .state()
            .zone_changes_this_turn
            .iter()
            .find(|r| r.object_id == id && r.from_zone == Some(Zone::Battlefield))
            .unwrap();
        let mut group = vec![board.borrowed, board.land, board.ashaya];
        group.retain(|other| *other != id);
        group.sort();
        let mut actual = record.co_departed.clone();
        actual.sort();
        assert_eq!(actual, group);
    }
    let resolution = order_mount_boundary(&mut board, false);
    resolve_mount_batch(&mut board, &resolution);
    assert_eq!(
        board.runner.state().objects[&board.mount].zone,
        Zone::Graveyard,
        "retained paid creature departure sacrifices its bound Mount once"
    );
    assert_eq!(board.runner.state().objects[&board.victim].zone, Zone::Hand);
    assert_eq!(board.runner.life(P0) - board.life_before[0], 11);
    assert_eq!(board.runner.life(P1) - board.life_before[1], 13);
    assert_eq!(
        board.runner.life(P2) - board.life_before[2],
        36 + if finishing { 29 } else { 0 }
    );
    assert_eq!(
        board
            .runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|r| r.object_id == board.mount
                && r.from_zone == Some(Zone::Battlefield)
                && r.to_zone == Zone::Graveyard)
            .count(),
        1
    );
    assert!(board
        .runner
        .state()
        .delayed_triggers
        .iter()
        .any(|d| d.source_id == board.decoy));
}

#[test]
fn counted_return_delayed_mount_survives_commander_pause_completion() {
    for round_trip in [false, true] {
        for command in [false, true] {
            mount_completion(round_trip, command, false);
        }
    }
}

#[test]
fn counted_return_current_delayed_fragment_is_collected_once() {
    for round_trip in [false, true] {
        for command in [false, true] {
            mount_completion(round_trip, command, true);
        }
        // A separate completed return leg stages an ordinary announcement
        // carrier before the observed two-land leg, without target selection.
        delayed_two_land_sibling(false, false, false, round_trip, true);
    }
}

#[test]
fn counted_return_mixed_controller_prefix_and_current_share_apnap_batch() {
    for round_trip in [false, true] {
        for command in [false, true] {
            mount_completion(round_trip, command, false);
        }
    }
}

fn mount_concession(round_trip: bool) {
    let mut board = mount_cost_board(round_trip, false, Zone::Hand);
    let result = board
        .runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("payer leaves");
    assert!(board.runner.state().players[0].is_eliminated);
    assert!(!board.runner.state().players[1].is_eliminated);
    assert!(!board.runner.state().players[2].is_eliminated);
    assert!(board.runner.state().pending_cost_move_resume.is_none());
    assert!(board.runner.state().pending_replacement.is_none());
    let all: Vec<_> = board
        .paid_events
        .iter()
        .chain(result.events.iter())
        .cloned()
        .collect();
    assert_eq!(
        departures(&all, board.borrowed)
            .iter()
            .filter(|(to, _)| *to == Zone::Hand)
            .count(),
        1
    );
    assert_eq!(
        departures(&all, board.ashaya)
            .iter()
            .filter(|(to, _)| *to == Zone::Hand)
            .count(),
        0
    );
    assert!(!result.events.iter().any(|e| matches!(
        e,
        GameEvent::LifeChanged {
            player_id: P0,
            amount: 11,
            ..
        }
    )));
    let resolution = order_mount_boundary(&mut board, true);
    resolve_mount_batch(&mut board, &resolution);
    assert_eq!(
        board.runner.state().objects[&board.mount].zone,
        Zone::Graveyard
    );
    assert_eq!(
        board.runner.state().objects[&board.victim].zone,
        Zone::Battlefield
    );
    assert_eq!(board.runner.life(P1) - board.life_before[1], 0);
    assert_eq!(board.runner.life(P2) - board.life_before[2], 17);
    assert_eq!(board.runner.life(P0), board.life_before[0]);
    assert_eq!(
        board
            .runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|r| r.object_id == board.mount
                && r.from_zone == Some(Zone::Battlefield)
                && r.to_zone == Zone::Graveyard)
            .count(),
        1
    );
    assert_eq!(
        board
            .runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|r| r.object_id == board.ashaya
                && r.from_zone == Some(Zone::Battlefield)
                && r.to_zone == Zone::Hand)
            .count(),
        0
    );
}

#[test]
fn counted_return_delayed_mount_survives_commander_pause_concession() {
    for round_trip in [false, true] {
        mount_concession(round_trip);
    }
}

#[test]
fn counted_return_concession_retires_matching_payer_observer_and_publishes_survivors() {
    // O0's actual positive match is asserted by the companion completion row.
    for round_trip in [false, true] {
        mount_concession(round_trip);
    }
}

/// Shared None-carrier and spell-announcement sibling. Both delayed observers
/// are executable mechanism definitions installed by activation/resolution.
fn delayed_two_land_sibling(
    spell: bool,
    abandon: bool,
    empty_prefix: bool,
    round_trip: bool,
    prime_carrier: bool,
) {
    let mut scenario = GameScenario::new_n_player(3, 9938);
    scenario.at_phase(Phase::PreCombatMain);
    let lands = [
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Blue),
    ];
    let priming_land = prime_carrier.then(|| scenario.add_basic_land(P0, ManaColor::Green));
    let source = if spell {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Ensnare", true, ENSNARE)
            .with_mana_cost(ensnare_mana_cost())
            .id()
    } else {
        let counted_cost = parse_oracle_cost(if prime_carrier {
            "Return two Islands you control to their owner's hand"
        } else {
            "Return two lands you control to their owner's hand"
        });
        let cost = if prime_carrier {
            AbilityCost::Composite {
                costs: vec![
                    parse_oracle_cost("Return a Forest you control to its owner's hand"),
                    counted_cost,
                ],
            }
        } else {
            counted_cost
        };
        scenario
            .add_creature(P0, "Untargeted counted cost", 1, 1)
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
            .id()
    };
    let victim = scenario
        .add_creature(P2, "Ensnare resolution witness", 2, 2)
        .id();
    let prefix = recurring_observer(
        &mut scenario,
        P1,
        "Retained sibling observer",
        departure_trigger(lands[0]).destination(Zone::Hand),
        5,
    );
    let current = recurring_observer(
        &mut scenario,
        P2,
        "Current sibling observer",
        departure_trigger(lands[1]),
        7,
    );
    let finishing = (!spell).then(|| {
        recurring_observer(
            &mut scenario,
            P2,
            "Untargeted finishing observer",
            TriggerDefinition::new(TriggerMode::AbilityActivated)
                .valid_card(TargetFilter::SpecificObject { id: source }),
            29,
        )
    });
    for name in ["First redirect", "Second redirect"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_land_to_exile(lands[usize::from(!empty_prefix)]));
    }
    let mut runner = scenario.build();
    for id in [Some(prefix), Some(current), finishing]
        .into_iter()
        .flatten()
    {
        let player = runner.state().objects[&id].controller;
        pass_to(&mut runner, player);
        runner.activate(id, 0).resolve();
    }
    assert!(runner
        .state()
        .delayed_triggers
        .iter()
        .any(|d| d.source_id == prefix && d.controller == P1));
    assert!(runner
        .state()
        .delayed_triggers
        .iter()
        .any(|d| d.source_id == current && d.controller == P2));
    pass_to(&mut runner, P0);
    let life1 = runner.life(P1);
    let life2 = runner.life(P2);
    let life0 = runner.life(P0);
    if spell {
        let card_id = runner.state().objects[&source].card_id;
        // A manual announcement is needed to stop at the actual payment pause;
        // Ensnare has no targets to assemble outside the standard intent driver.
        runner
            .act(GameAction::CastSpell {
                object_id: source,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("announce actual Ensnare");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ));
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("select counted alternate cost");
    } else {
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("announce no-target counted activation");
    }
    if let Some(priming_land) = priming_land {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                count: 1,
                min_count: 1,
                ..
            }
        ));
        let primed = runner
            .act(GameAction::SelectCards {
                cards: vec![priming_land],
            })
            .expect("complete the separate unobserved return leg");
        assert_eq!(departures(&primed.events, priming_land).len(), 1);
        assert_eq!(runner.state().objects[&priming_land].zone, Zone::Hand);
        assert!(runner.state().pending_cost_move_resume.is_none());
        assert!(matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                count: 2,
                min_count: 2,
                resume: CostResume::Spell { spell: pending },
                ..
            } if serde_json::to_value(pending)
                .expect("serialize actual primed activation")
                .get("activation_trigger_collection")
                .is_some_and(|value| value.is_object())
        ));
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::ReturnToHand,
            count: 2,
            min_count: 2,
            ..
        }
    ));
    let paid = runner
        .act(GameAction::SelectCards {
            cards: lands.to_vec(),
        })
        .expect("pay until actual replacement pause");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert_eq!(
        departures(&paid.events, lands[0]).len(),
        usize::from(!empty_prefix)
    );
    let Some(PendingCostMoveResume::Cast {
        pending: Some(pending),
        deferred_cost_events,
        ..
    }) = &runner.state().pending_cost_move_resume
    else {
        panic!("actual retained Cast root");
    };
    assert_eq!(
        deferred_cost_events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Battlefield),
                    ..
                }
            ))
            .count(),
        usize::from(!empty_prefix)
    );
    assert_eq!(pending.activation_ability_index.is_none(), spell);
    if !spell {
        assert_eq!(
            serde_json::to_value(pending)
                .expect("serialize actual paused activation")
                .get("activation_trigger_collection")
                .is_some_and(|value| value.is_object()),
            prime_carrier,
            "the earlier completed return leg supplies the no-target carrier"
        );
    }
    let unpaid = lands[usize::from(!empty_prefix)];
    assert_eq!(runner.state().objects[&unpaid].zone, Zone::Battlefield);
    assert!(
        matches!(&runner.state().pending_replacement.as_ref().unwrap().proposed,
        ProposedEvent::ZoneChange { object_id, from: Zone::Battlefield, to: Zone::Hand, .. }
        if *object_id == unpaid)
    );
    runner = round_trip_paused(runner, round_trip);
    let terminal = if abandon {
        runner
            .act(GameAction::Concede { player_id: P0 })
            .expect("abandon spell cost")
    } else {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("finish no-carrier cost")
    };
    if abandon {
        assert!(runner.state().players[0].is_eliminated);
        assert!(!runner.state().players[1].is_eliminated);
        assert!(!runner.state().players[2].is_eliminated);
        for (index, land) in lands.iter().enumerate() {
            assert_eq!(
                runner
                    .state()
                    .zone_changes_this_turn
                    .iter()
                    .filter(|r| r.object_id == *land
                        && r.from_zone == Some(Zone::Battlefield)
                        && r.to_zone == Zone::Hand)
                    .count(),
                usize::from(index == 0 && !empty_prefix)
            );
        }
        assert_eq!(
            departures(&terminal.events, unpaid)
                .iter()
                .filter(|(to, _)| *to == Zone::Hand)
                .count(),
            0
        );
        assert_eq!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .filter(|r| r.object_id == unpaid
                    && r.from_zone == Some(Zone::Battlefield)
                    && r.to_zone == Zone::Hand)
                .count(),
            0
        );
    } else {
        assert_eq!(runner.state().objects[&lands[0]].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&lands[1]].zone, Zone::Exile);
        assert_eq!(departures(&terminal.events, lands[1]).len(), 1);
    }
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
    // CR 603.2c: retained and current observations each occur once. On
    // abandonment only the paid prefix's hand delivery can trigger.
    assert_eq!(runner.life(P1) - life1, if empty_prefix { 0 } else { 5 });
    if !abandon {
        assert_eq!(runner.life(P2) - life2, 7 + if spell { 0 } else { 29 });
        if spell {
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert!(runner.state().objects[&victim].tapped);
        } else {
            assert_eq!(runner.life(P0) - life0, 1);
        }
    } else {
        // The surviving prefix observer watches hand delivery; leave-game
        // cleanup cannot stand in for that paid cost occurrence.
        assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
        assert!(!runner.state().objects[&victim].tapped);
    }
}

#[test]
fn counted_return_delayed_noncarrier_completion() {
    // Preservation controls, with no independent red-at-base claim.
    for round_trip in [false, true] {
        for spell in [false, true] {
            delayed_two_land_sibling(spell, false, false, round_trip, false);
        }
    }
}

#[test]
fn counted_return_spell_abandonment_keeps_delayed_prefix() {
    for round_trip in [false, true] {
        for empty_prefix in [false, true] {
            delayed_two_land_sibling(true, true, empty_prefix, round_trip, false);
        }
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
